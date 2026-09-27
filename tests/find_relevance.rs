//! Relevance check for `find` (ADR-0021): natural-language queries against
//! codescope's own source, each with a known-correct target symbol. Reports
//! MRR / Recall@k and fails on regression.
//!
//! Queries are phrased the way an agent would ask — describing behavior, not
//! naming the function (so most do not contain the target's full name).

use codescope::domain::CodeGraph;
use codescope::search::{find, SearchIndex};
use codescope::{extract, resolve, walker};

const CASES: &[(&str, &str)] = &[
    (
        "turn a unified diff into changed line ranges",
        "parse_unified_diff",
    ),
    ("which tests should run after my change", "diff_impact"),
    ("rank symbols by importance in the graph", "pagerank"),
    ("did you mean suggestions for a misspelled name", "suggest"),
    ("detect circular imports between files", "detect_cycles"),
    ("compress a file record before writing to disk", "encode"),
    ("is the index out of date with the working tree", "check"),
    ("rebuild the index when files change on disk", "watch"),
    ("split camelCase identifiers into words", "tokenize"),
    (
        "guess the type of a variable used as a receiver",
        "infer_receiver_type",
    ),
    ("bind a call site to its definition", "resolve_one"),
    ("walk the repository respecting gitignore", "walk"),
    ("negotiate the mcp protocol version", "negotiate"),
    ("strip comment markers from documentation", "clean_doc"),
    ("edit distance between two strings", "levenshtein"),
    ("who is affected if this symbol changes", "blast_radius"),
];

fn own_graph() -> CodeGraph {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut g = CodeGraph::new();
    for f in walker::walk(&root.join("src")) {
        let src = std::fs::read_to_string(&f.abs_path).unwrap();
        g.upsert_file(extract::extract(f.language, &f.rel_path, &src, 0));
    }
    g.reindex();
    resolve::resolve(&mut g);
    g
}

#[test]
fn find_relevance_on_own_source() {
    let g = own_graph();
    let idx = SearchIndex::build(&g);
    let (mut rr_sum, mut at1, mut at5) = (0.0, 0, 0);
    let mut report = String::new();
    for (q, want) in CASES {
        let r = find(&g, &idx, q, 100_000);
        let rank = r
            .results
            .iter()
            .position(|v| v.name == *want)
            .map(|p| p + 1);
        let rr = rank.map_or(0.0, |r| 1.0 / r as f64);
        rr_sum += rr;
        at1 += usize::from(rank == Some(1));
        at5 += usize::from(rank.is_some_and(|r| r <= 5));
        report.push_str(&format!(
            "  {:>4}  {want:<22} ← {q}\n",
            rank.map_or("-".to_string(), |r| r.to_string())
        ));
    }
    let n = CASES.len() as f64;
    let mrr = rr_sum / n;
    println!(
        "find relevance on codescope/src ({} queries):\n{report}  MRR {:.3}   R@1 {:.2}   R@5 {:.2}",
        CASES.len(),
        mrr,
        at1 as f64 / n,
        at5 as f64 / n
    );
    assert!(mrr >= 0.6, "MRR regressed: {mrr:.3}\n{report}");
    assert!(at5 as f64 / n >= 0.8, "Recall@5 regressed\n{report}");
}
