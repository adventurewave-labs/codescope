//! Query application services (ADR-0009, ADR-0011).
//!
//! These are the operations agents actually call: callers, callees,
//! blast-radius, definition, references, dependency graph, structural search and
//! repo summary. Every result is **token-budgeted**: when the answer would
//! exceed the caller's budget it is truncated and `truncated` is set, rather
//! than dumping the whole graph (the output contract from the PRD).

use crate::domain::{CodeGraph, EdgeKind, Symbol, SymbolId, SymbolKind};
use crate::rank;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

/// Default token budget for a single query answer.
pub const DEFAULT_MAX_TOKENS: usize = 4000;

/// Rough token estimate (~4 chars/token, ADR-0011).
pub(crate) fn est_tokens(chars: usize) -> usize {
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
    /// Owning type/trait/class for methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
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
    /// For edge-derived hits: how many other candidates tied when the edge
    /// was heuristically bound (omitted when unambiguous).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambiguity: Option<u16>,
}

impl SymbolView {
    pub(crate) fn from_symbol(s: &Symbol) -> SymbolView {
        SymbolView {
            id: s.id.to_string(),
            name: s.name.clone(),
            kind: s.kind.name(),
            language: s.language.name(),
            signature: s.signature.clone(),
            owner: s.owner.clone(),
            file: s.file.clone(),
            line_start: s.span.line_start,
            line_end: s.span.line_end,
            depth: None,
            site_line: None,
            confidence: None,
            ambiguity: None,
        }
    }

