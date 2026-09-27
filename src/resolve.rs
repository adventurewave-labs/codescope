//! Symbol Resolution (Tier 1, ADR-0004).
//!
//! After every file's symbols/edges are loaded into the [`CodeGraph`], we run a
//! graph-wide pass that binds each unresolved edge (`to == None`) to a concrete
//! [`SymbolId`] by name, using fast heuristics:
//!
//! * `Calls` edges resolve only to callable symbols (functions/methods).
//! * Same-file candidates are preferred over cross-file ones.
//! * Ties are broken deterministically (lowest id) and labeled `Heuristic`.
//!
//! Tier 2 (SCIP ingestion for compiler-accurate, `Precise` edges) is future
//! work; the data model already carries [`Confidence`] so it can slot in.

use crate::domain::{CodeGraph, Confidence, Edge, EdgeKind, Language, SymbolId, SymbolKind};
use rayon::prelude::*;
use std::collections::HashMap;

/// What the resolver needs to know about a candidate target.
struct Cand {
    id: SymbolId,
    kind: SymbolKind,
    file: String,
    /// File stem (`store` for `src/store.rs`) and parent dir (`store` for
    /// `pkg/store/x.go`) — module-qualifier matching.
    stem: String,
    dir: String,
    owner: Option<String>,
}

/// What the resolver needs to know about the edge's source.
struct Caller<'a> {
    file: &'a str,
    owner: Option<&'a str>,
    language: Language,
}

/// Languages where a bare `f()` inside a method may be an implicit
/// `this.f()` / `self.f()` call (ADR-0019).
fn implicit_receiver(lang: Language) -> bool {
    matches!(
        lang,
        Language::Java | Language::CSharp | Language::Cpp | Language::Ruby
    )
}

fn stem_and_dir(path: &str) -> (String, String) {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    // `mod.rs` / `__init__.py` / `index.ts` are named by their directory.
    let dir = parts.next().unwrap_or("").to_string();
    let stem = if matches!(stem, "mod" | "__init__" | "index" | "lib") && !dir.is_empty() {
        dir.clone()
    } else {
        stem.to_string()
    };
    (stem, dir)
}

fn is_self_qualifier(q: &str) -> bool {
    matches!(q, "self" | "Self" | "this" | "cls" | "super")
}

/// Drop every heuristic binding and resolve the whole graph again. Used after
/// an incremental patch: a changed file can invalidate bindings anywhere
/// (a removed target, a new better candidate), and re-resolving in memory is
/// far cheaper than re-decoding the store (ADR-0020).
pub fn re_resolve(graph: &mut CodeGraph) {
    for e in graph.edges_mut() {
        if e.kind != EdgeKind::Contains && e.confidence == Confidence::Heuristic {
            e.to = None;
            e.alternatives = 0;
        }
    }
    resolve(graph);
}

/// What an incremental patch touched: bindings outside this scope cannot
/// change, because a binding depends only on (a) the caller's own file (its
/// imports, owner, path) and (b) the candidate set for the called *name*.
pub struct Scope {
    /// Files re-extracted or removed.
    pub files: std::collections::HashSet<String>,
    /// Names of every symbol removed from or added to the graph.
    pub names: std::collections::HashSet<String>,
}

fn last_seg(name: &str) -> &str {
    name.rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(name)
}

/// Incremental counterpart of [`re_resolve`]: unbind and re-resolve only
/// edges whose binding could have changed (ADR-0020). Equivalent to a full
/// re-resolve; much cheaper when little changed.
pub fn re_resolve_scoped(graph: &mut CodeGraph, scope: &Scope) {
    // Edges from touched files are freshly extracted (already unbound).
    for e in graph.edges_mut() {
        if e.kind != EdgeKind::Contains
            && e.confidence == Confidence::Heuristic
            && e.to.is_some()
            && scope.names.contains(last_seg(&e.to_name))
        {
            e.to = None;
            e.alternatives = 0;
        }
    }
    resolve_in(graph, Some(scope));
}

/// Resolve all unresolved edges in place and rebuild indexes.
pub fn resolve(graph: &mut CodeGraph) {
    resolve_in(graph, None);
}

