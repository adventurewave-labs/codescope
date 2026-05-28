# ADR-0007: Concurrency Model

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope` has two workloads with opposite characteristics. **Indexing** — walking the repo, parsing each file with tree-sitter, extracting symbols/edges, resolving them — is CPU-bound and embarrassingly parallel across files; it is the hot path that must hit the cold-index targets (100k LOC < 10 s, 1M-LOC monorepo < 2 min) and the < 200 ms incremental re-index. The **MCP/HTTP server surface** that streams tool calls to agents over stdio/sockets is I/O-bound and benefits from async multiplexing of many small requests.

Forcing one concurrency model onto both would either burden the CPU-bound indexing path with async runtime overhead and `.await` plumbing, or force the I/O-bound server into a thread-per-connection model.

## Decision

Use **two complementary models, scoped to their workload**:

- **`rayon`** for parallel file parsing and extraction. The walker produces a work list of files; `rayon`'s data-parallel iterators fan parsing/extraction across the thread pool, and per-file results are merged into the store (ADR-0005) and graph (ADR-0006). The indexing hot path contains **no async**.
- **`tokio`** only for the MCP/HTTP server surface. The long-lived server uses `tokio` to handle concurrent agent requests over stdio/sockets; query handlers read the in-memory graph and return token-budgeted results.

The boundary is deliberate: a server request that triggers a (re-)index hands off to the `rayon`-based indexer rather than parsing inside the async runtime.

## Consequences

**Positive**
- The CPU-bound hot path stays free of async overhead, maximizing parsing throughput and making latency more deterministic — directly serving the index-time targets.
- `rayon`'s work-stealing scales naturally with core count and fits the per-file parallelism of tree-sitter parsing.
- `tokio` gives an efficient, idiomatic async server for many concurrent agent requests without thread-per-connection cost.
- Clear separation keeps each subsystem simple and independently testable/benchmarkable.

**Negative / tradeoffs**
- Two runtimes in one binary increases conceptual surface and binary size, and the rayon/tokio handoff boundary must be managed carefully (avoid blocking the tokio reactor on a long `rayon` index; bridge via a blocking task / channel).
- Shared state crossing the boundary (the in-memory graph) needs sound synchronization (e.g. an RwLock or snapshot/swap) so server reads don't tear against an incremental re-index.
- Slightly more complex than a single-runtime design for contributors to reason about.

## Alternatives Considered

- **`tokio` everywhere (async indexing too):** one runtime, but burdens CPU-bound parsing with async overhead and `.await` plumbing for no I/O benefit; risks starving the reactor during heavy indexing. Rejected.
- **`rayon` everywhere (thread-per-connection server):** keeps one model but makes the I/O-bound server inefficient at scale. Rejected.
- **Hand-rolled thread pool + manual epoll:** maximal control, but reinvents two mature, well-optimized libraries. Rejected.
