//! Symbol Extraction context (ADR-0003, ADR-0004).
//!
//! Runs per-language tree-sitter queries (`queries/*.scm`) over a parse tree to
//! produce [`Symbol`]s and (initially unresolved) [`Edge`]s. Cross-file
//! resolution happens later in [`crate::resolve`] once the whole graph exists.

use crate::domain::{
    Confidence, Edge, EdgeKind, Language, SourceFile, Span, Symbol, SymbolId, SymbolKind,
};
use crate::parser;
use std::collections::HashMap;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

const RUST_Q: &str = include_str!("../../queries/rust.scm");
const PYTHON_Q: &str = include_str!("../../queries/python.scm");
const JS_Q: &str = include_str!("../../queries/javascript.scm");
const TS_Q: &str = include_str!("../../queries/typescript.scm");
const GO_Q: &str = include_str!("../../queries/go.scm");

fn query_src(lang: Language) -> &'static str {
    match lang {
        Language::Rust => RUST_Q,
        Language::Python => PYTHON_Q,
        Language::JavaScript => JS_Q,
        Language::TypeScript => TS_Q,
        Language::Go => GO_Q,
    }
}

/// Compiled query cache, keyed by language. Compiled once per process.
fn compiled_query(lang: Language) -> &'static Query {
    static CACHE: OnceLock<HashMap<Language, Query>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let mut m = HashMap::new();
        for &l in &[
            Language::Rust,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Go,
        ] {
            let q = Query::new(&parser::ts_language(l), query_src(l))
                .unwrap_or_else(|e| panic!("invalid query for {l}: {e}"));
            m.insert(l, q);
        }
        m
    });
    map.get(&lang).expect("query compiled for language")
}

/// Map a `@def.<kind>` capture name to a [`SymbolKind`].
fn kind_for_capture(name: &str) -> Option<SymbolKind> {
    let kind = name.strip_prefix("def.")?;
    Some(match kind {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "class" => SymbolKind::Class,
        "module" => SymbolKind::Module,
        "type" => SymbolKind::Type,
        "constant" => SymbolKind::Constant,
        "field" => SymbolKind::Field,
        _ => return None,
    })
}

/// A definition discovered in the first pass, before containers are linked.
struct DefRecord {
    id: SymbolId,
    kind: SymbolKind,
    name: String,
    signature: String,
    span: Span,
    byte_start: usize,
    byte_end: usize,
}

fn span_of(node: Node) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        line_start: s.row as u32 + 1,
        line_end: e.row as u32 + 1,
        byte_start: node.start_byte() as u32,
        byte_end: node.end_byte() as u32,
    }
}

