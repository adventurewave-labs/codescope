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
const JAVA_Q: &str = include_str!("../../queries/java.scm");
const C_Q: &str = include_str!("../../queries/c.scm");
const CPP_Q: &str = include_str!("../../queries/cpp.scm");
const CSHARP_Q: &str = include_str!("../../queries/csharp.scm");
const RUBY_Q: &str = include_str!("../../queries/ruby.scm");

fn query_src(lang: Language) -> &'static str {
    match lang {
        Language::Rust => RUST_Q,
        Language::Python => PYTHON_Q,
        Language::JavaScript => JS_Q,
        Language::TypeScript => TS_Q,
        Language::Go => GO_Q,
        Language::Java => JAVA_Q,
        Language::C => C_Q,
        Language::Cpp => CPP_Q,
        Language::CSharp => CSHARP_Q,
        Language::Ruby => RUBY_Q,
    }
}

/// Fingerprint of every extraction query. Part of the index-compatibility key
/// so editing a `queries/*.scm` file re-extracts unchanged sources (ADR-0005).
pub fn rules_fingerprint() -> u64 {
    let mut all = String::new();
    for l in Language::ALL {
        all.push_str(l.name());
        all.push_str(query_src(l));
    }
    seahash::hash(all.as_bytes())
}

/// Compiled query cache, keyed by language. Each language compiles lazily and
/// independently: a grammar/query mismatch in one language disables only that
/// language (logged once) instead of taking the whole indexer down.
fn compiled_query(lang: Language) -> Option<&'static Query> {
    static CACHE: OnceLock<HashMap<Language, OnceLock<Option<Query>>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        Language::ALL
            .iter()
            .map(|&l| (l, OnceLock::new()))
            .collect()
    });
    map.get(&lang)?
        .get_or_init(
            || match Query::new(&parser::ts_language(lang), query_src(lang)) {
                Ok(q) => Some(q),
                Err(e) => {
                    tracing::error!("codescope: disabling {lang}: invalid query/grammar: {e}");
                    eprintln!("codescope: disabling {lang}: invalid query/grammar: {e}");
                    None
                }
            },
        )
        .as_ref()
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
    /// Owner type discovered syntactically at capture time (Go receivers).
    owner: Option<String>,
    doc: Option<String>,
}

fn is_comment(kind: &str) -> bool {
    kind.contains("comment")
}

/// Strip comment markers and collapse whitespace.
fn clean_doc(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let l = line.trim();
        let l = l
            .trim_start_matches("/**")
            .trim_start_matches("/*!")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start_matches("//")
            .trim_start_matches('*')
            .trim_start_matches('#')
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        // Drop doc-tool noise lines (`@param`, `<summary>` tags).
        let l = l
            .trim_start_matches("<summary>")
            .trim_end_matches("</summary>")
            .trim();
        if l.is_empty() || l.starts_with("@") && l.len() < 3 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(l);
    }
    let out: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > crate::domain::DOC_MAX_CHARS {
        let mut t: String = out.chars().take(crate::domain::DOC_MAX_CHARS - 1).collect();
        t.push('…');
        t
    } else {
        out
    }
}

/// File-level documentation: Rust `//!` / `/*!` inner docs, a Python module
/// docstring, or the comment block before a Go `package` clause.
fn module_doc(root: Node, source: &str, lang: Language) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let text = &source[child.start_byte()..child.end_byte()];
        match (lang, child.kind()) {
            (Language::Rust, k) if is_comment(k) => {
                if text.starts_with("//!") || text.starts_with("/*!") {
                    parts.push(text);
                } else if !parts.is_empty() {
                    break;
                }
            }
            (Language::Python, "expression_statement") if parts.is_empty() => {
                if let Some(st) = child.named_child(0).filter(|n| n.kind() == "string") {
                    parts.push(&source[st.start_byte()..st.end_byte()]);
                }
                break;
            }
            (Language::Go, k) if is_comment(k) => parts.push(text),
            (Language::Go, "package_clause") => break,
            (_, k) if is_comment(k) && lang != Language::Python => {}
            _ => break,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let d = clean_doc(&parts.join("\n"));
    (!d.is_empty()).then_some(d)
}