fn resolve_in(graph: &mut CodeGraph, scope: Option<&Scope>) {
    // Snapshot the lookups we need (immutable borrow) before mutating edges.
    let mut name_index: HashMap<&str, Vec<Cand>> = HashMap::new();
    for s in graph.symbols() {
        let (stem, dir) = stem_and_dir(&s.file);
        name_index.entry(s.name.as_str()).or_default().push(Cand {
            id: s.id,
            kind: s.kind,
            file: s.file.clone(),
            stem,
            dir,
            owner: s.owner.clone(),
        });
    }
    // Deterministic candidate order → deterministic tie-breaking.
    for v in name_index.values_mut() {
        v.sort_by_key(|c| c.id);
    }

    // Per-file set of import path segments (for O(1) import-aware scoring).
    let mut imports: HashMap<&str, ImportSet> = HashMap::new();
    for e in graph.edges() {
        if e.kind == EdgeKind::Imports {
            if let Some(s) = graph.symbol(e.from) {
                imports.entry(s.file.as_str()).or_default().add(&e.to_name);
            }
        }
    }
    let no_imports = ImportSet::default();

    // Compute bindings under an immutable borrow, then apply in place — no
    // edge cloning (ADR-0020: re-resolution is on the incremental hot path).
    let bindings: Vec<(usize, SymbolId, u16)> = graph
        .edges()
        .par_iter()
        .enumerate()
        .filter(|(_, e)| e.to.is_none())
        .filter_map(|(i, edge)| {
            let src = graph.symbol(edge.from)?;
            if let Some(sc) = scope {
                // Unbound edges outside the scope were unbound before and
                // would stay so — skip re-trying them (mostly external calls).
                if !sc.files.contains(&src.file) && !sc.names.contains(last_seg(&edge.to_name)) {
                    return None;
                }
            }
            let caller = Caller {
                file: &src.file,
                owner: src.owner.as_deref(),
                language: src.language,
            };
            let file_imports = imports.get(src.file.as_str()).unwrap_or(&no_imports);
            resolve_one(edge, &caller, file_imports, &name_index).map(|(to, alts)| (i, to, alts))
        })
        .collect();
    drop(name_index);
    drop(imports);

    let edges = graph.edges_mut();
    for (i, to, alternatives) in bindings {
        let e = &mut edges[i];
        e.to = Some(to);
        e.alternatives = alternatives;
        e.confidence = Confidence::Heuristic;
    }
    graph.reindex();
}

/// The distinct path segments named by a file's imports.
#[derive(Default)]
struct ImportSet(std::collections::HashSet<String>);

impl ImportSet {
    fn add(&mut self, import: &str) {
        for seg in import
            .split([':', '.', '/', '{', '}', ',', ' ', '"', '\''])
            .filter(|s| !s.is_empty())
        {
            self.0.insert(seg.to_string());
        }
    }

    /// 3 when an import names the candidate's module (`pkg.beta`,
    /// `crate::store`), 2 when only its package directory (Go-style), else 0.
    fn score(&self, c: &Cand) -> u32 {
        if self.0.is_empty() {
            0
        } else if self.0.contains(&c.stem) {
            3
        } else if !c.dir.is_empty() && self.0.contains(&c.dir) {
            2
        } else {
            0
        }
    }
}

