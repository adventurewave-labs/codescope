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
#[derive(Clone)]
struct Cand {
    id: SymbolId,
    kind: SymbolKind,
    file: String,
    /// File stem (`store` for `src/store.rs`) and parent dir (`store` for
    /// `pkg/store/x.go`) — module-qualifier matching.
    stem: String,
    dir: String,
    /// Full parent path (`src/interfaces` for `src/interfaces/mcp.rs`) —
    /// import-path evidence.
    dir_path: String,
    owner: Option<String>,
    language: Language,
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
    // Module symbols keyed by their file's basename — for imports that name a
    // file outright (`#include "list.h"`, `from "./b.ts"`).
    let mut by_basename: HashMap<&str, Vec<Cand>> = HashMap::new();
    for s in graph.symbols() {
        let (stem, dir) = stem_and_dir(&s.file);
        let dir_path = s
            .file
            .rsplit_once(['/', '\\'])
            .map(|(d, _)| d.to_string())
            .unwrap_or_default();
        let cand = Cand {
            id: s.id,
            kind: s.kind,
            file: s.file.clone(),
            stem,
            dir,
            dir_path,
            owner: s.owner.clone(),
            language: s.language,
        };
        if s.kind == SymbolKind::Module {
            if let Some(base) = s.file.rsplit(['/', '\\']).next() {
                by_basename.entry(base).or_default().push(cand.clone());
            }
        }
        name_index.entry(s.name.as_str()).or_default().push(cand);
    }
    // Deterministic candidate order → deterministic tie-breaking.
    for v in name_index.values_mut() {
        v.sort_by_key(|c| c.id);
    }
    for v in by_basename.values_mut() {
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
            let bound = if edge.kind == EdgeKind::Imports {
                resolve_import(edge, &caller, &name_index, &by_basename)
            } else {
                resolve_one(edge, &caller, file_imports, &name_index)
            };
            bound.map(|(to, alts)| (i, to, alts))
        })
        .collect();
    drop(name_index);
    drop(imports);
    drop(by_basename);

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

/// File extensions an import string may carry when it names a file outright
/// (`#include "list.h"`, `require "./b.ts"`). Anything else with a dot is a
/// dotted module path (`app.store.Repo`), not a filename.
fn is_code_extension(ext: &str) -> bool {
    matches!(
        ext,
        "h" | "hh"
            | "hpp"
            | "hxx"
            | "inc"
            | "c"
            | "cc"
            | "cpp"
            | "cxx"
            | "cs"
            | "rs"
            | "py"
            | "pyi"
            | "rb"
            | "go"
            | "java"
            | "ts"
            | "tsx"
            | "mts"
            | "cts"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
    )
}

/// Languages that can import each other's files. Imports are language-local
/// (`import b` in Python can never mean `cpp/b.hpp`); the one real-world
/// crossover is TypeScript pulling in JavaScript (and C including C++ headers).
fn lang_family(lang: Language) -> u8 {
    match lang {
        Language::JavaScript | Language::TypeScript => 1,
        Language::C | Language::Cpp => 2,
        Language::Rust => 3,
        Language::Python => 4,
        Language::Go => 5,
        Language::Java => 6,
        Language::CSharp => 7,
        Language::Ruby => 8,
    }
}

/// The parent-directory segment of a path (`py` for `py/a.py`).
fn dir_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).nth(1).unwrap_or("")
}

