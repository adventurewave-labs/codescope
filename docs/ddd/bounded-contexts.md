# Bounded Contexts & Context Map

`codescope` is decomposed into seven bounded contexts, each owning a coherent
slice of the model and each implemented as a Rust module under `src/`. A bounded
context is where one model and one ubiquitous language hold without ambiguity;
crossing a boundary means translating into the downstream context's terms.

The flow is a mostly linear pipeline — `Ingestion → Parsing → Extraction →
Code Graph → Storage/Query → Interfaces` — converging on the `CodeGraph`
aggregate, which Storage persists and Query reads.

## 1. Ingestion — module `walker`

**Responsibility.** Walk the repository, ignore-aware (respects `.gitignore` and
standard ignore rules), read file contents, and compute a `ContentHash` per file.
Decides *which files exist and which changed*.

**Produces.** A stream of `(path, language, bytes, content_hash)` candidates. The
`content_hash` is the pivot for incremental re-indexing: unchanged files are
skipped downstream.

**Knows nothing about.** ASTs, symbols, edges, or queries. It is purely
filesystem + hashing.

## 2. Parsing — module `parser`

**Responsibility.** Turn source bytes into an `AST` using tree-sitter, selecting
the correct per-language grammar from `Language`. Tolerates broken/in-progress
code via tree-sitter's error recovery; supports incremental reparse.

**Consumes.** Ingestion output. **Produces.** A parsed tree per `SourceFile`.

**Boundary note.** Parsing has no opinion about what a "symbol" is; it only
yields a syntax tree. The mapping from tree to domain happens downstream.

## 3. Symbol Extraction & Resolution — module `extract`

**Responsibility.** Run tree-sitter queries over each `AST` to extract `Symbol`s
and candidate `Edge`s, then resolve edge targets to `SymbolId`s. Resolution is
**two-tier**: fast tree-sitter name/scope heuristics now (`Heuristic`), SCIP
ingestion for compiler-accurate targets later (`Precise`). Every edge carries a
`Confidence`; unresolvable targets become labelled **unresolved targets**, never
dropped.

**Consumes.** ASTs from Parsing. **Produces.** Per-file `(symbols, edges)` ready
to be merged into the graph, each edge stamped with `Confidence`.

## 4. Code Graph — module `graph`

**Responsibility.** The aggregate. Owns the full set of `Symbol` nodes and `Edge`
relations plus the **adjacency index** (forward edges) and **reverse-edge index**
(incoming edges). `CodeGraph` is the consistency boundary: all mutation goes
through it so invariants hold (every edge references an existing symbol or a
labelled unresolved target; `SymbolId`s are stable and content-addressed).

**Consumes.** Per-file extraction output. **Produces.** A consistent, indexed
graph for Storage and Query.

## 5. Storage — module `store`

**Responsibility.** Embedded, single-file persistence via `redb`. Records are
keyed **per file** so incremental updates rewrite only changed files' records.
Implements the `GraphStore` repository: persist/load the `CodeGraph` and per-file
`SourceFile` records.

**Consumes.** The `CodeGraph` aggregate. **Produces.** A durable on-disk index,
and reconstitutes the aggregate on load.

## 6. Query — module `query`

**Responsibility.** Application services answering agent questions over the
loaded `CodeGraph`: `callers`, `callees`, `blast_radius`, `definition`,
`references`, `dependency_graph`, `structural_search`, `repo_summary`. All output
is **token-budgeted** and propagates `Confidence` so the agent knows how much to
trust an answer.

**Consumes.** The `CodeGraph` (via `GraphStore`) and domain services
(`BlastRadiusCalculator`, `CallGraphTraversal`). **Produces.** Compact result
DTOs for Interfaces.

## 7. Interfaces — module `interfaces`

**Responsibility.** Adapt Query results to the outside world: CLI (`clap`), JSON
on stdout, and an MCP server (JSON-RPC over stdio). This is the
anti-corruption / presentation layer — it translates domain results into each
protocol's shape and enforces protocol-level concerns (MCP tool schemas, exit
codes).

**Consumes.** Query application services. **Produces.** CLI text, JSON, MCP tool
responses.

## Context Map

Arrows show upstream → downstream dependency and data flow. Downstream contexts
**conform** to the data shapes produced upstream.

```
  repo on disk
       │  files + content hashes
       ▼
┌──────────────┐   AST    ┌──────────────┐  symbols+edges  ┌──────────────────┐
│  Ingestion   │ ───────▶ │   Parsing    │ ──────────────▶ │   Extraction &   │
│  (walker)    │          │   (parser)   │                 │   Resolution     │
└──────────────┘          └──────────────┘                 │   (extract)      │
                                                            └────────┬─────────┘
                                       per-file (symbols, edges)     │
                                       + Confidence                  ▼
                                                            ┌──────────────────┐
                                                            │   Code Graph     │
                                                            │   (graph)        │  ◀── AGGREGATE
                                                            │  nodes + edges + │
                                                            │  fwd/rev indexes │
                                                            └───┬──────────┬───┘
                                          persist / load        │          │  read
                                                                ▼          ▼
                                                       ┌────────────┐  ┌──────────────┐
                                                       │  Storage   │  │   Query      │
                                                       │  (store)   │  │  (query)     │
                                                       │ redb, per- │  │ 8 token-     │
                                                       │ file keys  │  │ budgeted ops │
                                                       └────────────┘  └──────┬───────┘
                                                                               │ result DTOs
                                                                               ▼
                                                                      ┌──────────────────┐
                                                                      │   Interfaces     │
                                                                      │  (interfaces)    │
                                                                      │  CLI · JSON ·    │
                                                                      │  MCP (stdio)     │
                                                                      └──────────────────┘
```

### Relationship summary

| Upstream | Downstream | Relationship |
|---|---|---|
| Ingestion | Parsing | Customer/Supplier — Parsing conforms to file+hash stream. |
| Parsing | Extraction | Customer/Supplier — Extraction consumes ASTs. |
| Extraction | Code Graph | Conformist — extraction output is merged through the aggregate root. |
| Code Graph | Storage | Repository pairing — `GraphStore` persists/loads the aggregate. |
| Code Graph | Query | Shared kernel — Query reads the aggregate and its indexes directly. |
| Query | Interfaces | Open Host / Published Language — CLI, JSON, and MCP are protocol adapters over a stable result contract. |