/// A compact one-line signature: the declaration text up to the body opener or
/// the first newline, whichever comes first.
fn signature_of(node: Node, source: &str) -> String {
    let text = &source[node.start_byte()..node.end_byte()];
    let cut = text
        .find('{')
        .or_else(|| text.find(':'))
        .map(|i| i.min(text.find('\n').unwrap_or(usize::MAX)))
        .unwrap_or_else(|| text.find('\n').unwrap_or(text.len()));
    text[..cut.min(text.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find the smallest definition strictly containing `[start, end)`, excluding an
/// identical range. Returns the index into `defs`.
fn enclosing(defs: &[DefRecord], start: usize, end: usize, exclude_self: bool) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_len = usize::MAX;
    for (i, d) in defs.iter().enumerate() {
        let contains = d.byte_start <= start && d.byte_end >= end;
        let is_self = exclude_self && d.byte_start == start && d.byte_end == end;
        if contains && !is_self {
            let len = d.byte_end - d.byte_start;
            if len < best_len {
                best_len = len;
                best = Some(i);
            }
        }
    }
    best
}

/// Extract all symbols and edges from one source file.
pub fn extract(lang: Language, rel_path: &str, source: &str, content_hash: u64) -> SourceFile {
    let empty = || SourceFile {
        path: rel_path.to_string(),
        language: lang,
        content_hash,
        symbols: Vec::new(),
        edges: Vec::new(),
    };
    let Some(tree) = parser::parse(lang, source) else {
        return empty();
    };
    let query = compiled_query(lang);
    let root = tree.root_node();

    // A synthetic module symbol representing the file itself; used as the
    // fallback container/source for top-level edges.
    let module_name = rel_path
        .rsplit('/')
        .next()
        .and_then(|f| f.split('.').next())
        .unwrap_or(rel_path)
        .to_string();
    let module_id = SymbolId::compute(lang, rel_path, &module_name, SymbolKind::Module, 0);

    // ---- Pass 1: collect definitions + raw edge sites ----
    let mut defs: Vec<DefRecord> = Vec::new();
    struct EdgeSite {
        kind: EdgeKind,
        to_name: String,
        node_start: usize,
        node_end: usize,
        line: u32,
    }
    let mut sites: Vec<EdgeSite> = Vec::new();

    let cap_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, root, source.as_bytes());
    while let Some(m) = it.next() {
        // Within a match, find the @name node and the @def.* / @call / @import.
        let mut name_node: Option<Node> = None;
        let mut def_kind: Option<SymbolKind> = None;
        let mut def_node: Option<Node> = None;
        let mut call_node: Option<Node> = None;
        let mut import_node: Option<Node> = None;
        for cap in m.captures {
            let cname = cap_names[cap.index as usize];
            if cname == "name" {
                name_node = Some(cap.node);
            } else if let Some(k) = kind_for_capture(cname) {
                def_kind = Some(k);
                def_node = Some(cap.node);
            } else if cname == "call" {
                call_node = Some(cap.node);
            } else if cname == "import" {
                import_node = Some(cap.node);
            }
        }

        if let (Some(kind), Some(dnode)) = (def_kind, def_node) {
            let name = name_node
                .map(|n| source[n.start_byte()..n.end_byte()].to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let span = span_of(dnode);
            let id = SymbolId::compute(lang, rel_path, &name, kind, span.line_start);
            defs.push(DefRecord {
                id,
                kind,
                name,
                signature: signature_of(dnode, source),
                span,
                byte_start: dnode.start_byte(),
                byte_end: dnode.end_byte(),
            });
        }

        if let Some(cnode) = call_node {
            let to_name = source[cnode.start_byte()..cnode.end_byte()].to_string();
            sites.push(EdgeSite {
                kind: EdgeKind::Calls,
                to_name,
                node_start: cnode.start_byte(),
                node_end: cnode.end_byte(),
                line: cnode.start_position().row as u32 + 1,
            });
        }
        if let Some(inode) = import_node {
            let raw = source[inode.start_byte()..inode.end_byte()].to_string();
            let to_name = raw
                .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                .to_string();
            sites.push(EdgeSite {
                kind: EdgeKind::Imports,
                to_name,
                node_start: inode.start_byte(),
                node_end: inode.end_byte(),
                line: inode.start_position().row as u32 + 1,
            });
        }
    }

    // ---- Build symbols with containers ----
    let mut symbols: Vec<Symbol> = Vec::with_capacity(defs.len() + 1);
    // The module symbol spans the whole file.
    let file_span = span_of(root);
    symbols.push(Symbol {
        id: module_id,
        name: module_name,
        kind: SymbolKind::Module,
        signature: String::new(),
        language: lang,
        file: rel_path.to_string(),
        span: file_span,
        container: None,
    });

    let mut edges: Vec<Edge> = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        let container = enclosing(&defs, d.byte_start, d.byte_end, true)
            .map(|j| defs[j].id)
            .unwrap_or(module_id);
        symbols.push(Symbol {
            id: d.id,
            name: d.name.clone(),
            kind: d.kind,
            signature: d.signature.clone(),
            language: lang,
            file: rel_path.to_string(),
            span: d.span,
            container: Some(container),
        });
        // Contains edge from container to this symbol.
        edges.push(Edge {
            kind: EdgeKind::Contains,
            from: container,
            to_name: d.name.clone(),
            to: Some(d.id),
            confidence: Confidence::Precise,
            line: d.span.line_start,
        });
        let _ = i;
    }

    // ---- Attribute edge sites to their enclosing definition ----
    for site in sites {
        let from = enclosing(&defs, site.node_start, site.node_end, false)
            .map(|j| defs[j].id)
            .unwrap_or(module_id);
        edges.push(Edge {
            kind: site.kind,
            from,
            to_name: site.to_name,
            to: None,
            confidence: Confidence::Heuristic,
            line: site.line,
        });
    }

    SourceFile {
        path: rel_path.to_string(),
        language: lang,
        content_hash,
        symbols,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_function_and_call() {
        let src = "fn helper() {}\nfn main() {\n    helper();\n}\n";
        let f = extract(Language::Rust, "a.rs", src, 0);
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"main"));
        // A call edge helper() from inside main.
        assert!(f
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.to_name == "helper"));
    }

    #[test]
    fn extracts_python_def() {
        let src = "def foo():\n    bar()\n\ndef bar():\n    pass\n";
        let f = extract(Language::Python, "a.py", src, 0);
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
        assert!(f
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.to_name == "bar"));
    }

    #[test]
    fn extracts_go_struct_and_func() {
        let src = "package main\ntype T struct{}\nfunc F() {}\n";
        let f = extract(Language::Go, "a.go", src, 0);
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"T"));
        assert!(names.contains(&"F"));
    }
}