/// Bind an `Imports` edge to the module symbol of the file it names.
///
/// Every shape is conservative (ADR-0023): candidates are always confined to
/// the caller's language family and never include the importing file itself.
///
/// * **File-naming imports** — the string carries a code extension
///   (`"list.h"`, `"./b.ts"`). Bind the module symbol of the file with that
///   exact basename; same-directory files win ties. `#include <stdio.h>`
///   stays unbound unless a `stdio.h` actually exists in the repo. TS
///   NodeNext-style `"./b.js"` (which means `b.ts`) retries without the JS
///   extension as a relative module path.
/// * **Relative module imports** — `"./b"`, `"../lib/b"`. Resolved as an
///   exact path against the importing file's directory (`..` components
///   applied), matching the module whose file lives there — never a
///   same-named module elsewhere.
/// * **Qualified imports** — `app.store.Repo`, `crate::fresh::LiveGraph`.
///   The parent path must be proven *per candidate*: the candidate's own
///   path must end with the parent chain (dotted/slash) or with a suffix of
///   it (`::`-paths, where the crate-name prefix need not match the disk
///   layout), so `crate::a::B` cannot bind a `B` that lives outside `a`.
///   Rust's prelude roots (`std`, `core`, `alloc`) are never in-repo.
/// * **Bare imports** — `b`, `import time`, `use serde;`. Single-segment
///   names bind module symbols only, never a same-named function.
fn resolve_import(
    edge: &Edge,
    caller: &Caller,
    name_index: &HashMap<&str, Vec<Cand>>,
    by_basename: &HashMap<&str, Vec<Cand>>,
) -> Option<(SymbolId, u16)> {
    let raw = edge
        .to_name
        .trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
    let relative = raw.starts_with("./") || raw.starts_with("../");
    let mut eff = raw;

    // Shape 1: the import names a file (`list.h`). Only a module in a file
    // with that exact basename can match, which keeps system includes
    // unbound.
    'shape1: {
        let seg = eff.rsplit(['/', '\\']).next().unwrap_or(eff);
        let Some((_, ext)) = seg.rsplit_once('.') else {
            break 'shape1;
        };
        if !is_code_extension(ext) {
            break 'shape1;
        }
        if let Some(cands) = by_basename.get(seg) {
            let mut best: Option<(SymbolId, u32)> = None;
            let mut ties = 0u16;
            for c in cands {
                if c.id == edge.from {
                    continue; // a file never includes itself
                }
                if lang_family(c.language) != lang_family(caller.language) {
                    continue;
                }
                // A multi-component include (`sub/b.h`, `../sub/b.h`) must
                // honor its path: a file at that exact suffix beats a
                // same-basename file in the importer's directory. `c.dir` is
                // the last directory segment — see `stem_and_dir`; `dir_of`
                // mirrors that contract.
                // Normalize away `.`/`..` components so `../sub/b.h` competes
                // on its real path.
                let full: String = eff
                    .split(['/', '\\'])
                    .filter(|s| !s.is_empty() && *s != "." && *s != "..")
                    .collect::<Vec<_>>()
                    .join("/");
                let on_path =
                    c.file == full || (full.contains('/') && c.file.ends_with(&format!("/{full}")));
                let score =
                    1 + u32::from(on_path) * 32 + u32::from(c.dir == dir_of(caller.file)) * 8;
                match best {
                    Some((_, b)) if b > score => {}
                    Some((_, b)) if b == score => ties += 1,
                    _ => {
                        best = Some((c.id, score));
                        ties = 0;
                    }
                }
            }
            return best.map(|(id, _)| (id, ties));
        }
        // No file of that basename: TS NodeNext writes `"./b.js"` while
        // meaning `b.ts` — retry as a relative module path without the
        // extension. Anything else names a file we simply do not have.
        if relative && matches!(ext, "js" | "mjs" | "cjs" | "jsx") {
            eff = &eff[..eff.len() - ext.len() - 1];
        } else {
            return None;
        }
    }

    // Shape 2: the import names a module (or, for qualified `::` paths, a
    // symbol inside one). Normalize `crate::a::B`, `app.store.Repo`, `./b`
    // down to the candidate name.
    let seg = eff.rsplit(['/', '\\']).next().unwrap_or(eff);
    let name = seg
        .rsplit("::")
        .next()
        .and_then(|s| s.rsplit('.').next())
        .unwrap_or(seg);
    let segments: Vec<&str> = eff
        .split([':', '.', '/', '\\', ' '])
        .filter(|s| !s.is_empty())
        .collect();
    let cands = name_index.get(name)?;

    // Relative imports resolve as exact paths against the importer's
    // directory — `../lib/b` from `c/d/x.ts` means `c/lib/b`, not whichever
    // module is named `b` nearest by name.
    if relative {
        let mut target: Vec<String> = caller
            .file
            .split(['/', '\\'])
            .filter(|s| !s.is_empty() && *s != ".")
            .map(String::from)
            .collect();
        target.pop(); // the importing file itself
        for t in eff.split(['/', '\\']).filter(|s| !s.is_empty()) {
            match t {
                ".." => {
                    target.pop();
                }
                "." => {}
                t => target.push(t.to_string()),
            }
        }
        let mut best: Option<(SymbolId, u32)> = None;
        let mut ties = 0u16;
        for c in cands {
            if c.id == edge.from || lang_family(c.language) != lang_family(caller.language) {
                continue;
            }
            if c.kind != SymbolKind::Module {
                continue; // a relative path names a file/module, not a symbol
            }
            if candidate_segments(c) != target {
                continue;
            }
            let score = 1;
            match best {
                Some((_, b)) if b == score => ties += 1,
                _ => {
                    best = Some((c.id, score));
                    ties = 0;
                }
            }
        }
        return best.map(|(id, _)| (id, ties));
    }

    let qualified = segments.len() > 1;
    if qualified
        && matches!(
            segments.first(),
            Some(&"std") | Some(&"core") | Some(&"alloc")
        )
    {
        return None; // Rust prelude roots are never in-repo
    }

    let mut best: Option<(SymbolId, u32)> = None;
    let mut ties = 0u16;
    for c in cands {
        if c.id == edge.from || lang_family(c.language) != lang_family(caller.language) {
            continue;
        }
        if qualified {
            if !proven_for_candidate(eff, &segments, c) {
                continue;
            }
        } else if c.kind != SymbolKind::Module {
            continue; // a bare import names a module, never a function
        }
        let mut score = 1;
        if c.kind == SymbolKind::Module {
            score += 6; // an import names a module first, a symbol second
        }
        if c.dir == dir_of(caller.file) {
            score += 4; // relative imports resolve against the importer's dir
        }
        if c.file == caller.file {
            score += 2;
        }
        match best {
            Some((_, b)) if b > score => {}
            Some((_, b)) if b == score => ties += 1,
            _ => {
                best = Some((c.id, score));
                ties = 0;
            }
        }
    }
    best.map(|(id, _)| (id, ties))
}

