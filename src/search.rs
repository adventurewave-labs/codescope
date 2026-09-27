//! Hybrid symbol retrieval (ADR-0021): "find the code that does X" from a
//! natural-language query, with zero model/network dependencies.
//!
//! **Lexical tier — BM25F.** Each symbol is a document with weighted fields:
//! name (×4), owner (×2), doc comment (×1.5), signature (×1), file path (×0.75),
//! and the file's module-level doc (×0.3, shared context).
//! Identifiers are split into subtokens (`parseUnifiedDiff`, `parse_unified_diff`
//! → `parse unified diff`), lowercased and lightly stemmed, so prose queries
//! meet code identifiers. A small code-aware synonym table widens recall
//! (`delete`↔`remove`, `fetch`↔`get`, …) at a discount.
//!
//! **Structural prior — PageRank.** Among lexical hits, architecturally central
//! symbols are more likely what an agent wants.
//!
//! **Fusion — RRF.** Final score = Σ wᵢ / (k + rankᵢ) over the BM25 ranking and
//! the PageRank ranking (restricted to lexical hits), k = 60. Rank fusion needs
//! no score calibration between the two signals.
//!
//! Structural filters from `structural_search` (`kind:`, `lang:`, `file:`,
//! `owner:` …) can be mixed into the query.

use crate::domain::{CodeGraph, Symbol, SymbolId, SymbolKind};
use crate::query::{self, Budget, QueryResult, SymbolView};
use crate::rank;
use rayon::prelude::*;
use std::collections::HashMap;

const K_RRF: f64 = 60.0;
const W_BM25: f64 = 1.0;
const W_RANK: f64 = 0.35;
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;
/// Weight of a term reached only through a synonym.
const SYNONYM_WEIGHT: f64 = 0.5;

#[derive(Clone, Copy)]
enum Field {
    Name,
    Owner,
    Doc,
    Signature,
    Path,
    /// The enclosing file's module-level doc (context for every symbol in it).
    FileDoc,
}

impl Field {
    fn weight(self) -> f64 {
        match self {
            Field::Name => 4.0,
            Field::Owner => 2.0,
            Field::Doc => 1.5,
            Field::Signature => 1.0,
            Field::Path => 0.75,
            Field::FileDoc => 0.3,
        }
    }
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "code", "do", "does", "for", "from",
    "function", "how", "i", "in", "into", "is", "it", "me", "method", "of", "on", "or", "that",
    "the", "this", "to", "we", "what", "when", "where", "which", "who", "with", "fn", "def",
    "func", "pub", "let", "self", "mut", "const", "return", "returns", "void", "public", "private",
    "static", "str", "string", "none", "some", "option", "result", "ok", "vec",
];

/// Split identifiers and prose into normalized search terms.
///
/// `parseUnifiedDiff` → [parse, unified, diff]; `HTTPServer2` → [http, server, 2];
/// `blast_radius` → [blast, radius].
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let chars: Vec<char> = word.chars().collect();
        let mut start = 0;
        for i in 1..=chars.len() {
            let boundary = i == chars.len() || {
                let (p, c) = (chars[i - 1], chars[i]);
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() != c.is_alphabetic())
                    // `HTTPServer`: split before the last capital of a run.
                    || (i + 1 < chars.len()
                        && p.is_uppercase()
                        && c.is_uppercase()
                        && chars[i + 1].is_lowercase())
            };
            if boundary {
                let piece: String = chars[start..i].iter().collect::<String>().to_lowercase();
                start = i;
                if piece.len() < 2 && !piece.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if STOPWORDS.contains(&piece.as_str()) {
                    continue;
                }
                out.push(stem(&piece));
            }
        }
    }
    out
}

/// Tiny suffix stemmer — maps `parse`/`parses`/`parsing`/`parsed` to one
/// form (`pars`). Consistency matters more than linguistic accuracy: the same
/// function stems both documents and queries.
pub fn stem(w: &str) -> String {
    let mut base = w;
    for suf in ["ings", "ing", "ers", "ies", "ied", "ed", "er", "es", "s"] {
        if w.len() > suf.len() + 2 && w.ends_with(suf) {
            base = &w[..w.len() - suf.len()];
            if matches!(suf, "ies" | "ied") {
                return format!("{base}y");
            }
            break;
        }
    }
    let base = if base.len() > 3 {
        base.trim_end_matches('e')
    } else {
        base
    };
    base.to_string()
}

