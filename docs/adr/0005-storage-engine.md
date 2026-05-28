# ADR-0005: Storage Engine

**Status:** Accepted

**Date:** 2026-05-28

## Context

The extracted symbols and edges must be persisted on disk so an index survives between invocations and so a query can run without re-parsing the repo. The store must be embedded (no external DB server — see ADR-0001), single-file, fast to open and memory-map, and structured for **incremental re-index**: when one file changes, only that file's records should be rewritten so a single-file re-index stays under the 200 ms target. The PRD also caps index size at < ~15% of source size on disk.

The PRD left this as an explicit open question: **`redb` vs `rusqlite`**, to be benchmarked on the 1M-LOC target before committing.

## Decision

Use **`redb`** — a pure-Rust, embedded, single-file, memory-mapped key-value store — as the persistence engine.

Records are **keyed per file path**: a file's symbols and outgoing edges are stored under keys namespaced by that path. On incremental re-index, `codescope` deletes the changed file's records and writes the freshly extracted ones in a single transaction, leaving every other file's records untouched. The in-memory `CodeGraph` (ADR-0006) is loaded from this store at startup and patched on incremental updates.

**Resolution of the open question:** `redb` is chosen over `rusqlite` primarily because it is pure Rust with no C dependency, which keeps the static-binary build and cross-compilation simple (consistent with ADR-0001) and avoids bundling/linking SQLite's C library across every release target. The relational query power of SQLite is not needed because graph traversal happens in memory (ADR-0006); the store only needs fast keyed get/put/range scans.

## Consequences

**Positive**
- Pure-Rust, no C dependency — simplest possible static binary and cross-compile story.
- Memory-mapped reads give fast cold-open of the index, supporting the query-latency target.
- Per-file keying makes incremental re-index a localized delete+insert, directly serving the < 200 ms single-file goal.
- Single-file on-disk index is trivial to ship, cache, gitignore, or key to a commit hash later.

**Negative / tradeoffs**
- `redb` is younger and less battle-tested than SQLite, with a smaller tool/ecosystem (no external inspection via a `sqlite3` CLI) and an evolving on-disk format that may require migrations.
- No ad-hoc SQL; any relational-style querying must be implemented in Rust against key ranges.
- Compactness/size-on-disk (the < 15% target) depends on our own value encoding rather than a mature engine's storage layer.

## Alternatives Considered

- **`rusqlite` (SQLite):** extremely mature, rich SQL, great tooling, but pulls in a C dependency that complicates the pure-Rust static binary; relational features are largely unused since traversal is in-memory. Rejected after weighing against the open question, but a credible fallback if `redb` proves limiting.
- **External graph DB (Neo4j, etc.):** natural graph fit but reintroduces the server/dependency footprint we explicitly differentiate against. Rejected.
- **Custom flat files / append-only log:** maximally simple but reinvents transactions, crash safety, and indexing. Rejected.

## Measured results (2026-05-28)

Benchmarking (see `docs/BENCHMARKS.md`) confirmed redb on every target **except
on-disk size**:

- Incremental single-file re-index: **55–58 ms** (target < 200 ms) ✅
- Cold index ~1M LOC: **1.6 s** ✅; memory-mapped cold-open keeps query latency
  in the low-millisecond range ✅
- **On-disk size: ~75% of source** on a dense-call ~1M-LOC corpus (target < 15%) ❌

The logical, LZ4-compressed index data is ~4 MB (~17% of source); the redb file
is ~17 MB. The ~4× gap is redb's page/MVCC allocation model and is insensitive
to per-record compression. LZ4 + looped compaction + a separate hash table + a
slimmer `Edge` record were applied as genuine optimizations but cannot close a
constant-factor structural gap.

**Decision stands for v1:** redb's incremental and concurrency properties are
load-bearing for the architecture and the size overhead is a constant factor on
a worst-case corpus. The `GraphStore` repository boundary keeps a compact
snapshot store (the documented remediation) swappable for a later revision
without disturbing the extraction or query layers.