/// Score candidates and return the best `(target, tied_alternatives)`.
///
/// Scoring (higher wins; ADR-0018):
/// * owner match — `self.m()` in a method of `T` → `T::m`; `T::m()` → owner `T`: **+16**
/// * module qualifier — `store::open()` / `store.open()` → a symbol in `store.*`: **+8**
/// * same file: **+4**
/// * caller's file imports the candidate's module: **+3** (package dir only: **+2**)
/// * bare call `f()` → a free function (no owner): **+2**
/// * base: **+1**
fn resolve_one(
    edge: &Edge,
    caller: &Caller,
    file_imports: &ImportSet,
    name_index: &HashMap<&str, Vec<Cand>>,
) -> Option<(SymbolId, u16)> {
    // The callee/reference name may be a path tail; match on the last segment.
    let name = edge
        .to_name
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(&edge.to_name);
    let candidates = name_index.get(name)?;
    let want_callable = matches!(edge.kind, EdgeKind::Calls | EdgeKind::Defines);
    let q = edge.qualifier.as_deref();

    let mut best: Option<(SymbolId, u32)> = None;
    let mut ties: u16 = 0;
    let mut best_key: Option<(Option<&str>, &str)> = None;
    for c in candidates {
        if c.id == edge.from {
            continue; // never self-resolve
        }
        if want_callable && !c.kind.is_callable() {
            continue;
        }
        if edge.kind == EdgeKind::Defines && c.owner.as_deref() != q {
            continue; // a trait impl binds only to the trait's own method
        }
        let mut score = 1u32;
        match q {
            Some(q) if is_self_qualifier(q) => {
                if c.owner.is_some() && c.owner.as_deref() == caller.owner {
                    score += 16;
                }
            }
            Some(q) => {
                if c.owner.as_deref() == Some(q) {
                    score += 16;
                } else if q == c.stem || q == c.dir {
                    // `module::f()` names a module member: prefer its free
                    // functions over same-named methods in that module.
                    score += if c.owner.is_none() { 9 } else { 8 };
                }
            }
            None => {
                if implicit_receiver(caller.language)
                    && c.owner.is_some()
                    && c.owner.as_deref() == caller.owner
                {
                    score += 16; // implicit this/self
                } else if c.owner.is_none() {
                    score += 2;
                }
            }
        }
        if c.file == caller.file {
            score += 4;
        }
        score += file_imports.score(c);
        match best {
            Some((_, b)) if b > score => {}
            Some((_, b)) if b == score => {
                // Overloads (same owner, same file) are one logical method —
                // not real ambiguity (Java/C#/C++ overload sets).
                if best_key != Some((c.owner.as_deref(), c.file.as_str())) {
                    ties += 1;
                }
            }
            _ => {
                best = Some((c.id, score));
                best_key = Some((c.owner.as_deref(), c.file.as_str()));
                ties = 0;
            }
        }
    }
    // Instantiation without an explicit constructor: `Repo()`, `new Repo()`,
    // Ruby `Repo.new` — a capitalized callee with no callable candidate binds
    // to the class/struct itself.
    if best.is_none()
        && edge.kind == EdgeKind::Calls
        && name.starts_with(|c: char| c.is_ascii_uppercase())
    {
        let classes: Vec<&Cand> = candidates
            .iter()
            .filter(|c| matches!(c.kind, SymbolKind::Class | SymbolKind::Struct))
            .collect();
        let pick = classes
            .iter()
            .max_by_key(|c| {
                (
                    u32::from(c.file == caller.file) * 4 + file_imports.score(c),
                    std::cmp::Reverse(c.id),
                )
            })
            .map(|c| c.id);
        return pick.map(|id| (id, classes.len().saturating_sub(1) as u16));
    }

    // A call through an arbitrary variable (`items.push(x)`) with no
    // supporting evidence — not same-file, not imported, no owner/module
    // match — is far more likely a std/third-party method than whichever repo
    // method shares the name. Leave it unresolved rather than invent an edge.
    // Uppercase qualifier that is a *type* (owns methods somewhere) vs. a
    // module-ish name; only method candidates make it a typed receiver.
    let typed_receiver = q.is_some_and(|q| {
        q.starts_with(|c: char| c.is_ascii_uppercase())
            && !is_self_qualifier(q)
            && candidates.iter().any(|c| c.owner.is_some())
    });
    let receiver_is_variable = q.is_some_and(|q| {
        !is_self_qualifier(q) && q.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
    });
    match best {
        Some((_, score)) if receiver_is_variable && score < MIN_RECEIVER_SCORE => None,
        // A typed receiver (`Vec`, `String`, an external type) whose type owns
        // none of the candidates: the method lives outside the repo.
        Some((_, score)) if typed_receiver && score < 16 && edge.kind == EdgeKind::Calls => None,
        Some((id, _)) => Some((id, ties)),
        None => None,
    }
}

