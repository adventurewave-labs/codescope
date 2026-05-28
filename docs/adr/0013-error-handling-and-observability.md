# ADR-0013: Error Handling & Observability

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope` is both a library (the Query API and indexing core) and a set of binary surfaces (CLI, JSON, MCP). Libraries and binaries have opposite error-handling needs: a library should return precise, typed, matchable errors; a binary wants ergonomic propagation with context for the user. Separately, the JSON and MCP surfaces have a hard constraint — **stdout carries machine output and protocol frames** (ADR-0010/0011), so any diagnostic logging written to stdout would corrupt it. We also index real, often broken, in-progress code, so the engine must degrade gracefully rather than abort on the first parse error.

## Decision

Use **`thiserror`** to define explicit, typed error enums for the library crates, and **`anyhow`** at the binary boundary for ergonomic propagation with added context. Use **`tracing`** for structured logging, **off/quiet by default** and enabled with **`-v`** (increasing verbosity); **all logs go to stderr** and never to stdout, so JSON and MCP streams stay clean.

On parse errors, rely on **tree-sitter's error recovery** to extract what it can and continue; emit **partial results that are explicitly labeled** as such, rather than failing the whole index or query.

## Consequences

**Positive**
- Typed `thiserror` errors let callers (and surfaces) match and handle specific failures; `anyhow` keeps the binary code concise with rich context in user-facing messages.
- The stdout/stderr split guarantees the machine contract and MCP framing are never polluted by logs, even at high verbosity.
- Graceful degradation means a broken or half-written file yields useful partial answers (with a confidence/partial label) instead of an empty or failed result — important since agents edit code mid-flight.

**Negative / tradeoffs**
- Maintaining hand-written error enums is more upfront work than a catch-all error type, and adds friction when error variants evolve.
- "Partial results, labeled" pushes responsibility onto each surface to surface the label and onto agents to respect it; an ignored label could mislead.
- Two error idioms (library vs. binary) require a clear conversion boundary and discipline about where `anyhow` is allowed.

## Alternatives Considered

- **`anyhow` everywhere, including the library:** simpler, but erases the typed error information that callers of the Query API need to react programmatically. Rejected for libraries.
- **`log` + `env_logger` instead of `tracing`:** adequate for flat logs, but `tracing`'s spans/structured fields suit the concurrent indexing pipeline (ADR-0007) better. Rejected.
- **Abort on first parse error:** trivially correct-or-nothing semantics, but useless against the broken-code reality of an agent's inner loop. Rejected in favor of recovery + labeled partials.
