# ADR-0006: Graph Model & Query Engine

**Status:** Accepted

**Date:** 2026-05-28

## Context

The core capabilities — symbol map, callers/callees (transitive), blast radius, definition/references, dependency graph — are all graph queries over symbols and their relationships. The PRD requires median query latency < 50 ms, including transitive traversals to a requested depth. The on-disk store (ADR-0005) is a per-file keyed KV store optimized for incremental writes, not for traversal, so we need a query layer that turns persisted records into fast graph operations.

Blast radius and "who calls X" are inherently *reverse* queries (incoming edges), which a forward-only adjacency representation answers poorly.

## Decision

Maintain an **in-memory `CodeGraph`** loaded from the store at startup:

- **Nodes** = symbols (functions, types, structs/classes, traits/interfaces, modules), each carrying signature and `file` + `line_start`/`line_end`.
- **Edges**, typed: `Calls`, `References`, `Imports`, `Contains`, `Defines`.
- **Adjacency indexes** for O(1) neighbor lookup, plus a **reverse-edge index** so callers and blast-radius (incoming edges) are as fast as callees.

Transitive queries — callers, callees, blast radius — are computed via **breadth-first search to a bounded depth** (the depth the caller requests). Results are token-budgeted at the output layer per the PRD's output contract: truncate with a `truncated: true` flag rather than dumping. On incremental re-index, the changed file's nodes/edges are patched into the in-memory graph in lockstep with the store update (ADR-0005).

## Consequences

**Positive**
- In-memory adjacency + reverse-edge indexes make neighbor lookups O(1) and bounded-depth BFS cheap, supporting the < 50 ms target.
- The reverse-edge index makes the single most-requested query (blast radius / "what will this edit break?") first-class rather than a full-graph scan.
- Typed edges map cleanly onto MCP tools (`cs_callers`, `cs_callees`, `cs_blast_radius`, `cs_dependency_graph`).
- Bounded-depth BFS plus token budgeting keeps payloads within an agent's context window.

**Negative / tradeoffs**
- The full graph (plus forward and reverse indexes) lives in RAM, so memory scales with repo size; very large monorepos (1M-LOC target) need a compact node/edge encoding and may need lazy/partial loading later.
- Maintaining forward and reverse indexes in sync on every incremental patch adds bookkeeping and a risk of drift if not carefully transactional.
- Startup pays a graph-load cost from the store; acceptable given memory-mapped reads (ADR-0005) but a factor for cold queries.

## Alternatives Considered

- **Query the on-disk store directly per traversal step:** no in-memory footprint, but each BFS hop becomes disk lookups, blowing the latency budget on transitive queries. Rejected.
- **Embedded graph database / datalog engine:** richer query semantics, but heavier dependency and contrary to the single-binary, in-memory-traversal design. Rejected.
- **Forward-adjacency only (compute reverse on demand):** smaller memory, but makes callers/blast-radius O(graph) per query — unacceptable for the headline use case. Rejected in favor of an explicit reverse index.
