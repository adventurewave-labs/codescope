//! Domain model for codescope.
//!
//! This module is the heart of the system: it defines the ubiquitous language
//! (see `docs/ddd/ubiquitous-language.md`) as Rust types. The aggregate root is
//! [`CodeGraph`]; everything else is an entity or value object that lives inside
//! it.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// A programming language codescope can index. Value object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
}

impl Language {
    /// Infer a language from a file path's extension. Returns `None` for
    /// unsupported files (the walker filters these out).
    pub fn from_path(path: &Path) -> Option<Language> {
        let ext = path.extension()?.to_str()?;
        Some(match ext {
            "rs" => Language::Rust,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "py" | "pyi" => Language::Python,
            "go" => Language::Go,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The kind of a symbol. Value object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Module,
    Type,
    Constant,
    Field,
    Import,
}

impl SymbolKind {
    pub fn name(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Interface => "interface",
            SymbolKind::Class => "class",
            SymbolKind::Module => "module",
            SymbolKind::Type => "type",
            SymbolKind::Constant => "constant",
            SymbolKind::Field => "field",
            SymbolKind::Import => "import",
        }
    }

    /// Whether this kind is callable (participates in the call graph).
    pub fn is_callable(self) -> bool {
        matches!(self, SymbolKind::Function | SymbolKind::Method)
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A source location range. Lines are 1-based and inclusive; bytes are 0-based.
/// Value object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub line_start: u32,
    pub line_end: u32,
    pub byte_start: u32,
    pub byte_end: u32,
}

impl Span {
    pub fn line_count(&self) -> u32 {
        self.line_end.saturating_sub(self.line_start) + 1
    }
}

/// A stable, content-addressed identity for a symbol. Value object.
///
/// Computed from `language + path + name + kind + line_start` so that the same
/// declaration keeps the same id across re-indexes as long as it does not move
/// lines — which keeps incremental updates and cross-file edges stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId(pub u64);

impl SymbolId {
    pub fn compute(
        language: Language,
        path: &str,
        name: &str,
        kind: SymbolKind,
        line_start: u32,
    ) -> SymbolId {
        // seahash is fast and stable across runs (no random seed).
        let mut buf = String::with_capacity(path.len() + name.len() + 24);
        buf.push_str(language.name());
        buf.push('\u{1}');
        buf.push_str(path);
        buf.push('\u{1}');
        buf.push_str(name);
        buf.push('\u{1}');
        buf.push_str(kind.name());
        buf.push('\u{1}');
        buf.push_str(&line_start.to_string());
        SymbolId(seahash::hash(buf.as_bytes()))
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Confidence level of a resolved edge. Mirrors the two-tier resolution model
/// (ADR-0004): `Precise` comes from a compiler-grade indexer (SCIP), `Heuristic`
/// from fast tree-sitter name/scope resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Confidence {
    Precise,
    Heuristic,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Confidence::Precise => "precise",
            Confidence::Heuristic => "heuristic",
        }
    }
}

/// The kind of a relationship between symbols. Value object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    /// A callable invokes another callable.
    Calls,
    /// A symbol references another symbol (non-call use).
    References,
    /// A file/module imports another module.
    Imports,
    /// A container symbol lexically contains a child (struct→field, module→fn).
    Contains,
    /// A symbol defines/implements another (impl→trait).
    Defines,
}

impl EdgeKind {
    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Calls => "calls",
            EdgeKind::References => "references",
            EdgeKind::Imports => "imports",
            EdgeKind::Contains => "contains",
            EdgeKind::Defines => "defines",
        }
    }
}

/// A directed relationship between a source symbol and a target. The target may
/// be unresolved (we know the name used at the call site but not which symbol it
/// binds to). Entity-ish, but value-comparable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: SymbolId,
    /// The name written at the use site (e.g. the callee identifier).
    pub to_name: String,
    /// Resolved target symbol, if resolution succeeded.
    pub to: Option<SymbolId>,
    pub confidence: Confidence,
    /// 1-based line of the use site. Edges don't need full byte ranges (only
    /// symbols do), so we keep just the line to shrink the on-disk/in-memory
    /// graph — edges dominate the graph by volume (ADR-0005, ADR-0006).
    pub line: u32,
}

/// A symbol: a named, located declaration in the codebase. Entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    /// A compact, single-line signature (e.g. `fn foo(a: i32) -> Result<()>`).
    pub signature: String,
    pub language: Language,
    /// File path relative to the indexed repo root.
    pub file: String,
    pub span: Span,
    /// The id of the lexically enclosing symbol, if any (e.g. the struct a
    /// method belongs to, or the module a function lives in).
    pub container: Option<SymbolId>,
}

/// A source file's extracted contribution to the graph. Entity / per-file
/// aggregate used as the unit of incremental update (ADR-0008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub language: Language,
    /// Content hash for change detection.
    pub content_hash: u64,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

/// The aggregate root. Holds every symbol and edge plus the adjacency indexes
/// that make queries O(1) on neighbors. The in-memory consistency boundary for
/// all queries (ADR-0006).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CodeGraph {
    symbols: HashMap<SymbolId, Symbol>,
    /// All edges, owned centrally.
    edges: Vec<Edge>,
    // ---- derived indexes (rebuilt by `reindex`, not serialized authoritatively)
    #[serde(skip)]
    by_name: HashMap<String, Vec<SymbolId>>,
    #[serde(skip)]
    out_edges: HashMap<SymbolId, Vec<usize>>,
    #[serde(skip)]
    in_edges: HashMap<SymbolId, Vec<usize>>,
    /// Symbols defined in a given file path.
    #[serde(skip)]
    by_file: HashMap<String, Vec<SymbolId>>,
}

