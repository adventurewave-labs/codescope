//! Query application services (ADR-0009, ADR-0011).
//!
//! These are the operations agents actually call: callers, callees,
//! blast-radius, definition, references, dependency graph, structural search and
//! repo summary. Every result is **token-budgeted**: when the answer would
//! exceed the caller's budget it is truncated and `truncated` is set, rather
//! than dumping the whole graph (the output contract from the PRD).

use crate::domain::{CodeGraph, EdgeKind, Symbol, SymbolId, SymbolKind};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};

/// Default token budget for a single query answer.
pub const DEFAULT_MAX_TOKENS: usize = 4000;

/// Rough token estimate (~4 chars/token, ADR-0011).
fn est_tokens(chars: usize) -> usize {
    chars / 4 + 1
}

/// A compact, agent-friendly view of a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolView {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub language: &'static str,
    pub signature: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    /// BFS depth from the query root (call graph / blast radius).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// For reference hits: the line of the use site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_line: Option<u32>,
    /// Resolution confidence for edge-derived results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
}

impl SymbolView {
    fn from_symbol(s: &Symbol) -> SymbolView {
        SymbolView {
            id: s.id.to_string(),
            name: s.name.clone(),
            kind: s.kind.name(),
            language: s.language.name(),
            signature: s.signature.clone(),
            file: s.file.clone(),
            line_start: s.span.line_start,
            line_end: s.span.line_end,
            depth: None,
            site_line: None,
            confidence: None,
        }
    }

    fn est(&self) -> usize {
        est_tokens(self.name.len() + self.signature.len() + self.file.len() + 64)
    }
}

/// The standard result envelope for list-style queries.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub query: String,
    pub kind: &'static str,
    pub count: usize,
    pub truncated: bool,
    pub results: Vec<SymbolView>,
}

/// Accumulate views under a token budget, truncating when exceeded.
struct Budget {
    max_tokens: usize,
    used: usize,
    items: Vec<SymbolView>,
    truncated: bool,
}

impl Budget {
    fn new(max_tokens: usize) -> Self {
        Budget {
            max_tokens,
            used: 0,
            items: Vec::new(),
            truncated: false,
        }
    }

    /// Try to add a view. Returns false (and sets `truncated`) if the budget is
    /// exhausted.
    fn push(&mut self, v: SymbolView) -> bool {
        let cost = v.est();
        if self.used + cost > self.max_tokens && !self.items.is_empty() {
            self.truncated = true;
            return false;
        }
        self.used += cost;
        self.items.push(v);
        true
    }

    fn finish(self, query: String, kind: &'static str) -> QueryResult {
        QueryResult {
            query,
            kind,
            count: self.items.len(),
            truncated: self.truncated,
            results: self.items,
        }
    }
}

/// Resolve a free-form target (symbol name or file path) to starting symbols.
fn resolve_targets(graph: &CodeGraph, target: &str) -> Vec<SymbolId> {
    // File path? Use every symbol defined in that file.
    let by_file = graph.by_file(target);
    if !by_file.is_empty() {
        return by_file.to_vec();
    }
    // Otherwise treat as a symbol name (match on last path segment too).
    let name = target
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(target);
    graph.by_name(name).to_vec()
}

/// `definition`: where is this symbol declared.
pub fn definition(graph: &CodeGraph, name: &str, max_tokens: usize) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let mut ids = resolve_targets(graph, name);
    ids.sort();
    for id in ids {
        if let Some(s) = graph.symbol(id) {
            if !budget.push(SymbolView::from_symbol(s)) {
                break;
            }
        }
    }
    budget.finish(name.to_string(), "definition")
}

/// `references`: every use site that resolves to the named symbol.
pub fn references(graph: &CodeGraph, name: &str, max_tokens: usize) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let targets: HashSet<SymbolId> = resolve_targets(graph, name).into_iter().collect();
    let mut hits: Vec<(SymbolId, u32, &'static str)> = Vec::new();
    for t in &targets {
        for e in graph.in_edges(*t) {
            hits.push((e.from, e.line, e.confidence.name()));
        }
    }
    hits.sort_by(|a, b| a.1.cmp(&b.1));
    for (from, line, conf) in hits {
        if let Some(s) = graph.symbol(from) {
            let mut v = SymbolView::from_symbol(s);
            v.site_line = Some(line);
            v.confidence = Some(conf);
            if !budget.push(v) {
                break;
            }
        }
    }
    budget.finish(name.to_string(), "references")
}

