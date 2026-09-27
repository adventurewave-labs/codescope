//! Graph centrality (ADR-0015): personalized PageRank over the symbol graph.
//!
//! Raw in-degree over-weights utility functions that are called a lot but say
//! little about architecture. PageRank propagates importance transitively — a
//! symbol matters if *important* symbols depend on it — which is the same
//! signal Aider's repo-map uses to pick what an agent should see first.
//!
//! **Personalization.** When the caller supplies focus symbols/files (what the
//! agent is currently editing), the teleport distribution is concentrated on
//! them, so the ranking answers "what matters *around this*" rather than "what
//! matters globally".

use crate::domain::{CodeGraph, EdgeKind, SymbolId, SymbolKind};
use std::collections::HashMap;

/// Damping factor (probability of following an edge vs. teleporting).
const DAMPING: f64 = 0.85;
/// Power-iteration cap; convergence is usually reached well before this.
const MAX_ITERS: usize = 50;
/// L1 convergence tolerance.
const TOLERANCE: f64 = 1e-9;

/// Edge kinds that carry "depends on" semantics for ranking.
fn ranks_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports | EdgeKind::Defines
    )
}

/// Compute (personalized) PageRank for every non-module symbol.
///
/// `focus` is a set of symbol ids to bias the teleport vector toward; an empty
/// slice yields classic uniform PageRank. Scores sum to ~1.0.
pub fn pagerank(graph: &CodeGraph, focus: &[SymbolId]) -> HashMap<SymbolId, f64> {
    // Dense index over rankable nodes.
    let nodes: Vec<SymbolId> = {
        let mut v: Vec<SymbolId> = graph
            .symbols()
            .filter(|s| s.kind != SymbolKind::Module)
            .map(|s| s.id)
            .collect();
        v.sort();
        v
    };
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }
    let index: HashMap<SymbolId, usize> =
        nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();

    // Edges are attributed to the enclosing symbol; top-level (module-sourced)
    // uses are skipped as sources, but their targets still receive teleport mass.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in graph.edges() {
        if !ranks_edge(e.kind) {
            continue;
        }
        let (Some(&from), Some(to)) = (index.get(&e.from), e.to) else {
            continue;
        };
        let Some(&to) = index.get(&to) else { continue };
        if from != to {
            out[from].push(to);
        }
    }

    // Teleport distribution.
    let mut teleport = vec![0.0f64; n];
    let focus_idx: Vec<usize> = focus
        .iter()
        .filter_map(|id| index.get(id).copied())
        .collect();
    if focus_idx.is_empty() {
        teleport.iter_mut().for_each(|t| *t = 1.0 / n as f64);
    } else {
        // Mostly focus, with a small uniform floor so disconnected parts of the
        // graph still get a stable (tiny) score.
        let floor = 0.1 / n as f64;
        teleport.iter_mut().for_each(|t| *t = floor);
        let share = 0.9 / focus_idx.len() as f64;
        for i in focus_idx {
            teleport[i] += share;
        }
    }

    let mut rank = teleport.clone();
    let mut next = vec![0.0f64; n];
    for _ in 0..MAX_ITERS {
        let mut dangling = 0.0;
        next.iter_mut().for_each(|x| *x = 0.0);
        for (i, outs) in out.iter().enumerate() {
            if outs.is_empty() {
                dangling += rank[i];
            } else {
                let share = rank[i] / outs.len() as f64;
                for &j in outs {
                    next[j] += share;
                }
            }
        }
        let mut delta = 0.0;
        for i in 0..n {
            let v = (1.0 - DAMPING) * teleport[i] + DAMPING * (next[i] + dangling * teleport[i]);
            delta += (v - rank[i]).abs();
            rank[i] = v;
        }
        if delta < TOLERANCE {
            break;
        }
    }

    nodes.into_iter().zip(rank).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;
    use crate::resolve;

    fn graph(src: &str) -> CodeGraph {
        let mut g = CodeGraph::new();
        g.upsert_file(extract(Language::Rust, "lib.rs", src, 0));
        g.reindex();
        resolve::resolve(&mut g);
        g
    }

    fn score(g: &CodeGraph, r: &HashMap<SymbolId, f64>, name: &str) -> f64 {
        r[&g.by_name(name)[0]]
    }

    #[test]
    fn hub_outranks_leaves() {
        let g = graph(
            "fn core() {}\nfn a() { core(); }\nfn b() { core(); }\nfn c() { core(); }\nfn lonely() {}\n",
        );
        let r = pagerank(&g, &[]);
        assert!(score(&g, &r, "core") > score(&g, &r, "lonely"));
        assert!(score(&g, &r, "core") > score(&g, &r, "a"));
        let total: f64 = r.values().sum();
        assert!((total - 1.0).abs() < 1e-6, "scores sum to 1, got {total}");
    }

    #[test]
    fn transitive_importance() {
        // deep is only called by core, but core is a hub → deep inherits rank.
        let g = graph(
            "fn deep() {}\nfn core() { deep(); }\nfn a() { core(); }\nfn b() { core(); }\nfn leaf() {}\nfn x() { leaf(); }\n",
        );
        let r = pagerank(&g, &[]);
        assert!(score(&g, &r, "deep") > score(&g, &r, "leaf"));
    }

    #[test]
    fn personalization_shifts_mass() {
        let g = graph("fn p() {}\nfn q() {}\nfn a() { p(); }\nfn b() { q(); }\n");
        let focus_a = pagerank(&g, &[g.by_name("a")[0]]);
        assert!(score(&g, &focus_a, "p") > score(&g, &focus_a, "q"));
        let focus_b = pagerank(&g, &[g.by_name("b")[0]]);
        assert!(score(&g, &focus_b, "q") > score(&g, &focus_b, "p"));
    }
}
