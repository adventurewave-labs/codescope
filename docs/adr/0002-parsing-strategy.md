# ADR-0002: Parsing Strategy

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope` must extract symbols and structural edges from real repositories, which are frequently mid-edit, syntactically broken, or large. It also has to re-index on every save while meeting the PRD targets: cold index of 100k LOC < 10 s, incremental re-index of a single changed file < 200 ms, and median query latency < 50 ms. This rules out any parsing approach that requires a full, type-correct compile or a fresh full-file parse on every keystroke.

We need a parser that (a) recovers gracefully from syntax errors so in-progress code still yields a partial graph, (b) reparses incrementally so editing one file is cheap, and (c) covers many languages with a uniform interface so the four v1 languages (ADR-0003) share extraction machinery.

## Decision

Use **tree-sitter** for all AST parsing. tree-sitter is itself Rust-friendly, supports 40+ languages through community grammars, performs robust error recovery, and supports incremental reparsing in roughly O(log n) of the edited region. `codescope` will **reparse only modified files** on change, reusing the prior syntax tree where tree-sitter's incremental API allows, and re-run extraction only for those files.

Symbol and edge extraction is driven by per-language tree-sitter **query files (`.scm`)** rather than hand-written tree walks, so adding or tuning a language's extraction is a declarative query change (see ADR-0003).

## Consequences

**Positive**
- Error recovery means agents get useful structure even from broken/in-progress files — important since agents query mid-refactor.
- Incremental reparse is the foundation for the < 200 ms incremental re-index target; 2026 benchmarks show incremental parsing cutting parse time up to ~70% on large codebases.
- One parsing abstraction across all languages; `.scm` queries keep language support declarative and reviewable.
- Mature, widely-used dependency (powers many editors), reducing parser-maintenance burden.

**Negative / tradeoffs**
- tree-sitter produces a concrete syntax tree, not a semantic/typed model — it knows *shape*, not *meaning*. Cross-file symbol resolution must be layered on top (see ADR-0004).
- Grammars compile from C, complicating the static-binary build and cross-compilation (mitigated by bundling precompiled grammars and a WASM fallback per the PRD).
- Query (`.scm`) authoring has a learning curve and must be kept in sync with grammar version bumps.

## Alternatives Considered

- **Per-language compiler front-ends / full ASTs (rustc, tsc, etc.):** maximally precise but slow, heavy, require buildable code, and fragment the architecture per language. Reserved instead as the optional Tier 2 precision layer via SCIP (ADR-0004).
- **Regex / line-based heuristics (grep-style):** trivially fast and dependency-free, but cannot model nesting or scope and would reproduce the very imprecision we differentiate against. Rejected.
- **Hand-written recursive-descent parsers:** full control but enormous maintenance cost across four-plus languages with no error-recovery payoff over tree-sitter. Rejected.
