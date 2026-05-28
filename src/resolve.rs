//! Symbol Resolution (Tier 1, ADR-0004).
//!
//! After every file's symbols/edges are loaded into the [`CodeGraph`], we run a
//! graph-wide pass that binds each unresolved edge (`to == None`) to a concrete
//! [`SymbolId`] by name, using fast heuristics:
//!
//! * `Calls` edges resolve only to callable symbols (functions/methods).
//! * Same-file candidates are preferred over cross-file ones.
//! * Ties are broken deterministically (lowest id) and labeled `Heuristic`.
//!
//! Tier 2 (SCIP ingestion for compiler-accurate, `Precise` edges) is future
//! work; the data model already carries [`Confidence`] so it can slot in.

use crate::domain::{CodeGraph, Confidence, Edge, EdgeKind, SymbolId, SymbolKind};
use std::collections::HashMap;

/// Resolve all unresolved edges in place and rebuild indexes.
pub fn resolve(graph: &mut CodeGraph) {
    // Snapshot the lookups we need (immutable borrow) before mutating edges.
    let name_index: HashMap<String, Vec<(SymbolId, SymbolKind, String)>> = {
        let mut m: HashMap<String, Vec<(SymbolId, SymbolKind, String)>> = HashMap::new();
        for s in graph.symbols() {
            m.entry(s.name.clone())
                .or_default()
                .push((s.id, s.kind, s.file.clone()));
        }
        m
    };
    let file_of: HashMap<SymbolId, String> =
        graph.symbols().map(|s| (s.id, s.file.clone())).collect();

    let mut resolved: Vec<Edge> = Vec::with_capacity(graph.edge_count());
    for edge in graph.edges() {
        if edge.to.is_some() {
            resolved.push(edge.clone());
            continue;
        }
        let target = resolve_one(edge, &name_index, &file_of);
        let mut e = edge.clone();
        if let Some(to) = target {
            e.to = Some(to);
            e.confidence = Confidence::Heuristic;
        }
        resolved.push(e);
    }

    graph.replace_edges(resolved);
    graph.reindex();
}

fn resolve_one(
    edge: &Edge,
    name_index: &HashMap<String, Vec<(SymbolId, SymbolKind, String)>>,
    file_of: &HashMap<SymbolId, String>,
) -> Option<SymbolId> {
    // The callee/reference name may be a path tail; match on the last segment.
    let name = edge
        .to_name
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(&edge.to_name);

    let candidates = name_index.get(name)?;
    let want_callable = edge.kind == EdgeKind::Calls;

    let from_file = file_of.get(&edge.from);

    let mut best: Option<(SymbolId, u8)> = None; // (id, score) higher score better
    for (id, kind, file) in candidates {
        if *id == edge.from {
            continue; // never self-resolve
        }
        if want_callable && !kind.is_callable() {
            continue;
        }
        let same_file = from_file.map(|f| f == file).unwrap_or(false);
        let score = if same_file { 2 } else { 1 };
        match best {
            Some((bid, bscore)) if bscore > score || (bscore == score && bid <= *id) => {}
            _ => best = Some((*id, score)),
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;

    #[test]
    fn resolves_call_within_file() {
        let src = "fn helper() {}\nfn main() {\n    helper();\n}\n";
        let f = extract(Language::Rust, "a.rs", src, 0);
        let mut g = CodeGraph::new();
        g.upsert_file(f);
        g.reindex();
        resolve(&mut g);
        let call = g
            .edges()
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == "helper")
            .unwrap();
        assert!(call.to.is_some(), "call to helper should resolve");
        assert_eq!(call.confidence, Confidence::Heuristic);
    }

    #[test]
    fn resolves_call_across_files() {
        let a = extract(Language::Rust, "a.rs", "pub fn shared() {}\n", 0);
        let b = extract(Language::Rust, "b.rs", "fn run() {\n    shared();\n}\n", 0);
        let mut g = CodeGraph::new();
        g.upsert_file(a);
        g.upsert_file(b);
        g.reindex();
        resolve(&mut g);
        let call = g
            .edges()
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == "shared")
            .unwrap();
        let target = call.to.expect("cross-file call resolves");
        assert_eq!(g.symbol(target).unwrap().file, "a.rs");
    }
}
