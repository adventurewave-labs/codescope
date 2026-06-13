# Ubiquitous Language

This glossary is the shared vocabulary of `codescope`. Every term has exactly one
meaning, used identically in conversation, documentation, and code. When a term
maps to a Rust type, the type name is given verbatim.

## Core domain terms

| Term | Definition |
|---|---|
| **Symbol** | A named, addressable code construct (function, struct, trait, etc.); the node type of the graph. Entity `Symbol`. |
| **SymbolId** | Stable, content-addressed identity of a symbol: a hash of `language + path + name + kind + line`. Value object. |
| **SymbolKind** | The category of a symbol: `Function, Method, Struct, Enum, Trait, Interface, Class, Module, Type, Constant, Field, Import`. Value object (enum). |
| **Signature** | The human-readable declaration text of a symbol (e.g. a function's parameter/return list), stored for token-efficient output. |
| **Container** | The enclosing symbol of another symbol (e.g. the `Struct` containing a `Method`), expressed structurally via `Contains` edges. |
| **Edge** | A directed structural relationship between two symbols; the edge type of the graph. |
| **EdgeKind** | The category of an edge: `Calls, References, Imports, Contains, Defines`. Value object (enum). |
| **Calls** | An edge meaning the source symbol invokes the target symbol (the call graph). |
| **References** | An edge meaning the source symbol mentions/uses the target symbol without calling it. |
| **Imports** | An edge meaning a file or module brings another module/symbol into scope (the dependency/import graph). |
| **Contains** | An edge meaning the source symbol lexically encloses the target (module contains function, struct contains field). |
| **Defines** | An edge meaning the source (file or symbol) is where the target symbol is declared. |
| **Span** | The location of a construct in a file: `line_start, line_end, byte_start, byte_end`. Value object. |
| **SourceFile** | A single indexed file: `id, path, language, content_hash, symbols, edges`. Entity and unit of incremental update. |
| **FileId** | Stable identity of a source file. Value object. |
| **Language** | The programming language of a file/symbol: `Rust, TypeScript, JavaScript, Python, Go`. Value object (enum). |
| **ContentHash** | A hash of a file's bytes, used to detect change and drive incremental re-indexing. |

## Graph and query terms

| Term | Definition |
|---|---|
| **Code Graph** | The aggregate: all symbols + all edges + adjacency and reverse-edge indexes. Aggregate root `CodeGraph`. |
| **Aggregate** | A consistency boundary owning a cluster of objects; here, `CodeGraph` is the single aggregate guarding graph invariants. |
| **Aggregate Root** | The one entry point through which the aggregate is mutated and read consistently; `CodeGraph`. |
| **Adjacency Index** | Forward map from a symbol to its outgoing edges (e.g. callees). |
| **Reverse-Edge Index** | Backward map from a symbol to its incoming edges (e.g. callers); makes "who calls X" O(1) to look up. |
| **Call Graph** | The subgraph of `Calls` edges; queried as callers (incoming) and callees (outgoing), transitively to a depth. |
| **Callers** | Symbols that call a given symbol (incoming `Calls` edges, transitive to depth). |
| **Callees** | Symbols a given symbol calls (outgoing `Calls` edges, transitive to depth). |
| **Blast Radius** | The set of symbols/files downstream-affected if a given symbol or file changes; computed over reverse edges. |
| **Definition** | The declaration site of a symbol (go-to-def), found structurally, not by text match. |
| **References** (query) | All sites that use a symbol (find-all-references), found structurally. |
| **Dependency Graph** | The module/file-level graph of `Imports` edges, including cycle detection. |
| **Structural Search** | Querying by code structure (e.g. "functions that call `db.query` and return `Result`"), not plain text. |
| **Repo Summary** | An auto-generated, token-bounded architectural overview an agent reads before acting. |

## Resolution and confidence terms

| Term | Definition |
|---|---|
| **Symbol Resolution** | Mapping an edge's textual target to a concrete `SymbolId` in the graph. |
| **Unresolved Target** | An edge target that could not be resolved to a known `SymbolId`; retained, labelled, never silently dropped. |
| **Confidence** | The trust level of a resolution result: `Precise` or `Heuristic`. Value object (enum). |
| **Precise** | Confidence from compiler-accurate resolution (SCIP). |
| **Heuristic** | Confidence from fast tree-sitter name/scope heuristics (no full type inference). |
| **Two-Tier Resolution** | Strategy of using fast heuristics always-on and SCIP precision when available; mirrors Sourcegraph's search-based vs. precise model. |
| **SCIP** | SCIP Code Intelligence Protocol; compiler-accurate index output (e.g. from `rust-analyzer`, `scip-typescript`) ingested for `Precise` edges. |
| **Tree-sitter** | The incremental, error-recovering parser library used to produce ASTs and run extraction queries. |
| **AST** | Abstract Syntax Tree; the tree-sitter parse output that extraction queries run against. |
| **Token Budget** | A caller-supplied max-tokens hint that bounds query output; results truncate with a `truncated` flag rather than overflowing. |

## Building-block stereotypes

| Term | Definition |
|---|---|
| **Value Object** | Immutable, identity-free, compared by value (`SymbolId`, `Span`, `Language`, `EdgeKind`, `Confidence`). |
| **Entity** | An object with identity persisting across change (`Symbol`, `SourceFile`). |
| **Domain Service** | Stateless domain logic spanning multiple objects (`SymbolResolver`, `BlastRadiusCalculator`, `CallGraphTraversal`). |
| **Repository** | Persistence abstraction for an aggregate (`GraphStore`). |
| **Application Service** | An orchestration operation exposed to interfaces — the 8 Query API operations. |
| **Bounded Context** | A boundary within which a model and its language are consistent; here, one per Rust module. |
| **Context Map** | The diagram of relationships and data flow between bounded contexts. |