/// Minimum evidence (see scoring table) to bind a call through a receiver
/// whose type could not be inferred: a module-qualifier match or better.
/// (Receivers with an inferred type carry that type as the qualifier and
/// bind via the owner rule instead.)
const MIN_RECEIVER_SCORE: u32 = 9;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;

    #[test]
    fn resolves_call_within_file() {
        let src = "fn helper() {}\nfn main() {\n    helper();\n}\n";
        let f = extract(Language::Rust, "a.rs", src, 0);
        let mut g = CodeGraph::new();
        g.upsert_file(f);
        g.reindex();
        resolve(&mut g);
        let call = g
            .edges()
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == "helper")
            .unwrap();
        assert!(call.to.is_some(), "call to helper should resolve");
        assert_eq!(call.confidence, Confidence::Heuristic);
    }

    #[test]
    fn resolves_call_across_files() {
        let a = extract(Language::Rust, "a.rs", "pub fn shared() {}\n", 0);
        let b = extract(Language::Rust, "b.rs", "fn run() {\n    shared();\n}\n", 0);
        let mut g = CodeGraph::new();
        g.upsert_file(a);
        g.upsert_file(b);
        g.reindex();
        resolve(&mut g);
        let call = g
            .edges()
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == "shared")
            .unwrap();
        let target = call.to.expect("cross-file call resolves");
        assert_eq!(g.symbol(target).unwrap().file, "a.rs");
    }

    fn graph(files: &[(&str, &str)]) -> CodeGraph {
        let mut g = CodeGraph::new();
        for (p, src) in files {
            let lang = Language::from_path(std::path::Path::new(p)).unwrap();
            g.upsert_file(extract(lang, p, src, 0));
        }
        g.reindex();
        resolve(&mut g);
        g
    }

    fn target_of<'a>(
        g: &'a CodeGraph,
        caller: &str,
        callee: &str,
    ) -> Option<&'a crate::domain::Symbol> {
        let from = g
            .symbols()
            .find(|s| s.name == caller && s.kind.is_callable())?
            .id;
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == callee)?;
        g.symbol(e.to?)
    }

    #[test]
    fn rust_self_call_binds_to_own_impl() {
        let g = graph(&[
            ("a.rs", "struct A;\nimpl A {\n    fn run(&self) { self.step(); }\n    fn step(&self) {}\n}\n"),
            ("b.rs", "struct B;\nimpl B {\n    fn step(&self) {}\n}\n"),
        ]);
        let t = target_of(&g, "run", "step").unwrap();
        assert_eq!(t.owner.as_deref(), Some("A"));
        assert_eq!(t.kind, SymbolKind::Method);
    }

    #[test]
    fn type_qualified_call_binds_to_owner() {
        // The same-file `new` would win on locality alone; `B::new` must win on owner.
        let g = graph(&[
            (
                "a.rs",
                "struct A;\nimpl A { fn new() -> A { A } }\nfn make() { B::new(); }\n",
            ),
            (
                "b.rs",
                "pub struct B;\nimpl B { pub fn new() -> B { B } }\n",
            ),
        ]);
        let t = target_of(&g, "make", "new").unwrap();
        assert_eq!(t.owner.as_deref(), Some("B"));
    }

    #[test]
    fn module_qualified_call_binds_to_module() {
        let g = graph(&[
            ("src/store.rs", "pub fn open() {}\n"),
            ("src/walker.rs", "pub fn open() {}\n"),
            ("src/main.rs", "fn main() { store::open(); }\n"),
        ]);
        assert_eq!(target_of(&g, "main", "open").unwrap().file, "src/store.rs");
    }

    #[test]
    fn imports_break_cross_file_ties() {
        let g = graph(&[
            ("pkg/alpha.py", "def helper():\n    pass\n"),
            ("pkg/beta.py", "def helper():\n    pass\n"),
            (
                "app.py",
                "from pkg.beta import helper\n\ndef run():\n    helper()\n",
            ),
        ]);
        let t = target_of(&g, "run", "helper").unwrap();
        assert_eq!(t.file, "pkg/beta.py");
    }

    #[test]
    fn unrelated_variable_receiver_stays_unbound() {
        // `v.push(1)` on some Vec must not bind to an unrelated repo method.
        let g = graph(&[
            (
                "budget.rs",
                "struct Budget;\nimpl Budget { fn push(&mut self) {} }\n",
            ),
            (
                "other.rs",
                "fn fill() { let mut v = Vec::new(); v.push(1); }\n",
            ),
        ]);
        let from = g.by_name("fill")[0];
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Calls && e.to_name == "push")
            .unwrap();
        assert!(e.to.is_none());
        // …same file is not enough evidence either…
        let g = graph(&[(
            "budget.rs",
            "struct Budget;\nimpl Budget { fn push(&mut self) {} }\nfn fill(v: Vec<u8>) { v.push(1); }\n",
        )]);
        assert!(target_of(&g, "fill", "push").is_none());
        // …but an inferred receiver type binds it precisely, even cross-file.
        let g = graph(&[
            (
                "budget.rs",
                "pub struct Budget;\nimpl Budget { pub fn push(&mut self) {} }\n",
            ),
            ("user.rs", "fn fill(b: &mut Budget) { b.push(); }\n"),
        ]);
        assert_eq!(
            target_of(&g, "fill", "push").unwrap().owner.as_deref(),
            Some("Budget")
        );
    }

    #[test]
    fn implicit_receiver_prefers_own_class() {
        let g = graph(&[
            (
                "a.rb",
                "class A\n  def run\n    step\n  end\n  def step\n  end\nend\n",
            ),
            ("b.rb", "class B\n  def step\n  end\nend\n"),
        ]);
        assert_eq!(
            target_of(&g, "run", "step").unwrap().owner.as_deref(),
            Some("A")
        );
        // Rust has no implicit receiver: a bare call prefers the free function.
        let g = graph(&[(
            "a.rs",
            "fn step() {}\nstruct A;\nimpl A {\n    fn step(&self) {}\n    fn run(&self) { step(); }\n}\n",
        )]);
        assert_eq!(target_of(&g, "run", "step").unwrap().owner, None);
    }

    #[test]
    fn overloads_are_not_ambiguous() {
        let g = graph(&[
            (
                "Svc.java",
                "class Svc {\n  void log(String s) {}\n  void log(int n) {}\n  void run() { log(1); }\n}\n",
            ),
        ]);
        let from = g.symbols().find(|s| s.name == "run").unwrap().id;
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Calls)
            .unwrap();
        assert!(e.to.is_some());
        assert_eq!(e.alternatives, 0);
    }

    #[test]
    fn ambiguity_is_counted() {
        let g = graph(&[
            ("x.rs", "pub fn dup() {}\n"),
            ("y.rs", "pub fn dup() {}\n"),
            ("z.rs", "fn caller() { dup(); }\n"),
        ]);
        let from = g.by_name("caller")[0];
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Calls)
            .unwrap();
        assert!(e.to.is_some());
        assert_eq!(e.alternatives, 1);
    }

    #[test]
    fn trait_impl_defines_trait_method() {
        let g = graph(&[
            ("t.rs", "pub trait Shape {\n    fn area(&self) -> f64;\n}\n"),
            (
                "c.rs",
                "struct Circle;\nimpl Shape for Circle {\n    fn area(&self) -> f64 { 1.0 }\n}\n",
            ),
        ]);
        let imp = g
            .symbols()
            .find(|s| s.name == "area" && s.owner.as_deref() == Some("Circle"))
            .unwrap();
        let def = g
            .out_edges(imp.id)
            .find(|e| e.kind == EdgeKind::Defines)
            .unwrap();
        let tr = g.symbol(def.to.unwrap()).unwrap();
        assert_eq!(tr.owner.as_deref(), Some("Shape"));
        assert_eq!(tr.file, "t.rs");
    }

    #[test]
    fn python_and_go_methods_get_owners() {
        let g = graph(&[
            ("m.py", "class Repo:\n    def save(self):\n        self.flush()\n    def flush(self):\n        pass\n"),
            ("s.go", "package s\ntype Server struct{}\nfunc (s *Server) Start() { s.listen() }\nfunc (s *Server) listen() {}\n"),
        ]);
        let save = g.symbols().find(|s| s.name == "save").unwrap();
        assert_eq!(
            (save.kind, save.owner.as_deref()),
            (SymbolKind::Method, Some("Repo"))
        );
        assert_eq!(
            target_of(&g, "save", "flush").unwrap().owner.as_deref(),
            Some("Repo")
        );
        let start = g.symbols().find(|s| s.name == "Start").unwrap();
        assert_eq!(start.owner.as_deref(), Some("Server"));
    }
}
