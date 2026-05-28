# ADR-0011: Token-Budgeted Output Contract

**Status:** Accepted

**Date:** 2026-05-28

## Context

The whole point of `codescope` is to keep an agent out of its files and inside its context budget. An unbounded answer — dumping every caller, every reference, every transitive edge — defeats the purpose: it blows the context window and forces the agent to do the very triage we exist to prevent. The design bar from the PRD is explicit: *an agent should be able to plan an entire refactor from `codescope` answers without reading a single file.* That requires every answer to be compact, structured, and bounded, with honest signaling when it has been trimmed.

## Decision

Every query accepts a **`max-tokens` / `max-results` hint**. Output is **compact JSON** with a stable shape per record: `symbol`, `kind`, `file`, `line_start`, `line_end`, and **edge lists** (callers/callees/refs/deps as applicable). When a result would exceed the budget, **truncate** the result set and set **`truncated: true`** on the response rather than emitting the full payload.

Token cost is estimated with a cheap heuristic of **~4 characters per token** applied to the serialized payload; results are accumulated until adding the next record would cross the budget. The same contract is shared across CLI `--json`, stdout JSON, and MCP tools (ADR-0009), so the budget behaves identically everywhere.

## Consequences

**Positive**
- Answers fit the agent's context window by design, directly supporting the PRD value metric (40%+ token reduction on multi-file refactor tasks).
- `truncated: true` lets the agent reason about completeness and re-query with a larger budget or narrower scope instead of silently acting on partial data.
- Line ranges (not file bodies) mean the agent opens only the exact spans it needs — the "plan without reading files" bar.

**Negative / tradeoffs**
- The 4-chars/token heuristic is approximate and tokenizer-dependent; it can over- or under-estimate, so we treat the budget as a soft bound and err toward staying under it.
- Truncation requires a deterministic ordering (e.g. by relevance/proximity) so that what survives the cut is the most useful subset, adding ranking responsibility to the Query API.
- Compact JSON is less human-readable; the CLI's default human formatting (ADR-0009) compensates for interactive use.

## Alternatives Considered

- **Unbounded output, let the caller truncate:** simplest, but pushes context-blowout risk onto the agent and defeats the product's core value. Rejected.
- **Exact tokenization with a bundled tokenizer (e.g. tiktoken/BPE):** more accurate budgeting but adds a heavy dependency and per-model variance for marginal gain over the heuristic. Rejected for v1.
- **Pagination cursors instead of truncation flags:** richer for large result sets but adds stateful protocol surface; `truncated` + re-query with a wider budget covers v1 needs. Deferred.
