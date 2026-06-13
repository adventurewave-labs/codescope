# ADR-0001: Language & Distribution

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope` competes in the developer-tooling niche occupied by ripgrep, fd, bat, Alacritty, Zed, and Biome — tools that displaced their predecessors because their runtime profile (fast startup, low memory, deterministic latency) is exactly what local dev tooling demands. A code-intelligence engine consumed by AI agents has the same profile, raised higher: agents issue many small queries and re-index on every save, so per-invocation startup cost and steady-state memory dominate the experience.

The incumbent MCP code-indexers (CodeIndexer, codebase-indexer-mcp, CodeGraphContext) are written in Python and depend on a separate vector database (ChromaDB, Milvus). They are slow to install (`pip` + DB provisioning) and slow to index, and they return approximate similarity results. The PRD's differentiation table makes "single binary, no external DB" and `cargo install` / curl-able install the headline wedge.

## Decision

Build `codescope` in **Rust**, shipped as a **single static binary** with no runtime dependencies. Distribution is via `cargo install codescope` and curl-able prebuilt release binaries (per-platform artifacts attached to GitHub releases). There is **no Python runtime, no Docker requirement, and no external database** — the embedded store ships inside the binary.

## Consequences

**Positive**
- Sub-millisecond process startup and low resident memory support the PRD targets (median query latency < 50 ms; cold index of 100k LOC < 10 s) without a warm daemon.
- Zero-friction install matches the ripgrep/fd mental model; no virtualenv, no container, no DB server to provision.
- Memory safety plus `rayon`-friendly fearless concurrency for the CPU-bound indexing hot path (see ADR-0007).
- Rust credibility with the target community and natural fit with the Rust-native tree-sitter and `redb` ecosystem.

**Negative / tradeoffs**
- Slower iteration and steeper contributor onboarding than Python; smaller pool of casual contributors.
- Cross-compiling and shipping prebuilt binaries for every target (Linux/macOS/Windows, x86_64/aarch64) adds release-engineering overhead, compounded by tree-sitter grammars that compile from C (see ADR-0002/0003).
- No dynamic plugin loading at runtime; new languages/features require a rebuild.

## Alternatives Considered

- **Python + vector DB (incumbent shape):** fastest to prototype and rich ML ecosystem, but exactly the install/latency footprint we are differentiating against. Rejected.
- **Go:** good single-binary story and fast compiles, but weaker tree-sitter integration and GC pauses undercut deterministic latency. Rejected.
- **Distribute via Docker image:** guarantees environment reproducibility but adds a heavyweight dependency for a tool meant to be as light as `rg`. Rejected for v1; not precluded as an optional artifact.