/// Generic transitive BFS over edges of `edge_kind`, following either outgoing
/// (callees) or incoming (callers/dependents) edges.
#[allow(clippy::too_many_arguments)]
fn traverse(
    graph: &CodeGraph,
    roots: &[SymbolId],
    edge_kinds: &[EdgeKind],
    incoming: bool,
    max_depth: u32,
    max_tokens: usize,
    query: String,
    result_kind: &'static str,
) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let mut seen: HashSet<SymbolId> = roots.iter().copied().collect();
    let mut queue: VecDeque<(SymbolId, u32)> = roots.iter().map(|&r| (r, 0)).collect();

    while let Some((id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let neighbors: Vec<SymbolId> = if incoming {
            graph
                .in_edges(id)
                .filter(|e| edge_kinds.contains(&e.kind))
                .map(|e| e.from)
                .collect()
        } else {
            graph
                .out_edges(id)
                .filter(|e| edge_kinds.contains(&e.kind))
                .filter_map(|e| e.to)
                .collect()
        };
        for n in neighbors {
            if seen.insert(n) {
                if let Some(s) = graph.symbol(n) {
                    let mut v = SymbolView::from_symbol(s);
                    v.depth = Some(depth + 1);
                    if !budget.push(v) {
                        return budget.finish(query, result_kind);
                    }
                }
                queue.push_back((n, depth + 1));
            }
        }
    }
    budget.finish(query, result_kind)
}

/// `callers`: who (transitively) calls the named symbol.
pub fn callers(graph: &CodeGraph, name: &str, depth: u32, max_tokens: usize) -> QueryResult {
    let roots = resolve_targets(graph, name);
    traverse(
        graph,
        &roots,
        &[EdgeKind::Calls],
        true,
        depth,
        max_tokens,
        name.to_string(),
        "callers",
    )
}

/// `callees`: what the named symbol (transitively) calls.
pub fn callees(graph: &CodeGraph, name: &str, depth: u32, max_tokens: usize) -> QueryResult {
    let roots = resolve_targets(graph, name);
    traverse(
        graph,
        &roots,
        &[EdgeKind::Calls],
        false,
        depth,
        max_tokens,
        name.to_string(),
        "callees",
    )
}

/// `blast_radius`: everything downstream-affected if the target changes — the
/// transitive set of symbols that call or reference it (or anything in the file).
pub fn blast_radius(graph: &CodeGraph, target: &str, max_tokens: usize) -> QueryResult {
    let roots = resolve_targets(graph, target);
    traverse(
        graph,
        &roots,
        &[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports],
        true,
        u32::MAX,
        max_tokens,
        target.to_string(),
        "blast_radius",
    )
}

