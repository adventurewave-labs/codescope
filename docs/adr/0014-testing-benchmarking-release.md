# ADR-0014: Testing, Benchmarking & Release

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope` makes hard, numeric promises: cold index of a 100k-LOC repo in < 10 s, incremental re-index of a single file in < 200 ms, median query latency < 50 ms, and on-disk index size < ~15% of source size. The PRD also states that the way to win the "yet another MCP indexer" perception battle is "concrete numbers, not adjectives." Correctness matters as much as speed — wrong call edges or missed references silently mislead an agent. So testing and benchmarking are not afterthoughts; they are how we defend both the correctness and the performance claims, and they must run in CI to prevent regression.

## Decision

Adopt a layered verification strategy:

- **Unit tests** per module (parsing, symbol resolution, store, graph queries).
- **Integration tests** against committed **fixture repos** exercising the full index-then-query path across surfaces.
- **`criterion` benchmarks** for index and query latency, tracking the PRD targets (cold index, incremental re-index, query latency, index size).
- **Property tests** where useful (e.g. round-trip store invariants, graph adjacency consistency after incremental patches).

**CI** runs **`cargo fmt` (check) + `clippy` + `test`** on every change. **Release** is **dual-licensed MIT/Apache-2.0**, shipping **prebuilt per-platform binaries** alongside `cargo install`.

## Consequences

**Positive**
- Benchmarks turn the PRD's performance targets into enforced, regression-guarded numbers we can cite in launch material.
- Fixture-repo integration tests catch cross-file resolution and incremental-patch bugs that unit tests miss, directly defending against the "wrong file / missed wiring" failure mode.
- `fmt` + `clippy` in CI keeps the codebase idiomatic and lowers contributor-review cost; dual MIT/Apache-2.0 is the standard, frictionless OSS Rust license combination.

**Negative / tradeoffs**
- Criterion benchmarks are slow and machine-sensitive; absolute numbers vary across CI hardware, so we track relative regressions and run authoritative benchmarks on a stable reference machine.
- Fixture repos add repo weight and must be maintained as languages/grammars evolve.
- Property tests can surface rare, hard-to-reproduce failures that cost triage time, though they are high-value for the store/graph invariants.

## Alternatives Considered

- **Manual benchmarking / ad-hoc timing only:** cheap, but unrepeatable and unguarded against regression — the opposite of "concrete numbers." Rejected.
- **Single permissive license (MIT only):** simpler, but dual MIT/Apache-2.0 is the prevailing Rust-ecosystem expectation (patent grant via Apache-2.0). Rejected in favor of the dual license.
- **No prebuilt binaries (cargo-only):** lighter release engineering, but forfeits the curl-able, ripgrep-style install that is part of the wedge. Rejected.
