# Services & Repositories

This document covers the behaviour layer: **domain services** (stateless logic
spanning multiple entities), the **application services** (the 8 Query API
operations with input/output contracts), and the **`GraphStore` repository**. It
also describes the two-tier resolution strategy and how `Confidence` propagates
into every result.

## Domain Services

Domain services hold logic that does not naturally belong to a single `Symbol` or
`Edge`. They are stateless and operate over the `CodeGraph` aggregate.

### `SymbolResolver`

Resolves an edge's textual target to a concrete `SymbolId`, stamping the result
with a `Confidence`. This is the heart of the **two-tier strategy**:

- **Tier 1 — Heuristic (always on).** Fast tree-sitter name + lexical-scope
  matching. Produces `EdgeTarget::Resolved(_)` with `Confidence::Heuristic`, or
  `EdgeTarget::Unresolved { raw }` if no plausible match exists. No type
  inference; deliberately approximate but cheap.
- **Tier 2 — Precise (when available).** Ingests SCIP output (e.g.
  `rust-analyzer scip`, `scip-typescript`, `scip-python`). When SCIP names a
  target, the resolver produces `Confidence::Precise` and, if it supersedes a
  prior heuristic/unresolved edge, replaces it through the aggregate root.

```rust
pub trait SymbolResolver {
    fn resolve(&self, graph: &CodeGraph, raw: &RawTarget) -> (EdgeTarget, Confidence);
}
```

Resolution is monotone in confidence: `Precise` may replace `Heuristic`/
`Unresolved`, never the reverse.

### `CallGraphTraversal`

Walks `Calls` edges over the aggregate's indexes. Callees use the **adjacency
index** (outgoing); callers use the **reverse-edge index** (incoming). Bounded by
a requested `depth`, cycle-safe (visited set), and it carries the **minimum**
`Confidence` along each path so a path through any heuristic edge is reported as
heuristic.

```rust
pub fn callers(graph: &CodeGraph, root: SymbolId, depth: u32) -> Vec<Path>;
pub fn callees(graph: &CodeGraph, root: SymbolId, depth: u32) -> Vec<Path>;
```

### `BlastRadiusCalculator`

Given a `SymbolId` or `FileId`, computes the downstream-affected set by traversing
**reverse** edges (`Calls`, `References`, `Imports`, `Contains`). A change to a
symbol affects its callers/referencers transitively; a change to a file expands
to all symbols defined in it first. Output is depth-ranked and confidence-tagged
so an agent can triage "definitely affected" (precise) vs. "possibly affected"
(heuristic).

```rust
pub fn blast_radius(graph: &CodeGraph, target: BlastTarget, depth: Option<u32>)
    -> BlastRadius; // ranked, confidence-tagged affected symbols + files
```

## Application Services — the Query API

Each operation is a token-budgeted application service in module `query`. All
share two output conventions: results are truncated to a caller-supplied
`max_tokens` with a `truncated: bool` flag (never overflow), and every returned
symbol/edge carries its `Confidence`.

| Operation | Input | Output |
|---|---|---|
| `callers` | `symbol`, `depth`, `max_tokens` | Caller symbols + call paths, each with `confidence`; `truncated`. |
| `callees` | `symbol`, `depth`, `max_tokens` | Callee symbols + call paths, each with `confidence`; `truncated`. |
| `blast_radius` | `symbol`\|`file`, `depth?`, `max_tokens` | Ranked affected symbols + files with `confidence`; `truncated`. |
| `definition` | `symbol`, `max_tokens` | Defining symbol(s): `kind, file, span, signature`, `confidence`. |
| `references` | `symbol`, `max_tokens` | All referencing sites (`file`, `span`, edge `kind`), `confidence`; `truncated`. |
| `dependency_graph` | `scope` (path/module), `max_tokens` | File/module `Imports` edges + detected cycles; `truncated`. |
| `structural_search` | structural `query`, `max_tokens` | Matching symbols (`kind, file, span, signature`) + matched edges; `truncated`. |
| `repo_summary` | `max_tokens` | Token-bounded architectural overview: top modules, key symbols, dependency shape. |

### Shared result shape

```rust
pub struct QueryResult<T> {
    pub items: Vec<T>,
    pub confidence: Confidence, // aggregate floor across items
    pub truncated: bool,        // true if max_tokens forced a cut
}

pub struct SymbolView {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub span: Span,
    pub signature: Option<String>,
    pub confidence: Confidence,
}
```

The design bar (from the PRD): an agent should be able to plan an entire refactor
from these answers without reading a file, then open only the exact `span`s it
needs.

## Repository — `GraphStore`

`GraphStore` is the persistence abstraction for the `CodeGraph` aggregate, backed
by embedded single-file `redb`. Records are keyed **per `FileId`** so incremental
re-indexing rewrites only changed files' records, and the aggregate is
reconstituted on load.

```rust
pub trait GraphStore {
    fn load(&self) -> Result<CodeGraph>;
    fn save(&self, graph: &CodeGraph) -> Result<()>;

    // Incremental, per-file (the hot path).
    fn upsert_file(&self, file: &SourceFile) -> Result<()>;
    fn remove_file(&self, id: FileId) -> Result<()>;
    fn file_hash(&self, id: FileId) -> Result<Option<ContentHash>>; // skip unchanged
}
```

The repository only persists and reconstitutes; it never resolves edges or runs
queries — those belong to the domain and application services above.

## How Confidence propagates end to end

1. **Extraction/Resolution** stamps each `Edge` with `Precise` or `Heuristic`
   (or marks it `Unresolved`).
2. **CodeGraph** preserves that `Confidence` unchanged in its indexes
   (aggregate invariant 5).
3. **Domain services** carry the **minimum** confidence along any traversed path
   (one heuristic hop makes the path heuristic).
4. **Application services** surface a per-item `confidence` and an aggregate
   floor on the `QueryResult`, so the agent always knows whether an answer is
   compiler-accurate or a fast heuristic — and unresolved targets are reported,
   never hidden.