/// A file-level dependency edge.
#[derive(Debug, Clone, Serialize)]
pub struct DepEdge {
    pub from_file: String,
    pub import: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyGraph {
    pub count: usize,
    pub truncated: bool,
    pub edges: Vec<DepEdge>,
    /// Import cycles detected among files (each a list of file paths).
    pub cycles: Vec<Vec<String>>,
}

/// `dependency_graph`: module/file import edges + cycle detection.
pub fn dependency_graph(graph: &CodeGraph, max_tokens: usize) -> DependencyGraph {
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    // adjacency for cycle detection (file -> files)
    let mut adj: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    for e in graph.edges() {
        if e.kind != EdgeKind::Imports {
            continue;
        }
        let from_file = graph.symbol(e.from).map(|s| s.file.clone());
        let Some(from_file) = from_file else { continue };
        let to_file = e.to.and_then(|id| graph.symbol(id)).map(|s| s.file.clone());
        if let Some(tf) = &to_file {
            adj.entry(from_file.clone()).or_default().push(tf.clone());
        }
        let cost = est_tokens(from_file.len() + e.to_name.len() + 32);
        if used + cost > max_tokens && !edges.is_empty() {
            truncated = true;
            break;
        }
        used += cost;
        edges.push(DepEdge {
            from_file,
            import: e.to_name.clone(),
            to_file,
        });
    }

    let cycles = detect_cycles(&adj);
    DependencyGraph {
        count: edges.len(),
        truncated,
        edges,
        cycles,
    }
}

/// Simple DFS-based cycle detection over the file import graph.
fn detect_cycles(adj: &std::collections::HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    let mut cycles = Vec::new();
    let mut color: std::collections::HashMap<String, u8> = std::collections::HashMap::new(); // 0=white,1=gray,2=black
    let mut stack: Vec<String> = Vec::new();

    fn dfs(
        node: &str,
        adj: &std::collections::HashMap<String, Vec<String>>,
        color: &mut std::collections::HashMap<String, u8>,
        stack: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        color.insert(node.to_string(), 1);
        stack.push(node.to_string());
        if let Some(neis) = adj.get(node) {
            for n in neis {
                match color.get(n).copied().unwrap_or(0) {
                    0 => dfs(n, adj, color, stack, cycles),
                    1 => {
                        // back-edge: extract the cycle from the stack
                        if let Some(pos) = stack.iter().position(|x| x == n) {
                            cycles.push(stack[pos..].to_vec());
                        }
                    }
                    _ => {}
                }
            }
        }
        stack.pop();
        color.insert(node.to_string(), 2);
    }

    let mut nodes: Vec<&String> = adj.keys().collect();
    nodes.sort();
    for n in nodes {
        if color.get(n).copied().unwrap_or(0) == 0 {
            dfs(n, adj, &mut color, &mut stack, &mut cycles);
        }
    }
    cycles
}

/// `structural_search`: query by code structure, not just text.
///
/// Supports space-separated terms; `key:value` terms are filters, bare terms are
/// substring matches against name+signature. Keys: `kind`, `lang`, `file`,
/// `calls`, `returns`, `name`.
pub fn structural_search(graph: &CodeGraph, query: &str, max_tokens: usize) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let filters = parse_query(query);

    let mut matches: Vec<&Symbol> = graph
        .symbols()
        .filter(|s| filters.iter().all(|f| f.matches(s, graph)))
        .collect();
    matches.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.span.line_start.cmp(&b.span.line_start))
    });

    for s in matches {
        if !budget.push(SymbolView::from_symbol(s)) {
            break;
        }
    }
    budget.finish(query.to_string(), "structural_search")
}

enum Filter {
    Kind(String),
    Lang(String),
    File(String),
    Calls(String),
    Returns(String),
    Name(String),
    Text(String),
}

impl Filter {
    fn matches(&self, s: &Symbol, graph: &CodeGraph) -> bool {
        match self {
            Filter::Kind(k) => s.kind.name().eq_ignore_ascii_case(k),
            Filter::Lang(l) => s.language.name().eq_ignore_ascii_case(l),
            Filter::File(f) => s.file.contains(f.as_str()),
            Filter::Name(n) => s.name.to_lowercase().contains(&n.to_lowercase()),
            Filter::Returns(t) => s.signature.contains(t.as_str()),
            Filter::Text(t) => {
                let t = t.to_lowercase();
                s.name.to_lowercase().contains(&t) || s.signature.to_lowercase().contains(&t)
            }
            Filter::Calls(callee) => graph
                .out_edges(s.id)
                .any(|e| e.kind == EdgeKind::Calls && e.to_name.contains(callee.as_str())),
        }
    }
}

