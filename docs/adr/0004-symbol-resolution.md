# ADR-0004: Symbol Resolution

**Status:** Accepted

**Date:** 2026-05-28

## Context

tree-sitter (ADR-0002) gives `codescope` syntactic structure but not semantics: it can see that `foo()` is a call expression, but not *which* `foo` across the repository it binds to. Accurate binding is what powers the core capabilities — callers/callees, blast radius, go-to-definition, find-all-references — and it is genuinely hard in dynamic languages (Python, JavaScript) where full accuracy needs type inference.

A central PRD principle is honesty about precision: the product is "compiler-adjacent precision where cheap, fast heuristics where not," explicitly *not* a full type checker, and resolution accuracy across dynamic languages is called out as a key risk. The mitigation named in the PRD is a labeled two-tier model mirroring Sourcegraph's proven split between fast search-based and precise compiler-accurate intelligence.

## Decision

Adopt a **two-tier hybrid** resolver:

- **Tier 1 — always-on, tree-sitter heuristic resolution.** Name- and scope-based resolution computed directly from the syntax tree (scopes, imports, lexical visibility, signature matching). Fast, requires no buildable code, runs on every index. This is the "search-based precision" baseline.
- **Tier 2 — optional, SCIP-backed precise resolution (Phase 2).** Ingest **SCIP** (SCIP Code Intelligence Protocol) output from compiler-accurate indexers — `rust-analyzer`'s `scip` command, `scip-typescript`, `scip-python` — for cross-file/cross-repo navigation when available.

**Every result is labeled with a confidence/precision level** (e.g. `precise` vs `search-based`) so the consuming agent knows how much to trust each edge.

## Consequences

**Positive**
- Tier 1 works on any repo immediately, including broken or non-buildable code, with no toolchain setup — preserving the zero-dependency install (ADR-0001).
- Tier 2 layers in compiler-grade accuracy where users have the indexers, without blocking the common path.
- Explicit confidence labels let agents make safe decisions and prevent the tool from silently over-promising on dynamic languages.
- Mirrors a battle-tested industry model (Sourcegraph), bundled into one binary.

**Negative / tradeoffs**
- Tier 1 will mis-resolve some cases (overloads, dynamic dispatch, monkey-patching, re-exports), especially in Python/JS — accepted and surfaced via the confidence label rather than hidden.
- Two resolution paths plus a merge/reconciliation step add engine complexity; SCIP ingestion (parsing the protobuf schema, reconciling SCIP symbols with tree-sitter nodes) is non-trivial and deferred to Phase 2.
- Tier 2 depends on external indexers being installed and run, which is outside `codescope`'s control.

## Alternatives Considered

- **Pure tree-sitter heuristics only:** simplest and fully self-contained, but caps precision and forecloses compiler-accurate navigation. Rejected as the *ceiling*; kept as Tier 1.
- **Embed a full language server / type checker per language:** maximal precision but heavy, slow, requires buildable code, and contradicts the single-binary non-goal of being a full type checker. Rejected.
- **Vector embeddings for "resolution":** the incumbent approach; returns similarity, not structural truth, which is precisely what we differentiate against. Reserved as a strictly additive, off-by-default semantic layer, never the resolution foundation.