/// The leading doc comment of a definition: contiguous comment siblings just
/// above it (skipping Rust attributes / decorators), or a Python docstring.
fn doc_of(def: Node, source: &str, lang: Language) -> Option<String> {
    // Python docstring: first statement of the body is a string literal.
    if lang == Language::Python {
        if let Some(body) = def.child_by_field_name("body") {
            if let Some(first) = body.named_child(0) {
                if first.kind() == "expression_statement" {
                    if let Some(st) = first.named_child(0).filter(|n| n.kind() == "string") {
                        let d = clean_doc(&source[st.start_byte()..st.end_byte()]);
                        return (!d.is_empty()).then_some(d);
                    }
                }
            }
        }
    }
    // Comments attach to the outermost wrapper (export / decorated / template).
    let mut anchor = def;
    while let Some(p) = anchor.parent() {
        if matches!(
            p.kind(),
            "export_statement" | "decorated_definition" | "template_declaration"
        ) {
            anchor = p;
        } else {
            break;
        }
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = anchor;
    let mut expect_row = anchor.start_position().row;
    while let Some(prev) = cur.prev_sibling() {
        let kind = prev.kind();
        if kind == "attribute_item" || kind == "decorator" || kind == "annotation" {
            expect_row = prev.start_position().row;
            cur = prev;
            continue;
        }
        if !is_comment(kind) || prev.end_position().row + 1 < expect_row {
            break;
        }
        parts.push(&source[prev.start_byte()..prev.end_byte()]);
        expect_row = prev.start_position().row;
        cur = prev;
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    let d = clean_doc(&parts.join("\n"));
    (!d.is_empty()).then_some(d)
}

/// A Rust `impl [Trait for] Type { … }` block: not a symbol itself, but the
/// scope that turns the functions inside into methods of `Type`.
struct ImplScope {
    byte_start: usize,
    byte_end: usize,
    ty: String,
    trait_: Option<String>,
}

/// Reduce a type expression to its bare name: `crate::a::Store<T>` → `Store`,
/// `*Server` → `Server`, `&'a mut Foo` → `Foo`.
fn bare_type(text: &str) -> String {
    let t = text.split(['<', '[', '(']).next().unwrap_or(text);
    let t = t.rsplit("::").next().unwrap_or(t);
    let t = t.rsplit('.').next().unwrap_or(t);
    t.split_whitespace()
        .last()
        .unwrap_or("")
        .trim_start_matches(['*', '&'])
        .trim_start_matches("mut ")
        .to_string()
}

/// Go: `(s *Server)` → `Server`.
fn go_receiver_type(receiver: &str) -> Option<String> {
    let inner = receiver
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');
    let ty = inner.split_whitespace().last()?;
    let t = bare_type(ty);
    (!t.is_empty()).then_some(t)
}

/// The receiver / path qualifier of a call site, from the callee node's parent:
/// `self.foo()` → `self`, `Store::open()` → `Store`, `a.b.c()` → `b`.
fn call_qualifier(callee: Node, source: &str) -> Option<String> {
    let parent = callee.parent()?;
    let fields: &[&str] = match parent.kind() {
        // Rust uses `value`, C/C++ use `argument`.
        "field_expression" => &["value", "argument"],
        "scoped_identifier" => &["path"],
        "qualified_identifier" => &["scope"],
        "member_expression" | "attribute" | "method_invocation" => &["object"],
        "member_access_expression" => &["expression"],
        "selector_expression" => &["operand"],
        "call" => &["receiver"],
        _ => return None,
    };
    let q = fields.iter().find_map(|f| parent.child_by_field_name(f))?;
    if q.id() == callee.id() {
        return None;
    }
    let text = &source[q.start_byte()..q.end_byte()];
    let last = text
        .rsplit("::")
        .next()
        .and_then(|t| t.rsplit('.').next())
        .unwrap_or(text)
        .trim_end_matches(')')
        .trim_end_matches('(');
    let last = last.split('<').next().unwrap_or(last).trim();
    (!last.is_empty()).then(|| last.to_string())
}

/// Lightweight local type inference for a call receiver (ADR-0018).
///
/// Scans the enclosing definition's text for the variable's declaration and
/// returns its type name when it is evident syntactically:
/// * annotations / params — `q: Type`, `q: &mut Type`, Go `(q *Type`, `, q Type`
/// * constructors — `q = Type::new(…)`, `q = Type(…)`, `q = new Type(…)`,
///   `q := &Type{…}`, Go `q := NewType(…)`
///
/// Only capitalized results are accepted (a type, not a variable/function).
pub(crate) fn infer_receiver_type(q: &str, scope: &str) -> Option<String> {
    let bytes = scope.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(off) = scope[from..].find(q) {
        let i = from + off;
        from = i + q.len();
        let before_ok = i == 0 || !is_ident(bytes[i - 1]);
        let end = i + q.len();
        let after_ok = end >= bytes.len() || !is_ident(bytes[end]);
        if !before_ok || !after_ok {
            continue;
        }
        let rest = scope[end..].trim_start();
        let prev = scope[..i].trim_end();
        let candidate = if let Some(r) = rest.strip_prefix(":=") {
            constructor_type(r)
        } else if rest.starts_with("::") {
            None
        } else if let Some(r) = rest.strip_prefix(':') {
            annotation_type(r)
        } else if rest.starts_with("==") || rest.starts_with("=>") {
            None
        } else if let Some(r) = rest.strip_prefix('=') {
            constructor_type(r)
        } else if prev.ends_with('(') || prev.ends_with(',') {
            // Go-style `(q *Type` / `, q Type` parameter.
            annotation_type(rest)
        } else {
            None
        };
        let accept =
            |t: &str| t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && t != "Self";
        if let Some(t) = candidate.filter(|t| accept(t)) {
            return Some(t);
        }
        // Type-before-name declarations (Java/C#/C/C++): `Store store`,
        // `final Repo r =`, `(Foo *f,`, `List<Foo> xs`.
        if rest.starts_with(['=', ';', ',', ')', ':']) || rest.is_empty() {
            if let Some(t) = type_before(prev).filter(|t| accept(t)) {
                return Some(t);
            }
        }
    }
    None
}

/// The type token immediately preceding a declared name, if any.
fn type_before(prev: &str) -> Option<String> {
    let prev = prev.trim_end_matches(['*', '&', ' ']);
    // Skip one generic argument list: `List<Foo>` → `List`.
    let prev = if prev.ends_with('>') {
        let mut depth = 0i32;
        let mut cut = None;
        for (i, ch) in prev.char_indices().rev() {
            match ch {
                '>' => depth += 1,
                '<' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        &prev[..cut?]
    } else {
        prev
    };
    let tok: String = prev
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':' || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let last = tok.rsplit("::").next()?.rsplit('.').next()?;
    const KEYWORDS: &[&str] = &[
        "return", "new", "await", "throw", "yield", "case", "in", "of",
    ];
    (!last.is_empty() && !KEYWORDS.contains(&last)).then(|| last.to_string())
}

fn annotation_type(r: &str) -> Option<String> {
    let mut r = r.trim_start();
    loop {
        let t = r
            .trim_start_matches(['&', '*'])
            .trim_start_matches("mut ")
            .trim_start_matches("dyn ")
            .trim_start_matches("impl ")
            .trim_start();
        let t = if t.starts_with('\'') {
            t.split_once(' ').map(|x| x.1).unwrap_or("")
        } else {
            t
        };
        if t.len() == r.len() {
            break;
        }
        r = t;
    }
    let path: String = r
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':' || *c == '.')
        .collect();
    let last = path.rsplit("::").next()?.rsplit('.').next()?;
    (!last.is_empty()).then(|| last.to_string())
}

fn constructor_type(r: &str) -> Option<String> {
    let r = r.trim_start().trim_start_matches('&').trim_start();
    let r = r.strip_prefix("new ").unwrap_or(r).trim_start();
    let r = r.strip_prefix("await ").unwrap_or(r);
    let path: String = r
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':' || *c == '.')
        .collect();
    let segs: Vec<&str> = path
        .split("::")
        .flat_map(|s| s.split('.'))
        .filter(|s| !s.is_empty())
        .collect();
    let last = *segs.last()?;
    // `Type::new(` → Type; `Type(` / `Type{` → Type; Go `NewType(` → Type.
    let ty = if last.chars().next()?.is_ascii_lowercase() && segs.len() >= 2 {
        segs[segs.len() - 2]
    } else if let Some(t) = last
        .strip_prefix("New")
        .filter(|t| t.starts_with(|c: char| c.is_ascii_uppercase()))
    {
        t
    } else {
        last
    };
    Some(ty.to_string())
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
    let Some(query) = compiled_query(lang) else {
        return empty();
    };
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
        qualifier: Option<String>,
        to_name: String,
        node_start: usize,
        node_end: usize,
        line: u32,
    }
    let mut sites: Vec<EdgeSite> = Vec::new();
    let mut impls: Vec<ImplScope> = Vec::new();

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
            if cname == "scope.impl" {
                let n = cap.node;
                if let Some(ty) = n.child_by_field_name("type") {
                    let ty = bare_type(&source[ty.start_byte()..ty.end_byte()]);
                    let trait_ = n
                        .child_by_field_name("trait")
                        .map(|t| bare_type(&source[t.start_byte()..t.end_byte()]));
                    impls.push(ImplScope {
                        byte_start: n.start_byte(),
                        byte_end: n.end_byte(),
                        ty,
                        trait_,
                    });
                }
                continue;
            }
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
            let owner = if dnode.kind() == "method_declaration" && lang == Language::Go {
                dnode
                    .child_by_field_name("receiver")
                    .and_then(|r| go_receiver_type(&source[r.start_byte()..r.end_byte()]))
            } else {
                // C++ out-of-line definition: `void Foo::bar() {}`.
                name_node
                    .and_then(|n| n.parent())
                    .filter(|p| p.kind() == "qualified_identifier")
                    .and_then(|p| p.child_by_field_name("scope"))
                    .map(|scope| bare_type(&source[scope.start_byte()..scope.end_byte()]))
            };
            defs.push(DefRecord {
                id: SymbolId(0), // assigned once the final kind is known
                kind,
                name,
                signature: signature_of(dnode, source),
                span,
                byte_start: dnode.start_byte(),
                byte_end: dnode.end_byte(),
                owner,
                doc: doc_of(dnode, source, lang),
            });
        }

        if let Some(cnode) = call_node {
            let mut to_name = source[cnode.start_byte()..cnode.end_byte()].to_string();
            let mut qualifier = call_qualifier(cnode, source);
            // Ruby `Repo.new` instantiates Repo.
            if lang == Language::Ruby
                && to_name == "new"
                && qualifier
                    .as_deref()
                    .is_some_and(|q| q.starts_with(|c: char| c.is_ascii_uppercase()))
            {
                to_name = qualifier.take().expect("checked");
            }
            sites.push(EdgeSite {
                kind: EdgeKind::Calls,
                qualifier,
                to_name,
                node_start: cnode.start_byte(),
                node_end: cnode.end_byte(),
                line: cnode.start_position().row as u32 + 1,
            });
        }
        if let Some(inode) = import_node {
            let raw = source[inode.start_byte()..inode.end_byte()].to_string();
            let to_name = raw
                .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '<' | '>'))
                .to_string();
            sites.push(EdgeSite {
                kind: EdgeKind::Imports,
                qualifier: None,
                to_name,
                node_start: inode.start_byte(),
                node_end: inode.end_byte(),
                line: inode.start_position().row as u32 + 1,
            });
        }
    }

    // ---- Classify methods and assign owners ----
    // Functions inside a Rust `impl`, or lexically inside a class/struct/trait/
    // interface, are methods of that owner (ADR-0018).
    let mut trait_of: Vec<Option<String>> = vec![None; defs.len()];
    for i in 0..defs.len() {
        if !defs[i].kind.is_callable() {
            continue;
        }
        let (bs, be) = (defs[i].byte_start, defs[i].byte_end);
        let imp = impls
            .iter()
            .filter(|im| im.byte_start <= bs && im.byte_end >= be)
            .min_by_key(|im| im.byte_end - im.byte_start);
        let encl = enclosing(&defs, bs, be, true).filter(|&j| {
            matches!(
                defs[j].kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Trait | SymbolKind::Interface
            ) || (lang == Language::Ruby && defs[j].kind == SymbolKind::Module)
        });
        // Innermost wins between an impl block and an enclosing type def.
        let from_impl = match (imp, encl) {
            (Some(im), Some(j)) => {
                (im.byte_end - im.byte_start) < (defs[j].byte_end - defs[j].byte_start)
            }
            (Some(_), None) => true,
            _ => false,
        };
        if from_impl {
            let im = imp.expect("checked");
            defs[i].owner = Some(im.ty.clone());
            trait_of[i] = im.trait_.clone();
        } else if let Some(j) = encl {
            defs[i].owner = Some(defs[j].name.clone());
        }
        if defs[i].owner.is_some() {
            defs[i].kind = SymbolKind::Method;
        }
    }
    for d in defs.iter_mut() {
        d.id = SymbolId::compute(lang, rel_path, &d.name, d.kind, d.span.line_start);
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
        owner: None,
        doc: module_doc(root, source, lang),
    });

    let mut edges: Vec<Edge> = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        // Impl methods hang off their type when it is declared in this file.
        let owner_def = d.owner.as_ref().and_then(|o| {
            defs.iter().find(|t| {
                &t.name == o
                    && matches!(
                        t.kind,
                        SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::Trait
                            | SymbolKind::Class
                            | SymbolKind::Interface
                            | SymbolKind::Type
                    )
            })
        });
        let container = enclosing(&defs, d.byte_start, d.byte_end, true)
            .map(|j| defs[j].id)
            .or(owner_def.map(|t| t.id))
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
            owner: d.owner.clone(),
            doc: d.doc.clone(),
        });
        // Contains edge from container to this symbol.
        edges.push(Edge {
            kind: EdgeKind::Contains,
            from: container,
            to_name: d.name.clone(),
            to: Some(d.id),
            confidence: Confidence::Precise,
            qualifier: None,
            alternatives: 0,
            line: d.span.line_start,
        });
        // `impl Trait for Type { fn m() }` → m Defines Trait::m.
        if let Some(tr) = &trait_of[i] {
            edges.push(Edge {
                kind: EdgeKind::Defines,
                from: d.id,
                to_name: format!("{tr}::{}", d.name),
                to: None,
                confidence: Confidence::Heuristic,
                qualifier: Some(tr.clone()),
                alternatives: 0,
                line: d.span.line_start,
            });
        }
    }

    // ---- Attribute edge sites to their enclosing definition ----
    let mut type_cache: HashMap<(usize, String), Option<String>> = HashMap::new();
    for mut site in sites {
        let encl = enclosing(&defs, site.node_start, site.node_end, false);
        let from = encl.map(|j| defs[j].id).unwrap_or(module_id);
        // Replace a variable receiver with its inferred type when evident.
        if let (Some(j), Some(q)) = (encl, site.qualifier.clone()) {
            let lower = q.starts_with(|c: char| c.is_ascii_lowercase() || c == '_');
            if lower && !matches!(q.as_str(), "self" | "this" | "cls" | "super") {
                let t = type_cache
                    .entry((j, q.clone()))
                    .or_insert_with(|| {
                        // Innermost definition first, then its containers
                        // (a method's class body declares its fields).
                        let mut k = Some(j);
                        for _ in 0..3 {
                            let Some(cur) = k else { break };
                            let d = &defs[cur];
                            if let Some(t) =
                                infer_receiver_type(&q, &source[d.byte_start..d.byte_end])
                            {
                                return Some(t);
                            }
                            k = enclosing(&defs, d.byte_start, d.byte_end, true);
                        }
                        None
                    })
                    .clone();
                if let Some(t) = t {
                    site.qualifier = Some(t);
                }
            }
        }
        edges.push(Edge {
            kind: site.kind,
            from,
            to_name: site.to_name,
            to: None,
            confidence: Confidence::Heuristic,
            qualifier: site.qualifier,
            alternatives: 0,
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
    fn infers_receiver_types() {
        let t = |q: &str, src: &str| infer_receiver_type(q, src);
        assert_eq!(
            t("store", "fn f(store: &mut Store) {}").as_deref(),
            Some("Store")
        );
        assert_eq!(
            t("g", "fn f(g: &'a CodeGraph) {}").as_deref(),
            Some("CodeGraph")
        );
        assert_eq!(
            t("b", "let mut b = Budget::new(5);").as_deref(),
            Some("Budget")
        );
        assert_eq!(t("r", "r = Repo(path)").as_deref(), Some("Repo"));
        assert_eq!(
            t("c", "const c = new Client({});").as_deref(),
            Some("Client")
        );
        assert_eq!(t("s", "s := &Server{}").as_deref(), Some("Server"));
        assert_eq!(t("s", "s := NewServer(cfg)").as_deref(), Some("Server"));
        assert_eq!(
            t("s", "func (s *Server) Start() {}").as_deref(),
            Some("Server")
        );
        assert_eq!(t("v", "let v = Vec::new();").as_deref(), Some("Vec"));
        assert_eq!(t("x", "if x == y {}"), None);
        assert_eq!(t("n", "let n = compute();"), None);
        assert_eq!(t("items", "items.push(1)"), None);
        assert_eq!(
            t("store", "void f(Store store) {}").as_deref(),
            Some("Store")
        );
        assert_eq!(
            t("r", "final Repo r = factory.make();").as_deref(),
            Some("Repo")
        );
        assert_eq!(t("xs", "List<Foo> xs;").as_deref(), Some("List"));
        assert_eq!(
            t("xs", "List<Foo> xs = new ArrayList<>();").as_deref(),
            Some("ArrayList")
        );
        assert_eq!(t("w", "Widget *w;").as_deref(), Some("Widget"));
        assert_eq!(t("x", "return x;"), None);
        assert_eq!(t("u", "u = User.new(name)").as_deref(), Some("User"));
    }

    fn by_name<'a>(f: &'a SourceFile, name: &str) -> &'a Symbol {
        // Skip the synthetic per-file module (the only container-less symbol).
        f.symbols
            .iter()
            .find(|s| s.name == name && s.container.is_some())
            .unwrap_or_else(|| {
                panic!(
                    "no symbol {name}: {:?}",
                    f.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
                )
            })
    }

    fn call(f: &SourceFile, name: &str) -> Option<Option<String>> {
        f.edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == name)
            .map(|e| e.qualifier.clone())
    }

    fn imports(f: &SourceFile) -> Vec<&str> {
        f.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .map(|e| e.to_name.as_str())
            .collect()
    }

    #[test]
    fn extracts_java() {
        let src = "package app;\nimport java.util.List;\nimport app.store.Repo;\n\npublic class Service {\n    private Repo repo;\n    public Service(Repo r) { this.repo = r; }\n    public void run(Repo r) {\n        r.save();\n        helper();\n        new Widget();\n    }\n    void helper() {}\n}\ninterface Shape { double area(); }\nenum Color { RED }\nrecord Point(int x, int y) {}\n";
        let f = extract(Language::Java, "Service.java", src, 0);
        let run = by_name(&f, "run");
        assert_eq!(
            (run.kind, run.owner.as_deref()),
            (SymbolKind::Method, Some("Service"))
        );
        assert_eq!(by_name(&f, "Shape").kind, SymbolKind::Interface);
        assert_eq!(by_name(&f, "Color").kind, SymbolKind::Enum);
        assert_eq!(by_name(&f, "Point").kind, SymbolKind::Class);
        assert_eq!(
            call(&f, "save"),
            Some(Some("Repo".into())),
            "receiver type inferred from param"
        );
        assert_eq!(call(&f, "helper"), Some(None));
        assert!(call(&f, "Widget").is_some(), "constructor call");
        assert_eq!(imports(&f), vec!["java.util.List", "app.store.Repo"]);
    }

    #[test]
    fn extracts_c() {
        let src = "#include <stdio.h>\n#include \"list.h\"\nstruct node { int v; };\ntypedef struct node node_t;\nenum color { RED };\nstatic int helper(int x) { return x; }\nchar *name(void) { return 0; }\nint main(void) {\n    struct ops *o;\n    helper(1);\n    o->run();\n    printf(\"hi\");\n    return 0;\n}\n";
        let f = extract(Language::C, "main.c", src, 0);
        for (n, k) in [
            ("node", SymbolKind::Struct),
            ("node_t", SymbolKind::Type),
            ("color", SymbolKind::Enum),
            ("helper", SymbolKind::Function),
            ("name", SymbolKind::Function),
            ("main", SymbolKind::Function),
        ] {
            assert_eq!(by_name(&f, n).kind, k, "{n}");
        }
        assert_eq!(call(&f, "helper"), Some(None));
        assert_eq!(call(&f, "run"), Some(Some("o".into())));
        assert_eq!(imports(&f), vec!["stdio.h", "list.h"]);
    }

    #[test]
    fn extracts_cpp() {
        let src = "#include <vector>\nnamespace app {\nclass Engine {\npublic:\n    void start() { tick(); }\n    void tick();\n};\nvoid Engine::tick() { util::log(); }\nstruct Point { int x; };\n}\nint main() {\n    app::Engine e;\n    e.start();\n    return 0;\n}\n";
        let f = extract(Language::Cpp, "engine.cpp", src, 0);
        assert_eq!(by_name(&f, "app").kind, SymbolKind::Module);
        assert_eq!(by_name(&f, "Engine").kind, SymbolKind::Class);
        let start = by_name(&f, "start");
        assert_eq!(
            (start.kind, start.owner.as_deref()),
            (SymbolKind::Method, Some("Engine"))
        );
        let tick = f.symbols.iter().find(|s| s.name == "tick").unwrap();
        assert_eq!(
            tick.owner.as_deref(),
            Some("Engine"),
            "out-of-line definition owned"
        );
        assert_eq!(call(&f, "log"), Some(Some("util".into())));
        assert_eq!(call(&f, "start"), Some(Some("Engine".into())));
        assert_eq!(imports(&f), vec!["vector"]);
    }

    #[test]
    fn extracts_csharp() {
        let src = "using System;\nusing App.Data;\nnamespace App.Core {\n    public class OrderService {\n        public OrderService(Repo repo) {}\n        public void Place(Repo repo) {\n            repo.Save();\n            Validate();\n            var o = new Order();\n        }\n        private void Validate() {}\n    }\n    public interface IRepo { void Save(); }\n    public struct Money {}\n    public enum Status { Open }\n}\n";
        let f = extract(Language::CSharp, "OrderService.cs", src, 0);
        let place = by_name(&f, "Place");
        assert_eq!(
            (place.kind, place.owner.as_deref()),
            (SymbolKind::Method, Some("OrderService"))
        );
        assert_eq!(by_name(&f, "IRepo").kind, SymbolKind::Interface);
        assert_eq!(by_name(&f, "Money").kind, SymbolKind::Struct);
        assert_eq!(by_name(&f, "Status").kind, SymbolKind::Enum);
        assert_eq!(call(&f, "Save"), Some(Some("Repo".into())));
        assert_eq!(call(&f, "Validate"), Some(None));
        assert!(call(&f, "Order").is_some());
        assert_eq!(imports(&f), vec!["System", "App.Data"]);
    }

    #[test]
    fn extracts_ruby() {
        let src = "require 'json'\nrequire_relative 'store/repo'\n\nmodule Billing\n  class Invoice\n    def total\n      items\n      items.sum\n      tax_for(1)\n    end\n\n    def self.build\n      r = Repo.new\n      r.save\n    end\n\n    def tax_for(x)\n      x\n    end\n  end\nend\n\ndef top_level\nend\n";
        let f = extract(Language::Ruby, "invoice.rb", src, 0);
        assert_eq!(by_name(&f, "Billing").kind, SymbolKind::Module);
        assert_eq!(by_name(&f, "Invoice").kind, SymbolKind::Class);
        let total = by_name(&f, "total");
        assert_eq!(
            (total.kind, total.owner.as_deref()),
            (SymbolKind::Method, Some("Invoice"))
        );
        assert_eq!(by_name(&f, "build").owner.as_deref(), Some("Invoice"));
        assert_eq!(by_name(&f, "top_level").kind, SymbolKind::Function);
        assert_eq!(call(&f, "tax_for"), Some(None));
        assert_eq!(call(&f, "items"), Some(None), "paren-less statement call");
        assert_eq!(call(&f, "save"), Some(Some("Repo".into())));
        assert_eq!(imports(&f), vec!["json", "store/repo"]);
    }

    #[test]
    fn extracts_doc_comments() {
        let rs = "/// Parse a unified diff.\n/// Returns ranges.\n#[inline]\npub fn parse() {}\n\n// unrelated\n\nfn bare() {}\n";
        let f = extract(Language::Rust, "a.rs", rs, 0);
        assert_eq!(
            by_name(&f, "parse").doc.as_deref(),
            Some("Parse a unified diff. Returns ranges.")
        );
        assert_eq!(
            by_name(&f, "bare").doc,
            None,
            "blank line breaks attachment"
        );

        let py = "def f():\n    \"\"\"Compute the blast radius.\"\"\"\n    pass\n";
        let f = extract(Language::Python, "a.py", py, 0);
        assert_eq!(
            by_name(&f, "f").doc.as_deref(),
            Some("Compute the blast radius.")
        );

        let ts = "/**\n * Load the user profile.\n */\nexport function load() {}\n";
        let f = extract(Language::TypeScript, "a.ts", ts, 0);
        assert_eq!(
            by_name(&f, "load").doc.as_deref(),
            Some("Load the user profile.")
        );

        let go = "package p\n// Start begins serving.\nfunc Start() {}\n";
        let f = extract(Language::Go, "a.go", go, 0);
        assert_eq!(
            by_name(&f, "Start").doc.as_deref(),
            Some("Start begins serving.")
        );

        let java =
            "class A {\n  /** Saves the order. */\n  @Override\n  public void save() {}\n}\n";
        let f = extract(Language::Java, "A.java", java, 0);
        assert_eq!(by_name(&f, "save").doc.as_deref(), Some("Saves the order."));
    }

    #[test]
    fn extracts_module_docs() {
        let f = extract(
            Language::Rust,
            "a.rs",
            "//! Change-impact analysis.\n//! Second line.\n\nfn x() {}\n",
            0,
        );
        let m = f.symbols.iter().find(|s| s.container.is_none()).unwrap();
        assert_eq!(
            m.doc.as_deref(),
            Some("Change-impact analysis. Second line.")
        );
        let f = extract(
            Language::Python,
            "a.py",
            "\"\"\"Billing helpers.\"\"\"\nimport os\n",
            0,
        );
        let m = f.symbols.iter().find(|s| s.container.is_none()).unwrap();
        assert_eq!(m.doc.as_deref(), Some("Billing helpers."));
        let f = extract(
            Language::Rust,
            "b.rs",
            "/// item doc, not module doc\nfn y() {}\n",
            0,
        );
        assert!(f
            .symbols
            .iter()
            .find(|s| s.container.is_none())
            .unwrap()
            .doc
            .is_none());
    }

    #[test]
    fn every_language_query_compiles() {
        for lang in Language::ALL {
            assert!(
                compiled_query(lang).is_some(),
                "{lang} query must compile against its grammar"
            );
        }
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