fn parse_query(query: &str) -> Vec<Filter> {
    query
        .split_whitespace()
        .map(|term| match term.split_once(':') {
            Some(("kind", v)) => Filter::Kind(v.to_string()),
            Some(("lang", v)) => Filter::Lang(v.to_string()),
            Some(("file", v)) => Filter::File(v.to_string()),
            Some(("calls", v)) => Filter::Calls(v.to_string()),
            Some(("returns", v)) => Filter::Returns(v.to_string()),
            Some(("name", v)) => Filter::Name(v.to_string()),
            _ => Filter::Text(term.to_string()),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoSummary {
    pub languages: Vec<(String, usize)>,
    pub file_count: usize,
    pub symbol_count: usize,
    pub edge_count: usize,
    /// Modules/files with the most symbols.
    pub top_modules: Vec<ModuleSummary>,
    /// Most-referenced symbols (likely architectural hubs).
    pub key_symbols: Vec<SymbolView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleSummary {
    pub file: String,
    pub symbols: usize,
}

/// `repo_summary`: a token-bounded architectural overview — the "read this
/// before you touch anything" artifact, generated rather than hand-written.
pub fn repo_summary(graph: &CodeGraph, max_tokens: usize) -> RepoSummary {
    use std::collections::HashMap;
    let mut lang_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut file_syms: HashMap<String, usize> = HashMap::new();
    for s in graph.symbols() {
        *lang_counts.entry(s.language.name()).or_default() += 1;
        *file_syms.entry(s.file.clone()).or_default() += 1;
    }
    let mut languages: Vec<(String, usize)> = lang_counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    languages.sort_by(|a, b| b.1.cmp(&a.1));

    let mut top_modules: Vec<ModuleSummary> = file_syms
        .into_iter()
        .map(|(file, symbols)| ModuleSummary { file, symbols })
        .collect();
    top_modules.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(a.file.cmp(&b.file)));
    top_modules.truncate(15);

    // Key symbols: rank by in-edge (reference/call) count.
    let mut ranked: Vec<(SymbolId, usize)> = graph
        .symbols()
        .filter(|s| s.kind != SymbolKind::Module)
        .map(|s| (s.id, graph.in_edges(s.id).count()))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Budget the key symbol list (the largest, variable part of the summary).
    let overhead = est_tokens(languages.iter().map(|(l, _)| l.len() + 8).sum::<usize>() + 256);
    let mut budget = Budget::new(max_tokens.saturating_sub(overhead.min(max_tokens / 2)));
    for (id, refs) in ranked.into_iter().take(50) {
        if refs == 0 {
            break;
        }
        if let Some(s) = graph.symbol(id) {
            if !budget.push(SymbolView::from_symbol(s)) {
                break;
            }
        }
    }
    let truncated = budget.truncated;
    let key_symbols = budget.items;

    RepoSummary {
        languages,
        file_count: graph.files().len(),
        symbol_count: graph.symbol_count(),
        edge_count: graph.edge_count(),
        top_modules,
        key_symbols,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;
    use crate::resolve;

    fn graph_from(files: &[(&str, &str)]) -> CodeGraph {
        let mut g = CodeGraph::new();
        for (path, src) in files {
            let lang = Language::from_path(std::path::Path::new(path)).unwrap();
            g.upsert_file(extract(lang, path, src, 0));
        }
        g.reindex();
        resolve::resolve(&mut g);
        g
    }

    #[test]
    fn callers_and_callees() {
        let g = graph_from(&[(
            "a.rs",
            "fn leaf() {}\nfn mid() { leaf(); }\nfn top() { mid(); }\n",
        )]);
        let callees = callees(&g, "top", 5, DEFAULT_MAX_TOKENS);
        let names: Vec<&str> = callees.results.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"mid"));
        assert!(names.contains(&"leaf")); // transitive

        let callers = callers(&g, "leaf", 5, DEFAULT_MAX_TOKENS);
        let names: Vec<&str> = callers.results.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"mid"));
        assert!(names.contains(&"top"));
    }

    #[test]
    fn blast_radius_includes_transitive_callers() {
        let g = graph_from(&[(
            "a.rs",
            "fn core() {}\nfn a() { core(); }\nfn b() { a(); }\n",
        )]);
        let br = blast_radius(&g, "core", DEFAULT_MAX_TOKENS);
        let names: Vec<&str> = br.results.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn structural_search_filters() {
        let g = graph_from(&[(
            "a.rs",
            "fn query_db() { db_query(); }\nfn other() {}\nfn db_query() {}\n",
        )]);
        let r = structural_search(&g, "kind:function calls:db_query", DEFAULT_MAX_TOKENS);
        let names: Vec<&str> = r.results.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"query_db"));
        assert!(!names.contains(&"other"));
    }

    #[test]
    fn summary_counts_languages() {
        let g = graph_from(&[("a.rs", "fn a() {}\n"), ("b.py", "def b():\n    pass\n")]);
        let s = repo_summary(&g, DEFAULT_MAX_TOKENS);
        assert_eq!(s.file_count, 2);
        assert!(s.languages.iter().any(|(l, _)| l == "rust"));
        assert!(s.languages.iter().any(|(l, _)| l == "python"));
    }

    #[test]
    fn budget_truncates() {
        // Many symbols, tiny budget → truncated.
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!("fn f{i}() {{}}\n"));
        }
        let g = graph_from(&[("a.rs", &src)]);
        let r = structural_search(&g, "kind:function", 50);
        assert!(r.truncated);
    }
}
