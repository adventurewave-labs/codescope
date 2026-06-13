//! Indexing throughput benchmarks (ADR-0014). Generates a synthetic repo, then
//! measures cold-index time. Run with `cargo bench --bench index_bench`.

use codescope::{index, store::Store};
use criterion::{criterion_group, criterion_main, Criterion};
use std::fmt::Write as _;

/// Write a synthetic Rust repo of roughly `files` files into `dir`.
fn synth_repo(dir: &std::path::Path, files: usize, fns_per_file: usize) {
    for fi in 0..files {
        let mut src = String::new();
        for j in 0..fns_per_file {
            let _ = writeln!(
                src,
                "pub fn f{fi}_{j}() {{\n    f{fi}_{prev}();\n}}",
                prev = j.saturating_sub(1)
            );
        }
        std::fs::write(dir.join(format!("m{fi}.rs")), src).unwrap();
    }
}

fn bench_cold_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_index");
    group.sample_size(10);
    // ~200 files * 50 fns ≈ 10k symbols.
    group.bench_function("200_files_50_fns", |b| {
        b.iter_batched(
            || {
                let dir = tempfile::tempdir().unwrap();
                synth_repo(dir.path(), 200, 50);
                dir
            },
            |dir| {
                let mut store = Store::open(&dir.path().join(".codescope/idx.redb")).unwrap();
                index::build_index(dir.path(), &mut store).unwrap();
            },
            criterion::BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_cold_index);
criterion_main!(benches);