/// Code-vocabulary synonym groups (plain words; stemmed at lookup).
const SYNONYMS: &[&[&str]] = &[
    &[
        "remove", "delete", "drop", "erase", "clear", "purge", "evict",
    ],
    &["get", "fetch", "load", "read", "retrieve", "lookup"],
    &[
        "create",
        "new",
        "build",
        "make",
        "init",
        "construct",
        "spawn",
    ],
    &["update", "modify", "patch", "change", "edit", "mutate"],
    &["save", "store", "persist", "write", "put", "commit"],
    &[
        "config",
        "configuration",
        "settings",
        "options",
        "preferences",
        "cfg",
    ],
    &["error", "err", "failure", "exception", "panic", "fault"],
    &[
        "auth",
        "authentication",
        "login",
        "credentials",
        "token",
        "signin",
    ],
    &["parse", "decode", "deserialize", "tokenize", "lex"],
    &["serialize", "encode", "marshal", "dump"],
    &["search", "find", "query", "lookup", "match"],
    &["start", "run", "launch", "serve", "execute", "begin"],
    &["stop", "shutdown", "close", "terminate", "kill"],
    &["check", "validate", "verify", "assert", "ensure"],
    &["compress", "zip", "pack", "deflate"],
    &["index", "scan", "crawl", "walk"],
    &["dependency", "import", "require", "include"],
    &["resolve", "bind", "link", "binding"],
    &["distance", "levenshtein", "similarity", "fuzzy"],
    &["reindex", "rebuild", "refresh", "reload"],
    &["impact", "affected", "affect", "downstream"],
    &["cache", "memo", "memoize"],
    &["stale", "fresh", "outdated", "freshness", "staleness"],
    &["rank", "score", "centrality", "importance", "pagerank"],
    &["suggest", "suggestion", "didyoumean", "fuzzy", "typo"],
];