    pub(crate) fn est(&self) -> usize {
        est_tokens(
            self.name.len()
                + self.signature.len()
                + self.file.len()
                + self.owner.as_ref().map_or(0, |o| o.len() + 10)
                + 64,
        )
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
    /// "Did you mean" candidates when the query target matched nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Accumulate views under a token budget, truncating when exceeded.
pub(crate) struct Budget {
    max_tokens: usize,
    used: usize,
    items: Vec<SymbolView>,
    truncated: bool,
}

impl Budget {
    pub(crate) fn new(max_tokens: usize) -> Self {
        Budget {
            max_tokens,
            used: 0,
            items: Vec::new(),
            truncated: false,
        }
    }

    /// Try to add a view. Returns false (and sets `truncated`) if the budget is
    /// exhausted.
    pub(crate) fn push(&mut self, v: SymbolView) -> bool {
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
            suggestions: Vec::new(),
        }
    }
}

/// Attach "did you mean" suggestions when a named target resolved to nothing.
fn with_suggestions(
    mut r: QueryResult,
    graph: &CodeGraph,
    target: &str,
    roots_empty: bool,
) -> QueryResult {
    if roots_empty {
        r.suggestions = suggest(graph, target, 5);
    }
    r
}

/// Rank known symbol names by closeness to `target` (case-insensitive exact,
/// then substring, then bounded Levenshtein distance).
pub fn suggest(graph: &CodeGraph, target: &str, limit: usize) -> Vec<String> {
    let t = last_segment(target).to_lowercase();
    if t.is_empty() {
        return Vec::new();
    }
    let max_dist = (t.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, String)> = graph
        .names()
        .filter_map(|name| {
            let n = name.to_lowercase();
            let score = if n == t {
                0
            } else if n.contains(&t) || t.contains(&n) && n.len() >= 3 {
                1 + n.len().abs_diff(t.len())
            } else {
                let d = levenshtein(&n, &t);
                if d > max_dist {
                    return None;
                }
                100 + d
            };
            Some((score, name.to_string()))
        })
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(limit).map(|(_, n)| n).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The segment before the last one: `Store` in `a::Store::open`.
fn qualifier_of(target: &str) -> Option<&str> {
    let (head, _) = target
        .rsplit_once("::")
        .or_else(|| target.rsplit_once('.'))?;
    let q = head
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(head);
    (!q.is_empty()).then_some(q)
}

fn last_segment(target: &str) -> &str {
    target
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(target)
}

/// Resolve a free-form target (symbol name or file path) to starting symbols.
pub(crate) fn resolve_targets(graph: &CodeGraph, target: &str) -> Vec<SymbolId> {
    // File path? Use every symbol defined in that file.
    let by_file = graph.by_file(target);
    if !by_file.is_empty() {
        return by_file.to_vec();
    }
    // Otherwise treat as a symbol name (match on last path segment too).
    // A bare name can match both a file's synthetic module symbol and a real
    // declaration (e.g. `parser` the module vs. `parser` the fn); prefer the
    // declarations and fall back to the module only if nothing else matches.
    let ids = graph.by_name(last_segment(target));
    // `Owner::name` / `Owner.name` / `module::name`: narrow by owner or file
    // module when the qualifier matches anything.
    if let Some(q) = qualifier_of(target) {
        let narrowed: Vec<SymbolId> = ids
            .iter()
            .copied()
            .filter(|id| {
                graph.symbol(*id).is_some_and(|s| {
                    s.owner.as_deref() == Some(q)
                        || s.file.rsplit('/').next().and_then(|f| f.split('.').next()) == Some(q)
                })
            })
            .collect();
        if !narrowed.is_empty() {
            return narrowed;
        }
    }
    let decls: Vec<SymbolId> = ids
        .iter()
        .copied()
        .filter(|id| {
            graph
                .symbol(*id)
                .is_some_and(|s| s.kind != SymbolKind::Module)
        })
        .collect();
    if decls.is_empty() {
        ids.to_vec()
    } else {
        decls
    }
}

/// `definition`: where is this symbol declared.
pub fn definition(graph: &CodeGraph, name: &str, max_tokens: usize) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let mut ids = resolve_targets(graph, name);
    let empty = ids.is_empty();
    ids.sort();
    for id in ids {
        if let Some(s) = graph.symbol(id) {
            if !budget.push(SymbolView::from_symbol(s)) {
                break;
            }
        }
    }
    with_suggestions(
        budget.finish(name.to_string(), "definition"),
        graph,
        name,
        empty,
    )
}

/// `references`: every use site that resolves to the named symbol.
pub fn references(graph: &CodeGraph, name: &str, max_tokens: usize) -> QueryResult {
    let mut budget = Budget::new(max_tokens);
    let targets: HashSet<SymbolId> = resolve_targets(graph, name).into_iter().collect();
    let empty = targets.is_empty();
    let mut hits: Vec<(SymbolId, u32, &'static str, u16)> = Vec::new();
    for t in &targets {
        for e in graph.in_edges(*t).filter(|e| e.kind != EdgeKind::Contains) {
            hits.push((e.from, e.line, e.confidence.name(), e.alternatives));
        }
    }
    hits.sort_by_key(|a| a.1);
    for (from, line, conf, alts) in hits {
        if let Some(s) = graph.symbol(from) {
            let mut v = SymbolView::from_symbol(s);
            v.site_line = Some(line);
            v.confidence = Some(conf);
            v.ambiguity = (alts > 0).then_some(alts);
            if !budget.push(v) {
                break;
            }
        }
    }
    with_suggestions(
        budget.finish(name.to_string(), "references"),
        graph,
        name,
        empty,
    )
}

/// Transitive BFS reachability over edges of `edge_kinds`, following either
/// outgoing (callees) or incoming (callers/dependents) edges. Returns each
/// reached symbol (excluding roots) with its BFS depth, in BFS order.
pub(crate) fn reach(
    graph: &CodeGraph,
    roots: &[SymbolId],
    edge_kinds: &[EdgeKind],
    incoming: bool,
    max_depth: u32,
) -> Vec<(SymbolId, u32)> {
    let mut out = Vec::new();
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
                out.push((n, depth + 1));
                queue.push_back((n, depth + 1));
            }
        }
    }
    out
}

/// Budgeted wrapper over [`reach`].
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
    for (id, depth) in reach(graph, roots, edge_kinds, incoming, max_depth) {
        if let Some(s) = graph.symbol(id) {
            let mut v = SymbolView::from_symbol(s);
            v.depth = Some(depth);
            if !budget.push(v) {
                break;
            }
        }
    }
    let r = budget.finish(query.clone(), result_kind);
    with_suggestions(r, graph, &query, roots.is_empty())
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
        &[
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::Imports,
            EdgeKind::Defines,
        ],
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
/// `calls`, `returns`, `name`, `owner` (alias `in`).
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
    Owner(String),
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
            Filter::Owner(o) => s
                .owner
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(o)),
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
            Some(("owner", v)) | Some(("in", v)) => Filter::Owner(v.to_string()),
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
    /// Call-graph resolution quality (ADR-0018).
    pub resolution: ResolutionStats,
    pub truncated: bool,
}

/// How well call sites bound to definitions.
#[derive(Debug, Clone, Serialize)]
pub struct ResolutionStats {
    pub call_sites: usize,
    /// Bound to a definition inside the repo.
    pub resolved: usize,
    /// Bound, but other candidates tied.
    pub ambiguous: usize,
    /// Share of call sites resolved unambiguously (0–1).
    pub unambiguous_rate: f32,
}