/// The candidate's path as segments, file stem for the final component
/// (`src/fresh.rs` → ["src", "fresh"]).
fn candidate_segments(c: &Cand) -> Vec<String> {
    let fname = c.file.rsplit(['/', '\\']).next().unwrap_or(&c.file);
    // Strip only the final extension: `b.test.ts` stems to `b.test`.
    let stem = match fname.rsplit_once('.') {
        Some((s, e)) if is_code_extension(e) => s,
        _ => fname.split('.').next().unwrap_or(fname),
    };
    let mut segs: Vec<String> = c
        .dir_path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    // `mod.rs` / `__init__.py` / `index.ts` are named by their directory.
    // (`lib.rs` is deliberately NOT remapped: a python `pkg/lib.py` is a real
    // module named `lib`, not the directory's package root.)
    if matches!(stem, "mod" | "__init__" | "index") && !segs.is_empty() {
        return segs;
    }
    segs.push(stem.to_string());
    segs
}

/// Per-candidate in-repo evidence for a qualified import path: the parent
/// chain must be a suffix of the candidate's own path.
///
/// * `::`-separated (Rust-style): the crate-name prefix need not match the
///   on-disk layout, so any non-empty suffix of the parent chain suffices —
///   `codescope::fresh::LiveGraph` passes on `fresh`. `crate`/`self`/`super`
///   prove in-repo-ness but are dropped before matching, so `crate::a::B`
///   still requires the candidate to live under `a`.
/// * dotted/slash package paths: the full parent chain must be a path
///   suffix — `app.store.Repo` passes for `app/store/Repo.java`, while
///   `java.util.List` fails even beside a `java/` directory (there is no
///   `java/util` chain above the candidate).
fn proven_for_candidate(raw: &str, segments: &[&str], c: &Cand) -> bool {
    let parent = &segments[..segments.len() - 1];
    let parent: Vec<&str> = parent
        .iter()
        .skip_while(|s| matches!(**s, "crate" | "self" | "super"))
        .copied()
        .collect();
    if parent.is_empty() {
        return true; // `crate::resolve` — proven, module named outright
    }
    let csegs = candidate_segments(c);
    let csegs: Vec<&str> = csegs.iter().map(|s| s.as_str()).collect();
    if csegs.len() < parent.len() {
        return false;
    }
    if raw.contains("::") {
        (1..=parent.len()).any(|i| csegs.ends_with(&parent[i - 1..]))
    } else {
        // Dotted package paths name the package the class lives in: the
        // parent chain must be a suffix of the candidate's directory, with
        // the file itself beyond it.
        csegs.len() > parent.len() && csegs[..csegs.len() - 1].ends_with(&parent[..])
    }
}

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

    /// The module symbol representing a file (import edges hang off it).
    fn module_of(g: &CodeGraph, file: &str) -> crate::domain::SymbolId {
        g.symbols()
            .find(|s| s.kind == SymbolKind::Module && s.file == file)
            .unwrap_or_else(|| panic!("no module symbol for {file}"))
            .id
    }

    #[test]
    fn c_includes_resolve_to_file_modules() {
        let g = graph(&[
            (
                "a.c",
                "#include <stdio.h>\n#include \"b.h\"\nint main(void) { return 0; }\n",
            ),
            ("b.h", "#include \"a.h\"\nint beta_c(int x);\n"),
            ("a.h", "int alpha_c(int x);\n"),
        ]);
        let from = module_of(&g, "a.c");
        let local = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "b.h")
            .unwrap();
        let target = g.symbol(local.to.expect("\"b.h\" resolves")).unwrap();
        assert_eq!(target.file, "b.h");
        let system = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "stdio.h")
            .unwrap();
        assert!(system.to.is_none(), "system include stays unbound");
    }

    #[test]
    fn ts_relative_imports_resolve_to_file_modules() {
        let g = graph(&[
            (
                "a.ts",
                "import { betaTs } from \"./b\";\nexport function alphaTs(x: number): number { return betaTs(x); }\n",
            ),
            (
                "b.ts",
                "import { alphaTs } from \"./a\";\nexport function betaTs(x: number): number { return x * 2; }\n",
            ),
        ]);
        for (f, imp, target_file) in [("a.ts", "./b", "b.ts"), ("b.ts", "./a", "a.ts")] {
            let from = module_of(&g, f);
            let e = g
                .out_edges(from)
                .find(|e| e.kind == EdgeKind::Imports && e.to_name == imp)
                .unwrap_or_else(|| panic!("no import edge {imp} in {f}"));
            let target = g
                .symbol(e.to.unwrap_or_else(|| panic!("{imp} should resolve")))
                .unwrap();
            assert_eq!(target.kind, SymbolKind::Module);
            assert_eq!(target.file, target_file);
        }
    }

    #[test]
    fn imports_never_cross_language_families() {
        // `import b` in Python must bind py/b.py, never a same-named module
        // of another language — even when the decoy sits in the importer's
        // own directory (without the family guard the decoy would win on
        // the same-directory bonus).
        let g = graph(&[
            (
                "py/a.py",
                "import b\n\ndef alpha(x):\n    return b.beta(x)\n",
            ),
            ("py/b.py", "def beta(x):\n    return x\n"),
            ("py/b.hpp", "int beta_cpp(int x);\n"),
        ]);
        let from = module_of(&g, "py/a.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "b")
            .unwrap();
        let target = g.symbol(e.to.expect("py import resolves")).unwrap();
        assert_eq!(target.file, "py/b.py");
    }

    #[test]
    fn header_cycles_are_detected() {
        use crate::query::dependency_graph;
        let g = graph(&[
            ("a.h", "#include \"b.h\"\nint alpha_c(int x);\n"),
            ("b.h", "#include \"a.h\"\nint beta_c(int x);\n"),
        ]);
        let dg = dependency_graph(&g, 4000);
        assert!(
            !dg.cycles.is_empty(),
            "mutual includes must form a detected cycle"
        );
    }

    #[test]
    fn ts_import_cycles_are_detected() {
        use crate::query::dependency_graph;
        let g = graph(&[
            (
                "a.ts",
                "import { bFn } from \"./b\";\nexport function aFn() { return bFn(); }\n",
            ),
            (
                "b.ts",
                "import { aFn } from \"./a\";\nexport function bFn() { return aFn(); }\n",
            ),
        ]);
        let dg = dependency_graph(&g, 4000);
        assert!(
            !dg.cycles.is_empty(),
            "mutual relative imports must form a detected cycle"
        );
    }

    #[test]
    fn external_qualified_imports_stay_unbound() {
        // An in-repo symbol that happens to share the imported name must not
        // capture an external import (`std::process::Command` vs clap's
        // `enum Command`) — no parent segment names an in-repo module.
        let g = graph(&[
            (
                "main.rs",
                "use std::process::Command;\nfn main() { let _c = Command::new(\"ls\"); }\n",
            ),
            ("cli.rs", "pub enum Command { Run, Help }\n"),
        ]);
        let from = module_of(&g, "main.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "std::process::Command must not bind to the in-repo enum"
        );
    }

    #[test]
    fn crate_qualified_imports_bind() {
        let g = graph(&[
            (
                "examples/demo.rs",
                "use crate::fresh::LiveGraph;\nfn main() { let _g: LiveGraph; }\n",
            ),
            ("src/fresh.rs", "pub struct LiveGraph;\n"),
        ]);
        let from = module_of(&g, "examples/demo.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("crate:: path with real module binds"))
            .unwrap();
        assert_eq!(target.file, "src/fresh.rs");
    }

    #[test]
    fn java_external_imports_with_colliding_dirs_stay_unbound() {
        // A java-family `java` directory exists and an in-repo class shares
        // the imported name — `java.util.List` still must not bind, because
        // the chain `java/util` is not a real directory (a single colliding
        // path segment is not evidence).
        let g = graph(&[
            (
                "java/app/Svc.java",
                "package java.app;\nimport java.util.List;\npublic class Svc { List l; }\n",
            ),
            ("List.java", "public class List {}\n"),
        ]);
        let from = module_of(&g, "java/app/Svc.java");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name.contains("List"))
            .unwrap();
        assert!(
            e.to.is_none(),
            "java.util.List must not capture the in-repo List"
        );
    }

    #[test]
    fn bare_imports_bind_only_modules() {
        // `import time` names a module; a same-named function in another
        // file must not capture it.
        let g = graph(&[
            ("a.py", "import time\n\ndef helper():\n    pass\n"),
            ("b.py", "def time():\n    pass\n"),
        ]);
        let from = module_of(&g, "a.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "import time must not bind to the function named time"
        );
    }

    #[test]
    fn same_basename_headers_prefer_the_importers_directory() {
        let g = graph(&[
            ("lib/util/head.h", "int lu(void);\n"),
            ("c/d/head.h", "int cd(void);\n"),
            (
                "c/d/main.c",
                "#include \"head.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "c/d/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "head.h")
            .unwrap();
        let target = g.symbol(e.to.expect("head.h resolves")).unwrap();
        assert_eq!(target.file, "c/d/head.h");
    }
    #[test]
    fn relative_imports_apply_dotdot_components() {
        // `../lib/b` from c/d/x.ts means c/lib/b — not the same-named module
        // sitting in the importer's own directory.
        let g = graph(&[
            (
                "c/d/x.ts",
                "import { b } from \"../lib/b\";\nexport function x() { return b(); }\n",
            ),
            ("c/d/b.ts", "export function b() { return 1; }\n"),
            ("c/lib/b.ts", "export function b() { return 2; }\n"),
        ]);
        let from = module_of(&g, "c/d/x.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("../lib/b resolves")).unwrap();
        assert_eq!(target.file, "c/lib/b.ts");
    }

    #[test]
    fn nodenext_js_extension_resolves_to_ts() {
        let g = graph(&[
            (
                "a.ts",
                "import { b } from \"./b.js\";\nexport function a() { return b(); }\n",
            ),
            ("b.ts", "export function b() { return 1; }\n"),
        ]);
        let from = module_of(&g, "a.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("./b.js means b.ts")).unwrap();
        assert_eq!(target.file, "b.ts");
    }

    #[test]
    fn crate_qualified_symbol_binds_only_inside_its_module() {
        // `crate::a::B` must not capture a same-named B that lives outside
        // module `a`, even though both are in-repo.
        let g = graph(&[
            ("src/x.rs", "use crate::a::B;\nfn main() { let _b: B; }\n"),
            ("src/a.rs", "pub struct B;\n"),
            ("src/b.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("B binds inside module a")).unwrap();
        assert_eq!(target.file, "src/a.rs");
    }

    #[test]
    fn std_path_with_in_repo_stem_stays_unbound() {
        // An in-repo `sync.rs` must not vouch for `std::sync::Arc`.
        let g = graph(&[
            (
                "src/x.rs",
                "use std::sync::Arc;\nfn main() { let _a: Arc; }\n",
            ),
            ("src/sync.rs", "pub struct Arc;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(e.to.is_none(), "std:: paths are never in-repo");
    }

    #[test]
    fn external_crate_path_with_in_repo_stem_stays_unbound() {
        // Not just the std prelude: a third-party path (`serde::de::…`)
        // beside an in-repo `de.rs` must stay unbound — the parent chain
        // must be proven against the *candidate's own* path.
        let g = graph(&[
            (
                "src/x.rs",
                "use serde::de::Deserialize;\nfn main() { let _d: Deserialize; }\n",
            ),
            ("src/de.rs", "pub mod other { pub struct Noop; }\n"),
            ("src/y.rs", "pub struct Deserialize;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "serde::de::Deserialize must not capture the in-repo Deserialize"
        );
    }

    #[test]
    fn python_dotted_submodule_binds() {
        let g = graph(&[
            ("pkg/__init__.py", "\n"),
            ("pkg/lib.py", "def run(x):\n    return x\n"),
            (
                "app.py",
                "import pkg.lib\n\ndef go():\n    return pkg.lib.run(1)\n",
            ),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("pkg.lib binds")).unwrap();
        assert_eq!(target.file, "pkg/lib.py");
    }

    #[test]
    fn multi_component_include_honors_its_path() {
        let g = graph(&[
            ("sub/b.h", "int sub_b(void);\n"),
            ("main/b.h", "int main_b(void);\n"),
            (
                "main/main.c",
                "#include \"b.h\"\n#include \"../sub/b.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "main/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "../sub/b.h")
            .unwrap();
        let target = g.symbol(e.to.expect("../sub/b.h resolves")).unwrap();
        assert_eq!(target.file, "sub/b.h");
    }

    #[test]
    fn dotted_package_import_binds_under_nested_root() {
        // A real package chain (`app/store`) deep in the tree binds even
        // though it is not rooted at the repo root.
        let g = graph(&[
            (
                "tests/eval/java/app/store/Repo.java",
                "package app.store;\npublic class Repo {}\n",
            ),
            (
                "tests/eval/java/app/svc/Svc.java",
                "package app.svc;\nimport app.store.Repo;\npublic class Svc { Repo r; }\n",
            ),
        ]);
        let from = module_of(&g, "tests/eval/java/app/svc/Svc.java");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name.contains("Repo"))
            .unwrap();
        let target = g.symbol(e.to.expect("app.store.Repo binds")).unwrap();
        assert_eq!(target.file, "tests/eval/java/app/store/Repo.java");
    }
}
