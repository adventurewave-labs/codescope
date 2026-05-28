# ADR-0008: Incremental Indexing

**Status:** Accepted

**Date:** 2026-05-28

## Context

Agents re-index on every save and every commit. A full re-parse of a large repo on each change would violate the PRD targets (incremental re-index of a single changed file < 200 ms) and make the tool feel sluggish in the inner loop. tree-sitter already re-parses modified code in roughly O(log n), and 2026 benchmarks show incremental parsing cuts parse time by up to ~70% on large codebases — but only if we feed it just the files that actually changed. We therefore need a cheap, reliable mechanism to detect *which* files changed and to patch the store and in-memory graph in place rather than rebuilding from scratch.

## Decision

Detect changes with a **file watcher (`notify` crate)** combined with **content hashing** (blake3 or seahash) recorded per file in the store. On a filesystem event, compare the new content hash against the stored hash; ignore events whose content is unchanged (editor save-without-change, metadata-only touches).

For each genuinely changed file, **re-parse only that file**, then **patch the store**: delete all records keyed to that file (symbols, edges, references), insert the freshly parsed records, and **rebuild the affected portion of the in-memory adjacency** so callers/callees/blast-radius queries stay correct. The index is **keyed to allow future commit-hash association**, so a later phase can snapshot per-commit indices and "navigate any commit" without a schema migration.

## Consequences

**Positive**
- Meets the < 200 ms incremental target by bounding work to changed files plus their immediate graph neighborhood.
- Content hashing suppresses redundant work from spurious or no-op filesystem events, and gives a cheap integrity check for detecting external/out-of-band edits.
- Commit-hash keying is designed in now, avoiding a costly reindex/migration when commit-aware navigation lands.

**Negative / tradeoffs**
- Cross-file edges (a renamed symbol referenced elsewhere) require touching neighbor files' adjacency, so a "single file" change can fan out; we bound this to direct neighbors and accept eventual full-consistency on the next full index.
- `notify` backends differ across platforms (inotify/FSEvents/ReadDirectoryChangesW); event coalescing and rename semantics vary and must be normalized.
- Stale-state risk if the process misses events while not running; mitigated by a hash-sweep reconciliation pass on startup.

## Alternatives Considered

- **Full re-index on every change:** simplest and always-correct, but blows the latency budget on large repos. Rejected.
- **Polling mtimes instead of a watcher:** avoids platform watcher quirks but adds latency and wakeups; mtime is also less reliable than content hashing. Rejected as the primary mechanism (kept as the startup reconciliation sweep).
- **Git-hook-driven indexing only:** misses unsaved/uncommitted edits in the agent inner loop, which is exactly when freshness matters most. Rejected as sole trigger.
