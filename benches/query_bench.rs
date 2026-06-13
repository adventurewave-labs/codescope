//! Query latency benchmarks (ADR-0014). Builds an in-memory graph once, then
//! measures per-query latency. Run with `cargo bench --bench query_bench`.

use codescope::domain::{CodeGraph, Language};
use codescope::{extract, query, resolve};
use criterion::{criterion_group, criterion_main, Criterion};
use std::fmt::Write as _;

fn build_graph(files: usize, fns_per_file: usize) -> CodeGraph {
    let mut g = CodeGraph::new();
    for fi in 0..files {
        let mut src = String::new();
        for j in 0..fns_per_file {
            let _ = writeln!(
                src,
                "pub fn f{fi}_{j}() {{\n    f{fi}_{prev}();\n}}",
                prev = j.saturating_sub(1)
            );
        }
        let path = format!("m{fi}.rs");
        g.upsert_file(extract::extract(Language::Rust, &path, &src, 0));
    }
    g.reindex();
    resolve::resolve(&mut g);
    g
}

fn bench_queries(c: &mut Criterion) {
    let graph = build_graph(200, 50);
    let mut group = c.benchmark_group("query");

    group.bench_function("callers_depth3", |b| {
        b.iter(|| query::callers(&graph, "f0_10", 3, query::DEFAULT_MAX_TOKENS));
    });
    group.bench_function("callees_depth3", |b| {
        b.iter(|| query::callees(&graph, "f0_10", 3, query::DEFAULT_MAX_TOKENS));
    });
    group.bench_function("blast_radius", |b| {
        b.iter(|| query::blast_radius(&graph, "f0_10", query::DEFAULT_MAX_TOKENS));
    });
    group.bench_function("structural_search", |b| {
        b.iter(|| {
            query::structural_search(
                &graph,
                "kind:function calls:f0_1",
                query::DEFAULT_MAX_TOKENS,
            )
        });
    });
    group.bench_function("repo_summary", |b| {
        b.iter(|| query::repo_summary(&graph, query::DEFAULT_MAX_TOKENS));
    });
    group.finish();
}

criterion_group!(benches, bench_queries);
criterion_main!(benches);
