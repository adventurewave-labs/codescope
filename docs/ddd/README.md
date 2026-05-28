# Domain-Driven Design — `codescope`

This directory documents `codescope` using Domain-Driven Design (DDD). It is the
authoritative description of the domain: the language we speak, the boundaries
we draw, the model we build, and the services that operate on it. The type names
here (`SymbolId`, `CodeGraph`, `EdgeKind`, `SymbolResolver`, …) are intended to
match the actual Rust code, module-for-module.

## Why DDD for a code-intelligence engine

`codescope` turns a repository into a queryable structural graph for AI coding
agents (see `plans/codescope.prd`). The hard part is not any single algorithm —
it is keeping a coherent, precise model of "what is a symbol, what is an edge,
how confident are we, and what is consistent with what" across ingestion,
parsing, extraction, storage, and a query API consumed over CLI/JSON/MCP.

DDD gives us three things the project needs:

- **A ubiquitous language.** Agents, contributors, and the code must agree that
  a "Call edge" or a "blast radius" mean exactly one thing. Ambiguity here leaks
  straight into wrong answers handed to an agent.
- **Bounded contexts that map to Rust modules.** Each context is a module under
  `src/`, with explicit upstream/downstream relationships, so a change to the
  parser cannot silently corrupt the graph.
- **An aggregate with invariants.** The `CodeGraph` is the consistency boundary
  for every query; its invariants (every edge resolves to a symbol or a labelled
  unresolved target; `SymbolId` is stable and content-addressed) are what make
  query answers trustworthy.

## Strategic design at a glance

```
Ingestion → Parsing → Extraction → Code Graph → Storage
                                        │
                                        └──→ Query → Interfaces (CLI / JSON / MCP)
```

Each arrow is a one-directional dependency: downstream contexts conform to the
data shapes produced upstream. The `CodeGraph` aggregate sits at the centre;
Storage persists it and Query reads it.

## Document index

| Doc | Contents |
|---|---|
| [`ubiquitous-language.md`](./ubiquitous-language.md) | Glossary of every domain term, with one-line definitions. |
| [`bounded-contexts.md`](./bounded-contexts.md) | The 7 bounded contexts, their responsibilities, and a Context Map with data flow. |
| [`domain-model.md`](./domain-model.md) | Value objects, entities, the `CodeGraph` aggregate, invariants, and Rust type sketches. |
| [`services-and-repositories.md`](./services-and-repositories.md) | Domain services, the 8 query application services (I/O contracts), the `GraphStore` repository, and the two-tier resolution / Confidence strategy. |

## Conventions

- **Value objects** are immutable, identity-free, and compared by value
  (`SymbolId`, `Span`, `Language`, …).
- **Entities** have identity that persists across change (`Symbol`,
  `SourceFile`).
- **Aggregate root** is `CodeGraph`; all mutation of graph state goes through it
  so its invariants hold.
- **Domain services** hold logic that does not belong to a single entity
  (resolution, traversal, blast-radius computation).
- **Application services** are the Query API operations; they orchestrate domain
  objects and enforce the token budget.

## Related documents

- Product context and capabilities: `plans/codescope.prd`.
- Cross-cutting technical decisions: `docs/adr/` (language/distribution,
  parsing strategy, …).
