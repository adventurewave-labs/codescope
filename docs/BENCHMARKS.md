# codescope — Benchmarks & Validation

Measured on **2026-05-28**, release build (`opt-level=3`, thin LTO, `codegen-units=1`),
4-core container, 15 GB RAM, `rustc 1.94`. Reproduce with `cargo bench` plus the
synthetic-repo scripts described under *Methodology*.

## Results vs. PRD success metrics

| Metric (PRD §8) | Target | Measured | Status |
|---|---|---|---|
| Cold index, 100k LOC | < 10 s | **0.20 s** (500 files, 25.5k symbols, 50k edges) | ✅ ~50× margin |
| Cold index, ~1M LOC | < 2 min | **1.6 s** (1,400 files, 909k LOC, 54.6k symbols, 480k edges) | ✅ ~75× margin |
| Incremental re-index (1 file) | < 200 ms | **55–58 ms** (over a 1,400-file index) | ✅ |
| Median query latency | < 50 ms | **< 2 ms** for every query type (in-process, 25.5k-symbol graph) | ✅ |
| Index size on disk | < 15% of source | **~75%** on dense-call ~1M LOC code | ❌ see *Storage footprint* |

### Query latency (criterion, 25.5k-symbol graph)

| Query | Time |
|---|---|
| `callers` (depth 3) | 0.84 µs |
| `callees` (depth 3) | 0.94 µs |
| `blast_radius` (unbounded) | 13.4 µs |
| `structural_search` (full scan) | 1.39 ms |
| `repo_summary` (full scan + ranking) | 1.69 ms |

Transitive graph queries are sub-microsecond because the adjacency and
reverse-edge indexes (ADR-0006) make neighbor lookup O(1); the two full-scan
queries are linear in symbol count and still finish in low-millisecond time.

> **Note on CLI vs. server latency.** The figures above are the in-process query
> cost. The `codescope <verb>` CLI additionally loads and resolves the whole
> graph from disk on each invocation (~0.85 s for the 1M-LOC index). The MCP
> server (`codescope serve --mcp`) loads the graph **once** and caches it, so
> agent queries pay only the in-process cost above.

### Index throughput (criterion)

| Benchmark | Time |
|---|---|
| `cold_index` — 200 files × 50 fns (~40k LOC, incl. compaction) | 75 ms |

## Storage footprint — the one unmet target

The PRD listed *"`redb` vs `rusqlite` — benchmark both on the 1M-LOC target
before committing"* as an open question (§12). Benchmarking answers it:

- The **logical** index data (LZ4-compressed per-file bincode blobs) for the
  ~1M-LOC corpus is **~4 MB** (~17% of the 23 MB source).
- The **redb file on disk** is **~17 MB** (~75% of source) after compaction.

The gap is redb's page/MVCC allocation model: its file footprint is ~4× the
logical data at this scale and is *insensitive* to per-record compression
(verified — shrinking the `Edge` record from a 16-byte span to a 4-byte line
changed the logical data but not the redb file size).

**Optimizations already applied** (each a real improvement):
1. LZ4 block compression of every per-file record (pure-Rust `lz4_flex`).
2. Looped `redb` compaction after bulk indexing (8.9 MB → 4.7 MB on the dense
   corpus).
3. A dedicated content-hash table so incremental change detection never
   decompresses a blob.
4. `Edge` stores only a use-site line, not a full byte span — shrinks the
   in-memory graph (edges dominate by volume) even though redb's file is
   unaffected.

**Why we keep redb anyway:** it delivers the incremental (<200 ms) and
concurrency story the architecture is built on (ADR-0005, ADR-0008), and the
size overhead is constant-factor, not algorithmic. The dense synthetic corpus
(~9 call edges per tiny function) is also a worst case; real code with lower
call density lands lower.

**Remediation path** (tracked for a future revision, not v1): a single
LZ4-compressed snapshot store would hit ~17% on this corpus and under 15% on
typical code, at the cost of rewriting the whole snapshot on each incremental
update. The `GraphStore` boundary (ADR-0005 / DDD `services-and-repositories`)
keeps this swappable without touching the query or extraction layers.