pub fn resolution_stats(graph: &CodeGraph) -> ResolutionStats {
    let (mut call_sites, mut resolved, mut ambiguous) = (0, 0, 0);
    for e in graph.edges().iter().filter(|e| e.kind == EdgeKind::Calls) {
        call_sites += 1;
        if e.to.is_some() {
            resolved += 1;
            if e.alternatives > 0 {
                ambiguous += 1;
            }
        }
    }
    ResolutionStats {
        call_sites,
        resolved,
        ambiguous,
        unambiguous_rate: if call_sites == 0 {
            1.0
        } else {
            (resolved - ambiguous) as f32 / call_sites as f32
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleSummary {
    pub file: String,
    pub symbols: usize,
}

/// `repo_summary`: a token-bounded architectural overview — the "read this
/// before you touch anything" artifact, generated rather than hand-written.
pub fn repo_summary(graph: &CodeGraph, max_tokens: usize) -> RepoSummary {
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
    languages.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut top_modules: Vec<ModuleSummary> = file_syms
        .into_iter()
        .map(|(file, symbols)| ModuleSummary { file, symbols })
        .collect();
    top_modules.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(a.file.cmp(&b.file)));
    top_modules.truncate(15);

    // Key symbols: rank by PageRank centrality (ADR-0015) — transitive
    // importance, not raw in-degree. Symbols nothing depends on are skipped.
    let pr = rank::pagerank(graph, &[]);
    let mut ranked: Vec<(SymbolId, f64)> = pr
        .into_iter()
        .filter(|(id, _)| graph.in_edges(*id).any(|e| e.kind != EdgeKind::Contains))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    // Budget the key symbol list (the largest, variable part of the summary).
    let overhead = est_tokens(languages.iter().map(|(l, _)| l.len() + 8).sum::<usize>() + 256);
    let mut budget = Budget::new(max_tokens.saturating_sub(overhead.min(max_tokens / 2)));
    for (id, _) in ranked.into_iter().take(50) {
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
        resolution: resolution_stats(graph),
        truncated,
    }
}

/// One symbol line in a [`RepoMap`].
#[derive(Debug, Clone, Serialize)]
pub struct MapSymbol {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub kind: &'static str,
    pub line: u32,
    pub signature: String,
    /// PageRank score scaled so the top symbol is 1.0.
    pub rank: f32,
}

/// A file section in a [`RepoMap`], ordered by its best symbol's rank.
#[derive(Debug, Clone, Serialize)]
pub struct MapFile {
    pub file: String,
    pub symbols: Vec<MapSymbol>,
}

/// An Aider-style ranked repository map: the most structurally important
/// signatures that fit in the token budget, grouped by file (ADR-0015).
#[derive(Debug, Clone, Serialize)]
pub struct RepoMap {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<String>,
    pub file_count: usize,
    pub symbol_count: usize,
    pub truncated: bool,
    pub files: Vec<MapFile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// `repo_map`: rank every symbol with (personalized) PageRank and emit the
/// top signatures grouped by file until `max_tokens` is spent.
///
/// `focus` entries are symbol names or file paths the agent is working on; they
/// personalize the ranking so the map shows what matters *around* them.
pub fn repo_map(graph: &CodeGraph, focus: &[String], max_tokens: usize) -> RepoMap {
    let mut focus_ids: Vec<SymbolId> = Vec::new();
    let mut suggestions = Vec::new();
    for f in focus {
        let ids = resolve_targets(graph, f);
        if ids.is_empty() {
            suggestions.extend(suggest(graph, f, 3));
        }
        focus_ids.extend(ids);
    }
    let pr = rank::pagerank(graph, &focus_ids);
    let top = pr
        .values()
        .copied()
        .fold(0.0f64, f64::max)
        .max(f64::MIN_POSITIVE);

    let mut ranked: Vec<(&Symbol, f64)> = pr
        .iter()
        .filter_map(|(id, r)| graph.symbol(*id).map(|s| (s, *r)))
        .filter(|(s, _)| !matches!(s.kind, SymbolKind::Import | SymbolKind::Field))
        // Test code is noise when orienting in a codebase.
        .filter(|(s, _)| !crate::diff::is_test(graph, s))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(a.0.file.cmp(&b.0.file))
            .then(a.0.span.line_start.cmp(&b.0.span.line_start))
    });

    let mut used = 0usize;
    let mut truncated = false;
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<MapSymbol>> = HashMap::new();
    let mut count = 0usize;
    for (s, r) in ranked {
        let sig = if s.signature.is_empty() {
            s.name.clone()
        } else {
            s.signature.clone()
        };
        let mut cost = est_tokens(sig.len() + 8);
        if !groups.contains_key(&s.file) {
            cost += est_tokens(s.file.len() + 2);
        }
        if used + cost > max_tokens && count > 0 {
            truncated = true;
            break;
        }
        used += cost;
        count += 1;
        if !groups.contains_key(&s.file) {
            order.push(s.file.clone());
        }
        groups.entry(s.file.clone()).or_default().push(MapSymbol {
            name: s.name.clone(),
            owner: s.owner.clone(),
            kind: s.kind.name(),
            line: s.span.line_start,
            signature: sig,
            rank: (r / top) as f32,
        });
    }

    let files: Vec<MapFile> = order
        .into_iter()
        .map(|file| {
            let mut symbols = groups.remove(&file).unwrap_or_default();
            symbols.sort_by_key(|m| m.line);
            MapFile { file, symbols }
        })
        .collect();

    RepoMap {
        focus: focus.to_vec(),
        file_count: files.len(),
        symbol_count: count,
        truncated,
        files,
        suggestions,
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
    fn suggestions_on_miss() {
        let g = graph_from(&[("a.rs", "fn parse_query() {}\nfn other() {}\n")]);
        let r = definition(&g, "parse_qeury", DEFAULT_MAX_TOKENS);
        assert_eq!(r.count, 0);
        assert_eq!(
            r.suggestions.first().map(String::as_str),
            Some("parse_query")
        );
        let r = callers(&g, "parse", 2, DEFAULT_MAX_TOKENS);
        assert!(r.suggestions.contains(&"parse_query".to_string()));
        // A hit never carries suggestions.
        assert!(definition(&g, "other", DEFAULT_MAX_TOKENS)
            .suggestions
            .is_empty());
    }

    #[test]
    fn repo_map_ranks_and_budgets() {
        let g = graph_from(&[
            ("core.rs", "pub fn hub() {}\nfn unused_helper() {}\n"),
            ("a.rs", "fn a1() { hub(); }\nfn a2() { hub(); }\n"),
            ("b.rs", "fn b1() { hub(); }\n"),
        ]);
        let m = repo_map(&g, &[], DEFAULT_MAX_TOKENS);
        assert_eq!(m.files[0].file, "core.rs", "hub's file ranks first");
        assert!(m.files[0]
            .symbols
            .iter()
            .any(|s| s.name == "hub" && s.rank == 1.0));
        let tiny = repo_map(&g, &[], 5);
        assert!(tiny.truncated);
        assert_eq!(tiny.symbol_count, 1);
    }

    #[test]
    fn repo_map_focus_personalizes() {
        let g = graph_from(&[
            ("x.rs", "fn x_dep() {}\nfn x() { x_dep(); }\n"),
            ("y.rs", "fn y_dep() {}\nfn y() { y_dep(); }\n"),
        ]);
        let m = repo_map(&g, &["y.rs".to_string()], DEFAULT_MAX_TOKENS);
        assert_eq!(m.files[0].file, "y.rs");
        let m = repo_map(&g, &["x".to_string()], DEFAULT_MAX_TOKENS);
        assert_eq!(m.files[0].file, "x.rs");
    }

    #[test]
    fn qualified_targets_and_owner_filter() {
        let g = graph_from(&[
            (
                "a.rs",
                "struct A;\nimpl A { fn go(&self) {} }\nfn use_a(a: A) { a.go(); }\n",
            ),
            ("b.rs", "struct B;\nimpl B { fn go(&self) {} }\n"),
        ]);
        let d = definition(&g, "B::go", DEFAULT_MAX_TOKENS);
        assert_eq!(d.count, 1);
        assert_eq!(d.results[0].owner.as_deref(), Some("B"));
        assert_eq!(definition(&g, "go", DEFAULT_MAX_TOKENS).count, 2);
        let r = structural_search(&g, "kind:method owner:a", DEFAULT_MAX_TOKENS);
        assert_eq!(r.count, 1);
        assert_eq!(r.results[0].file, "a.rs");
    }

    #[test]
    fn blast_radius_follows_trait_impls() {
        let g = graph_from(&[(
            "a.rs",
            "trait T { fn m(&self); }\nstruct S;\nimpl T for S { fn m(&self) {} }\n",
        )]);
        let br = blast_radius(&g, "T::m", DEFAULT_MAX_TOKENS);
        assert!(br.results.iter().any(|v| v.owner.as_deref() == Some("S")));
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