fn synonyms(term: &str) -> Vec<String> {
    static STEMMED: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let groups = STEMMED.get_or_init(|| {
        SYNONYMS
            .iter()
            .map(|g| g.iter().map(|w| stem(w)).collect())
            .collect()
    });
    let mut out: Vec<String> = groups
        .iter()
        .filter(|g| g.iter().any(|w| w == term))
        .flat_map(|g| g.iter().cloned())
        .filter(|w| w != term)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// An in-memory BM25F index over a graph's symbols. Build once per graph
/// generation; querying is cheap.
pub struct SearchIndex {
    ids: Vec<SymbolId>,
    /// Weighted term frequency per doc.
    docs: Vec<HashMap<String, f64>>,
    /// Weighted length per doc.
    lens: Vec<f64>,
    avg_len: f64,
    /// Document frequency per term.
    df: HashMap<String, usize>,
    /// Symbols that are test code (down-weighted unless the query is about tests).
    tests: Vec<bool>,
    /// PageRank order among all symbols (0 = most central).
    centrality_rank: HashMap<SymbolId, usize>,
}

fn searchable(s: &Symbol) -> bool {
    // Skip imports and the synthetic per-file module (the only container-less symbol).
    let synthetic_module = s.kind == SymbolKind::Module && s.container.is_none();
    s.kind != SymbolKind::Import && !synthetic_module
}

impl SearchIndex {
    pub fn build(graph: &CodeGraph) -> SearchIndex {
        let mut symbols: Vec<&Symbol> = graph.symbols().filter(|s| searchable(s)).collect();
        symbols.sort_by_key(|s| s.id);
        let file_docs: HashMap<&str, &str> = graph
            .symbols()
            .filter(|s| s.kind == SymbolKind::Module && s.container.is_none())
            .filter_map(|s| s.doc.as_deref().map(|d| (s.file.as_str(), d)))
            .collect();
        // Per-symbol term frequencies are independent — build in parallel.
        let docs: Vec<HashMap<String, f64>> = symbols
            .par_iter()
            .map(|s| {
                let mut tf: HashMap<String, f64> = HashMap::new();
                let mut add = |text: &str, field: Field| {
                    for t in tokenize(text) {
                        *tf.entry(t).or_default() += field.weight();
                    }
                };
                add(&s.name, Field::Name);
                if let Some(o) = &s.owner {
                    add(o, Field::Owner);
                }
                if let Some(d) = &s.doc {
                    add(d, Field::Doc);
                }
                // The signature repeats the name; count only what it adds.
                let sig_rest = s.signature.replacen(&s.name, " ", 1);
                add(&sig_rest, Field::Signature);
                add(&s.file, Field::Path);
                if let Some(fd) = file_docs.get(s.file.as_str()) {
                    add(fd, Field::FileDoc);
                }
                tf
            })
            .collect();
        let lens: Vec<f64> = docs.iter().map(|tf| tf.values().sum()).collect();
        let mut df: HashMap<String, usize> = HashMap::new();
        for tf in &docs {
            for t in tf.keys() {
                *df.entry(t.clone()).or_default() += 1;
            }
        }
        let avg_len = if lens.is_empty() {
            1.0
        } else {
            lens.iter().sum::<f64>() / lens.len() as f64
        };
        let mut pr: Vec<(SymbolId, f64)> = rank::pagerank(graph, &[]).into_iter().collect();
        pr.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let centrality_rank = pr
            .into_iter()
            .enumerate()
            .map(|(i, (id, _))| (id, i))
            .collect();
        SearchIndex {
            tests: symbols
                .iter()
                .map(|s| crate::diff::is_test(graph, s))
                .collect(),
            ids: symbols.iter().map(|s| s.id).collect(),
            docs,
            lens,
            avg_len,
            df,
            centrality_rank,
        }
    }

    fn idf(&self, term: &str) -> f64 {
        let n = self.ids.len() as f64;
        let df = *self.df.get(term).unwrap_or(&0) as f64;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Ranked `(symbol, fused score, bm25, matched terms)` for free text.
    pub fn search(&self, text: &str) -> Vec<Hit> {
        // Query terms with weights (originals 1.0, synonyms discounted).
        let mut qterms: HashMap<String, f64> = HashMap::new();
        for t in tokenize(text) {
            for s in synonyms(&t) {
                let e = qterms.entry(s).or_insert(0.0);
                *e = e.max(SYNONYM_WEIGHT);
            }
            qterms.insert(t, 1.0);
        }
        if qterms.is_empty() {
            return Vec::new();
        }
        let original: Vec<&String> = qterms
            .iter()
            .filter(|(_, w)| **w >= 1.0)
            .map(|(t, _)| t)
            .collect();
        // Test code is usually noise for "find the code that does X" — unless
        // the query is literally about tests.
        let about_tests = original
            .iter()
            .any(|t| t.as_str() == "test" || t.as_str() == "spec");

        let mut scored: Vec<(usize, f64, Vec<String>)> = Vec::new();
        for (i, tf) in self.docs.iter().enumerate() {
            let mut score = 0.0;
            let mut matched = Vec::new();
            for (t, qw) in &qterms {
                let Some(&f) = tf.get(t) else { continue };
                let norm = BM25_K1 * (1.0 - BM25_B + BM25_B * self.lens[i] / self.avg_len);
                score += qw * self.idf(t) * (f * (BM25_K1 + 1.0)) / (f + norm);
                matched.push(t.clone());
            }
            if score > 0.0 {
                // Coverage bonus: prefer docs matching more *distinct* original
                // query terms (BM25 alone lets one rare term dominate).
                let covered = original
                    .iter()
                    .filter(|t| tf.contains_key(t.as_str()))
                    .count();
                score *= 1.0 + 0.5 * covered as f64 / original.len().max(1) as f64;
                if self.tests[i] && !about_tests {
                    score *= 0.4;
                }
                matched.sort();
                scored.push((i, score, matched));
            }
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(self.ids[a.0].cmp(&self.ids[b.0])));

        // Centrality ranking restricted to the lexical hits.
        let mut by_centrality: Vec<usize> = (0..scored.len()).collect();
        by_centrality.sort_by_key(|&j| {
            self.centrality_rank
                .get(&self.ids[scored[j].0])
                .copied()
                .unwrap_or(usize::MAX)
        });
        let mut cent_pos = vec![0usize; scored.len()];
        for (pos, j) in by_centrality.into_iter().enumerate() {
            cent_pos[j] = pos;
        }

        let mut hits: Vec<Hit> = scored
            .into_iter()
            .enumerate()
            .map(|(lex_pos, (i, bm25, matched))| Hit {
                id: self.ids[i],
                score: W_BM25 / (K_RRF + lex_pos as f64 + 1.0)
                    + W_RANK / (K_RRF + cent_pos[lex_pos] as f64 + 1.0),
                bm25,
                matched,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
        hits
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// One retrieval hit.
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: SymbolId,
    pub score: f64,
    pub bm25: f64,
    pub matched: Vec<String>,
}

/// `find`: natural-language (+ optional structural filters) symbol search.
pub fn find(
    graph: &CodeGraph,
    index: &SearchIndex,
    query_text: &str,
    max_tokens: usize,
) -> QueryResult {
    let (filters, words) = query::split_filters(query_text);
    let mut budget = Budget::new(max_tokens);
    let hits = index.search(&words.join(" "));
    let top = hits.first().map(|h| h.score).unwrap_or(1.0);
    for h in hits {
        let Some(s) = graph.symbol(h.id) else {
            continue;
        };
        if !filters.iter().all(|f| f.matches(s, graph)) {
            continue;
        }
        let mut v = SymbolView::from_symbol(s);
        v.score = Some(((h.score / top) * 1000.0).round() as f32 / 1000.0);
        v.matched = Some(h.matched);
        v.doc = s.doc.clone();
        if !budget.push(v) {
            break;
        }
    }
    budget.finish(query_text.to_string(), "find")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;
    use crate::resolve;

    #[test]
    fn tokenizes_identifiers_and_prose() {
        assert_eq!(tokenize("parseUnifiedDiff"), vec!["pars", "unify", "diff"]);
        assert_eq!(
            tokenize("parse_unified_diff"),
            vec!["pars", "unify", "diff"]
        );
        assert_eq!(tokenize("HTTPServer2"), vec!["http", "serv", "2"]);
        assert_eq!(tokenize("How do we parse the diffs?"), vec!["pars", "diff"]);
    }

    fn graph(files: &[(&str, &str)]) -> CodeGraph {
        let mut g = CodeGraph::new();
        for (p, src) in files {
            let lang = Language::from_path(std::path::Path::new(p)).unwrap();
            g.upsert_file(extract(lang, p, src, 0));
        }
        g.reindex();
        resolve::resolve(&mut g);
        g
    }

    fn top(g: &CodeGraph, q: &str) -> Vec<String> {
        let idx = SearchIndex::build(g);
        find(g, &idx, q, 4000)
            .results
            .into_iter()
            .map(|v| v.name)
            .collect()
    }

    #[test]
    fn finds_by_subtokens_docs_and_synonyms() {
        let g = graph(&[
            (
                "src/users.rs",
                "/// Permanently erase an account and its sessions.\npub fn purge_account() {}\npub fn fetch_user_profile() {}\npub fn render() {}\n",
            ),
            ("src/cfg.rs", "pub fn load_settings() {}\n"),
        ]);
        assert_eq!(
            top(&g, "get user profile")[0],
            "fetch_user_profile",
            "synonym get→fetch"
        );
        assert_eq!(
            top(&g, "delete account")[0],
            "purge_account",
            "synonym + name"
        );
        assert_eq!(
            top(&g, "erase sessions")[0],
            "purge_account",
            "doc comment match"
        );
        assert_eq!(top(&g, "read configuration")[0], "load_settings");
        assert!(top(&g, "zebra").is_empty());
    }

    #[test]
    fn filters_mix_with_text() {
        let g = graph(&[(
            "a.rs",
            "struct Parser;\nimpl Parser { fn parse(&self) {} }\nfn parse_all() {}\n",
        )]);
        let names = top(&g, "parse kind:method");
        assert_eq!(names, vec!["parse"]);
    }

    #[test]
    fn centrality_breaks_lexical_ties() {
        // Two equally-named helpers; the one everyone calls should rank first.
        let g = graph(&[
            ("legacy.rs", "pub fn format_value() {}\n"),
            (
                "core.rs",
                "pub fn format_value() {}\nfn x() { format_value(); }\nfn y() { format_value(); }\nfn z() { format_value(); }\n",
            ),
        ]);
        let idx = SearchIndex::build(&g);
        let r = find(&g, &idx, "format value", 4000);
        assert_eq!(r.results[0].file, "core.rs");
    }
}
