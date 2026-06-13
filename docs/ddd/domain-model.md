# Domain Model

This is the tactical model of `codescope`: value objects, entities, the
`CodeGraph` aggregate root, and the invariants that make query answers
trustworthy. Type sketches are Rust-flavoured and intended to match the real
code; field details may evolve, but names and roles are fixed.

## Value Objects

Value objects are immutable and compared by value. They carry no identity of
their own beyond their contents.

### `SymbolId` — stable, content-addressed identity

A symbol's identity is a hash of the tuple `(language, path, name, kind, line)`.
It is **stable** (same inputs → same id across runs and machines) and
**content-addressed** (derived from the symbol's own attributes, not an
autoincrement). This lets per-file incremental updates re-key into the graph and
lets edges refer to symbols by id without a central counter.

```rust
pub struct SymbolId(pub u64); // blake3/xxhash of language|path|name|kind|line

impl SymbolId {
    pub fn new(language: Language, path: &Path, name: &str,
               kind: SymbolKind, line: u32) -> Self { /* hash */ }
}
```

### `FileId`, `Language`, `SymbolKind`

```rust
pub struct FileId(pub u64); // stable hash of the repo-relative path

pub enum Language { Rust, TypeScript, JavaScript, Python, Go }

pub enum SymbolKind {
    Function, Method, Struct, Enum, Trait, Interface,
    Class, Module, Type, Constant, Field, Import,
}
```

### `Span`, `EdgeKind`, `Confidence`

```rust
pub struct Span {
    pub line_start: u32,
    pub line_end: u32,
    pub byte_start: u32,
    pub byte_end: u32,
}

pub enum EdgeKind { Calls, References, Imports, Contains, Defines }

pub enum Confidence { Precise, Heuristic }
```

`Confidence::Precise` comes from SCIP-backed resolution; `Confidence::Heuristic`
from tree-sitter name/scope heuristics. It is attached at the edge level and
propagated into every query result so an agent can weigh the answer.

## Entities

Entities have identity that persists across change.

### `Symbol`

A named code construct — the node of the graph. Its identity is its `SymbolId`;
two `Symbol`s are the same entity iff their ids match, regardless of incidental
attribute drift.

```rust
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>, // declaration text, for token-cheap output
    pub file: FileId,
    pub span: Span,
    pub language: Language,
    pub container: Option<SymbolId>, // enclosing symbol, if any
}
```

### `SourceFile`

A single indexed file and the **unit of incremental update**. Its `content_hash`
decides whether re-parsing is needed; its `symbols`/`edges` are the file's
contribution to the graph and are rewritten atomically on change.

```rust
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub language: Language,
    pub content_hash: ContentHash,
    pub symbols: Vec<SymbolId>,
    pub edges: Vec<Edge>,
}
```

### `Edge`

A directed relationship between symbols. The target may be a resolved `SymbolId`
or a labelled **unresolved target** (text that could not be resolved). Every edge
carries `Confidence`.

```rust
pub enum EdgeTarget {
    Resolved(SymbolId),
    Unresolved { raw: String }, // kept, never silently dropped
}

pub struct Edge {
    pub kind: EdgeKind,
    pub source: SymbolId,
    pub target: EdgeTarget,
    pub confidence: Confidence,
    pub span: Span, // where the relationship occurs
}
```

## Aggregate Root — `CodeGraph`

`CodeGraph` is the single aggregate and the **consistency boundary for all
queries**. It owns the symbols, the edges, and the forward (adjacency) and
reverse-edge indexes. All mutation flows through it so invariants are preserved;
external code never mutates the indexes directly.

```rust
pub struct CodeGraph {
    symbols: HashMap<SymbolId, Symbol>,
    files: HashMap<FileId, SourceFile>,
    adjacency: HashMap<SymbolId, Vec<Edge>>,      // outgoing (callees, imports…)
    reverse:   HashMap<SymbolId, Vec<Edge>>,      // incoming (callers, importers…)
}

impl CodeGraph {
    pub fn upsert_file(&mut self, file: SourceFile, syms: Vec<Symbol>, edges: Vec<Edge>);
    pub fn remove_file(&mut self, id: FileId);
    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol>;
    pub fn outgoing(&self, id: SymbolId) -> &[Edge];
    pub fn incoming(&self, id: SymbolId) -> &[Edge];
}
```

### Invariants (always true after any mutation)

1. **Content-addressed stable ids.** A `SymbolId` is fully determined by
   `(language, path, name, kind, line)`. Re-indexing an unchanged symbol yields
   the same id; ids are never reassigned.
2. **Edge endpoints exist or are labelled.** For every `Edge`, `source` is a
   `SymbolId` present in `symbols`, and `target` is either a present `SymbolId`
   (`EdgeTarget::Resolved`) or an `EdgeTarget::Unresolved` — there are no
   dangling resolved targets.
3. **Index/edge agreement.** An edge appears in `adjacency[source]` iff it
   appears in `reverse[target]` (when the target is resolved). The two indexes are
   never out of sync.
4. **File ownership.** Every `Symbol` and `Edge` belongs to exactly one
   `SourceFile`; `upsert_file` and `remove_file` add/retract a file's full
   contribution atomically (no partial files in the graph).
5. **Confidence is preserved.** An edge's `Confidence` set during extraction is
   carried unchanged into the indexes; resolution never silently upgrades or
   downgrades it.

### Consistency rules

- **Per-file atomicity.** The smallest consistent change is one `SourceFile`.
  Incremental updates replace a file's entire `(symbols, edges)` set in one
  `upsert_file`/`remove_file` call; orphaned reverse edges to removed symbols are
  cleaned up in the same operation.
- **Single writer to indexes.** Indexes are private; only `CodeGraph` methods
  rebuild them, guaranteeing invariant 3.
- **Resolution is monotone in confidence.** When SCIP data later resolves a
  previously `Unresolved` target, the edge is replaced via the aggregate root,
  flipping the target to `Resolved` and `Confidence` to `Precise` — again as a
  single consistent mutation, never a partial in-place patch.
- **Queries read a snapshot.** The Query context treats the loaded `CodeGraph` as
  an immutable read model; it never mutates the aggregate during a query.