impl CodeGraph {
    pub fn new() -> Self {
        CodeGraph::default()
    }

    /// Merge a parsed file's symbols and edges into the graph (replacing any
    /// prior contribution from the same path). Does NOT rebuild indexes; call
    /// [`CodeGraph::reindex`] after a batch of inserts.
    pub fn upsert_file(&mut self, file: SourceFile) {
        self.remove_file(&file.path);
        for sym in file.symbols {
            self.symbols.insert(sym.id, sym);
        }
        self.edges.extend(file.edges);
    }

    /// Remove every symbol and edge originating from a file path.
    pub fn remove_file(&mut self, path: &str) {
        let removed: Vec<SymbolId> = self
            .symbols
            .values()
            .filter(|s| s.file == path)
            .map(|s| s.id)
            .collect();
        let removed_set: std::collections::HashSet<SymbolId> = removed.iter().copied().collect();
        for id in &removed {
            self.symbols.remove(id);
        }
        // Drop edges whose source symbol was removed.
        self.edges.retain(|e| !removed_set.contains(&e.from));
    }

    /// Replace the entire edge set (used by the resolver after binding targets).
    /// Caller must invoke [`CodeGraph::reindex`] afterwards.
    pub fn replace_edges(&mut self, edges: Vec<Edge>) {
        self.edges = edges;
    }

    /// Rebuild all derived adjacency indexes. Call once after bulk loading or a
    /// batch of upserts.
    pub fn reindex(&mut self) {
        self.by_name.clear();
        self.out_edges.clear();
        self.in_edges.clear();
        self.by_file.clear();

        for sym in self.symbols.values() {
            self.by_name
                .entry(sym.name.clone())
                .or_default()
                .push(sym.id);
            self.by_file
                .entry(sym.file.clone())
                .or_default()
                .push(sym.id);
        }
        for (idx, edge) in self.edges.iter().enumerate() {
            self.out_edges.entry(edge.from).or_default().push(idx);
            if let Some(to) = edge.to {
                self.in_edges.entry(to).or_default().push(idx);
            }
        }
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(&id)
    }

    pub fn symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.values()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// All symbols sharing a given name. Empty slice if none.
    pub fn by_name(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Symbols defined in a file path.
    pub fn by_file(&self, path: &str) -> &[SymbolId] {
        self.by_file.get(path).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All distinct file paths in the graph.
    pub fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = self.by_file.keys().cloned().collect();
        v.sort();
        v
    }

    /// Outgoing edges from a symbol.
    pub fn out_edges(&self, id: SymbolId) -> impl Iterator<Item = &Edge> {
        self.out_edges
            .get(&id)
            .into_iter()
            .flatten()
            .map(move |&i| &self.edges[i])
    }

    /// Incoming (resolved) edges to a symbol.
    pub fn in_edges(&self, id: SymbolId) -> impl Iterator<Item = &Edge> {
        self.in_edges
            .get(&id)
            .into_iter()
            .flatten()
            .map(move |&i| &self.edges[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_id_is_stable() {
        let a = SymbolId::compute(Language::Rust, "src/a.rs", "foo", SymbolKind::Function, 10);
        let b = SymbolId::compute(Language::Rust, "src/a.rs", "foo", SymbolKind::Function, 10);
        assert_eq!(a, b);
    }

    #[test]
    fn symbol_id_differs_on_line() {
        let a = SymbolId::compute(Language::Rust, "src/a.rs", "foo", SymbolKind::Function, 10);
        let b = SymbolId::compute(Language::Rust, "src/a.rs", "foo", SymbolKind::Function, 11);
        assert_ne!(a, b);
    }

    #[test]
    fn language_from_path() {
        assert_eq!(Language::from_path(Path::new("a.rs")), Some(Language::Rust));
        assert_eq!(
            Language::from_path(Path::new("a.tsx")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(Path::new("a.py")),
            Some(Language::Python)
        );
        assert_eq!(Language::from_path(Path::new("a.txt")), None);
    }

    #[test]
    fn upsert_and_remove_file() {
        let mut g = CodeGraph::new();
        let id = SymbolId::compute(Language::Rust, "a.rs", "foo", SymbolKind::Function, 1);
        let sym = Symbol {
            id,
            name: "foo".into(),
            kind: SymbolKind::Function,
            signature: "fn foo()".into(),
            language: Language::Rust,
            file: "a.rs".into(),
            span: Span {
                line_start: 1,
                line_end: 2,
                byte_start: 0,
                byte_end: 10,
            },
            container: None,
        };
        g.upsert_file(SourceFile {
            path: "a.rs".into(),
            language: Language::Rust,
            content_hash: 1,
            symbols: vec![sym],
            edges: vec![],
        });
        g.reindex();
        assert_eq!(g.symbol_count(), 1);
        assert_eq!(g.by_name("foo"), &[id]);
        g.remove_file("a.rs");
        g.reindex();
        assert_eq!(g.symbol_count(), 0);
    }
}