## Methodology

- **Dense corpus** (`scripts/gen_repo.py`): N files × M functions, every
  function 4 lines with one call — maximizes symbol/edge density per byte (a
  storage worst case).
- **Realistic corpus** (`scripts/gen_realistic.py`): functions with doc
  comments, input validation, loops and ~9 call sites each (21 lines per
  function, ~21 LOC/symbol) — closer to real Rust.
- **Query/throughput**: `cargo bench` (criterion) — `benches/index_bench.rs`,
  `benches/query_bench.rs`.
- **Correctness**: unit/integration tests (`cargo test`), plus dogfooding —
  `codescope` indexes its own source and answers callers/callees/blast-radius
  correctly.

## A second data point: 2-core Codespace (2026-10-14)

The headline numbers above are 4-core-scoped; hardware matters. Reproduced on
a 2-core / 8 GB shared-vCPU Codespace (release build, rustc 1.99):

| Workload | 4-core (above) | 2-core Codespace |
|---|---|---|
| Cold index, dense 125k LOC (501 files, 26k symbols, 50k edges) | ~0.2 s @ 100k | 0.86 s |
| Cold index, realistic ~1.15M LOC (1,401 files, 57k symbols, **1.2M edges**) | ~1.6 s @ 909k LOC / 480k edges | 16.7 s |
| 1-file incremental patch (1,402-file index) | 55–58 ms | 113 ms |
| `repo_summary` in-process (criterion) | 1.69 ms | 6.8 ms |
| `structural_search` in-process (criterion) | 1.39 ms | 1.20 ms |
| Index size, realistic ~1M corpus | ~75% of source | 67% (34.2 MB / 51.1 MB) |

The 1M-row corpus here is denser than the 4-core run's (1.2M vs 480k edges),
so it stresses resolution harder; PRD targets (<10 s @ 100k, <2 min @ 1M)
still clear with 7–12× margin. CLI query walls additionally pay a graph load
per invocation (~1.6 s on the 1M index there) — the MCP server caches the
graph and pays only the in-process cost.

## Freshness & reload (ADR-0020)

Release build, 4-core container, `cargo run --release --example fresh_latency -- <repo>`:

| Repo | Files | Edges | Full load (before) | Full load | Stat check | 1-file refresh |
|---|---|---|---|---|---|---|
| codescope | 21 | 2.8k | 9 ms | 7 ms | 3 ms | 19 ms |
| gson (Java) | 264 | 30k | 207 ms | 34 ms | 4 ms | 36 ms |
| fmt (C++) | 79 | 25k | 149 ms | 30 ms | 3 ms | 80 ms |
| dotnet/samples (C#) | 3,166 | 129k | 674 ms | 318 ms | 46 ms | 213 ms |

"Before" = pre-ADR-0020 resolver (per-candidate import re-splitting, edge cloning).

## `find` relevance (ADR-0021)

`cargo test --test find_relevance -- --nocapture` — 16 behavior-phrased queries over codescope's own source:
**MRR 0.730 · R@1 0.62 · R@5 0.88** (after ADR-0023 import-edge resolution bound previously-floating import edges; was 0.693 / 0.56 / 0.88; baseline BM25F+RRF before file-docs/test-downweight: 0.644 / 0.56 / 0.75).
CLI `find` end-to-end: 115 ms (gson, 4.5k symbols), 690 ms (dotnet/samples, 25k symbols).

## Call-resolution accuracy (ADR-0022)

`scripts/eval` — 85 annotated call sites across 10 languages (`tests/eval/`):

| Precision | Recall | F1 | Accuracy |
|---|---|---|---|
| 1.00 | 0.97 | 0.98 | 0.98 |

Before the ADR-0022 fixes (class instantiation, field-receiver inference, `dyn Trait`): 1.00 / 0.78.
Per-language table and the remaining misses: [`EVAL.md`](EVAL.md).
