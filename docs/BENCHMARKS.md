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

- **Dense corpus** (`gen_repo.py`): N files × M functions, every function 4
  lines with one call — maximizes symbol/edge density per byte (a storage worst
  case).
- **Realistic corpus** (`gen_realistic.py`): functions with doc comments, input
  validation, loops and ~9 call sites each (~18 LOC/symbol) — closer to real
  Rust.
- **Query/throughput**: `cargo bench` (criterion) — `benches/index_bench.rs`,
  `benches/query_bench.rs`.
- **Correctness**: 24 unit/integration tests (`cargo test`), plus dogfooding —
  `codescope` indexes its own source and answers callers/callees/blast-radius
  correctly.
