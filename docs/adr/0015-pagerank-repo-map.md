# ADR-0015: PageRank Repo Map & Centrality

**Status:** Accepted

**Date:** 2026-09-26

## Context

`cs_repo_summary` ranked "key symbols" by raw in-degree. In-degree over-weights
small utilities (`name()`, `new()`) and ignores transitive importance: a function
called once by the central orchestrator matters more than one called ten times by
leaf helpers. Agents also need orientation *relative to what they are editing*,
not just a global view. Aider's repo-map showed that personalized PageRank over a
def/ref graph, rendered as signatures within a token budget, is the most
context-efficient way to orient an LLM in an unfamiliar codebase.

## Decision

- Add `rank::pagerank(graph, focus)`: power-iteration PageRank (d = 0.85,
  ≤ 50 iterations, L1 tol 1e-9) over non-module symbols using resolved
  `Calls | References | Imports | Defines` edges. Dangling mass is redistributed
  along the teleport vector.
- **Personalization:** when `focus` symbols/files are given, 90% of teleport mass
  goes to them (10% uniform floor), so the ranking answers "what matters around
  this".
- New query `repo_map` (CLI `codescope map [focus…]`, MCP `cs_repo_map`): symbols
  in rank order, test code / imports / fields excluded, grouped by file, emitted as
  one-line signatures until `max_tokens` is spent (`truncated` set honestly).
- `repo_summary.key_symbols` now uses PageRank instead of in-degree.

## Consequences

- Better "first call" for agents: highest information per token.
- Ranking cost is O(iters × edges) per call — ~ms on 100k-LOC graphs; cached
  ranking can be added if it ever shows up in profiles.
- Quality inherits heuristic resolution (ADR-0004) — mis-bound edges move rank.

## Alternatives Considered

- **In-degree / betweenness:** in-degree is noisy (above); betweenness is O(VE),
  too slow for the latency target.
- **Embedding-based relevance:** needs a model; deferred to the optional semantic
  layer (ADR-0012).
