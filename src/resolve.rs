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
        let cand = Cand {
            id: s.id,
            kind: s.kind,
            file: s.file.clone(),
            stem,
            dir,
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

/// The caller's crate-root prefix: path components up to and including the
/// last `src` directory (`crates/foo/src` for `crates/foo/src/x.rs`; empty
/// for a crate rooted at the repository root).
fn crate_root_prefix(caller_file: &str) -> Vec<String> {
    let parts: Vec<&str> = caller_file
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    match parts.iter().rposition(|s| *s == "src") {
        Some(i) if i + 1 < parts.len() => parts[..=i].iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// The full parent path of a file (`py/sub` for `py/sub/a.py`, "" at the
/// root) — directory affinity must compare whole paths, not just the final
/// segment, or `app/include/` and `vendor/include/` look alike.
fn parent_path(path: &str) -> &str {
    path.rsplit_once(['/', '\\']).map(|(d, _)| d).unwrap_or("")
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
        if relative {
            // A relative file-naming import resolves to exactly one path:
            // apply the `..`/`.` components against the importing file's
            // directory and require that file — never a same-basename file
            // elsewhere. A missing target falls through to the NodeNext
            // retry below.
            if let Some(target) = relative_components(eff, caller.file) {
                let joined = target.join("/");
                if let Some(cands) = by_basename.get(seg) {
                    for c in cands {
                        if c.id == edge.from
                            || lang_family(c.language) != lang_family(caller.language)
                        {
                            continue;
                        }
                        if c.file == joined {
                            return Some((c.id, 0));
                        }
                    }
                }
            }
            if matches!(ext, "js" | "mjs" | "cjs" | "jsx") {
                eff = &eff[..eff.len() - ext.len() - 1];
                break 'shape1; // retry as a relative module path
            }
            return None;
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
                // the full parent path — see `parent_path`.
                // Normalize away `.`/`..` components so `../sub/b.h` competes
                // on its real path.
                // The import's own suffix with `..` APPLIED (`sub/../b.h` →
                // `b.h`, not `sub/b.h`): internal parent traversal pops, it
                // does not vanish.
                let mut suffix_parts: Vec<&str> = Vec::new();
                for t in eff.split(['/', '\\']).filter(|s| !s.is_empty()) {
                    if t == ".." {
                        suffix_parts.pop();
                    } else if t != "." {
                        suffix_parts.push(t);
                    }
                }
                let full = suffix_parts.join("/");
                let on_path = !full.is_empty()
                    && (c.file == full
                        || (full.contains('/') && c.file.ends_with(&format!("/{full}"))));
                // The importer-relative target — components resolved
                // against the includer's directory with `..` applied
                // (`sub/../b.h` from src/main.c means src/b.h) — outranks
                // the same suffix at the repo root: C searches the
                // includer's directory first.
                let local_hit =
                    relative_components(eff, caller.file).is_some_and(|t| c.file == t.join("/"));
                if full.contains('/') && !on_path {
                    continue; // `sub/b.h` never means an unrelated same-basename file
                }
                let score = 1
                    + u32::from(local_hit) * 48
                    + u32::from(parent_path(&c.file) == parent_path(caller.file)) * 32
                    + u32::from(on_path) * 16;
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
        // A non-relative file-naming import that finds no file of that
        // basename names a file we simply do not have.
        return None;
    }

    // Shape 2: the import names a module (or, for qualified `::` paths, a
    // symbol inside one). Normalize `crate::a::B`, `app.store.Repo`, `./b`
    // down to the candidate name.
    let seg = eff.rsplit(['/', '\\']).next().unwrap_or(eff);
    // Relative paths keep their full final component for the exact path
    // comparison below; the *lookup* name is its first dot segment, because
    // module symbols are named by the file stem before the first dot
    // (`b.test.ts` → `b`).
    let name = if relative {
        seg.split('.').next().unwrap_or(seg)
    } else {
        seg.rsplit("::")
            .next()
            .and_then(|s| s.rsplit('.').next())
            .unwrap_or(seg)
    };
    let segments: Vec<&str> = eff
        .split([':', '.', '/', '\\', ' '])
        .filter(|s| !s.is_empty())
        .collect();
    // Relative imports resolve as exact paths against the importer's
    // directory — `../lib/b` from `c/d/x.ts` means `c/lib/b`, not whichever
    // module is named `b` nearest by name.
    let mut cand_vec: Vec<&Cand> = name_index
        .get(name)
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    if relative || (segments.len() > 1 && !eff.contains("::")) {
        // A relative path or dotted package may target a directory module
        // (`./b` → b/index.ts, `import pkg.sub` → pkg/sub/__init__.py),
        // whose module symbol keeps the entry-file's name — the path
        // evidence below does the filtering.
        for alt in ["index", "__init__", "mod"] {
            if let Some(v) = name_index.get(alt) {
                cand_vec.extend(v.iter());
            }
        }
    }
    let cands = &cand_vec[..];

    if relative {
        let target = relative_components(eff, caller.file).unwrap_or_default();
        let mut best: Option<(SymbolId, u32)> = None;
        let mut ties = 0u16;
        for c in cands {
            if c.id == edge.from || lang_family(c.language) != lang_family(caller.language) {
                continue;
            }
            if c.kind != SymbolKind::Module {
                continue; // a relative path names a file/module, not a symbol
            }
            // Accept the module identity OR the explicit entry-file path
            // (`./b/index` names b/index.ts, whose module identity
            // collapses to `b`).
            let remapped = candidate_segments(c);
            let mut raw: Vec<String> = c
                .file
                .split(['/', '\\'])
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            raw.pop();
            if let Some(fname) = c.file.rsplit(['/', '\\']).next() {
                let stem = match fname.rsplit_once('.') {
                    Some((s, e)) if is_code_extension(e) => s.to_string(),
                    _ => fname.split('.').next().unwrap_or(fname).to_string(),
                };
                raw.push(stem);
            }
            if remapped != target && raw != target {
                continue;
            }
            // `./b` prefers the FILE b.ts over the directory module
            // b/index.ts when both normalize to the same path.
            let fstem = c
                .file
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&c.file)
                .split('.')
                .next()
                .unwrap_or("");
            let score = 1 + u32::from(Some(fstem) == target.last().map(|s| s.as_str())) * 2;
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

    let qualified = segments.len() > 1;
    // Whether BOTH crate-root files exist under the caller's crate prefix:
    // a module file's crate ownership (library vs binary) is then unknowable
    // without `mod` declarations, and root-file bindings must abstain.
    let (has_lib, has_main) = if qualified && caller.language == Language::Rust {
        let pref = crate_root_prefix(caller.file).join("/");
        let has_root = |n: &str| {
            let want = if pref.is_empty() {
                n.to_string()
            } else {
                format!("{pref}/{n}")
            };
            by_basename
                .get(n)
                .is_some_and(|v| v.iter().any(|c| c.file == want))
        };
        (has_root("lib.rs"), has_root("main.rs"))
    } else {
        (false, false)
    };
    if qualified
        && caller.language == Language::Rust
        && matches!(
            segments.first(),
            Some(&"std") | Some(&"core") | Some(&"alloc")
        )
    {
        return None; // Rust prelude roots are never in-repo (Rust only:
                     // `import core.utils` in Python may be a real local package)
    }

    let mut best: Option<(SymbolId, u32)> = None;
    let mut ties = 0u16;
    for c in cands {
        if c.id == edge.from || lang_family(c.language) != lang_family(caller.language) {
            continue;
        }
        if qualified {
            if !proven_for_candidate(eff, &segments, c, caller, has_lib, has_main) {
                continue;
            }
            // Python's `import a.b.c` imports a MODULE (a `from`-import of a
            // symbol is a different statement the extractor captures
            // separately): dotted python chains bind module candidates only.
            if caller.language == Language::Python
                && !eff.contains("::")
                && c.kind != SymbolKind::Module
            {
                continue;
            }
        } else if c.kind != SymbolKind::Module {
            continue; // a bare import names a module, never a function
        }
        let mut score = 1;
        if c.kind == SymbolKind::Module {
            score += 6; // an import names a module first, a symbol second
        }
        if parent_path(&c.file) == parent_path(caller.file) {
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

/// Resolve a relative import path against the importing file's directory,
/// applying `..` and `.` components: `../lib/b` from `c/d/x.ts` →
/// ["c", "lib", "b"]. `None` when the path escapes the repository root.
fn relative_components(raw: &str, caller_file: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = caller_file
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .map(String::from)
        .collect();
    parts.pop(); // the importing file itself
    for t in raw
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
    {
        if t == ".." {
            parts.pop()?; // escaping the repo root: not an in-repo path
        } else {
            parts.push(t.to_string());
        }
    }
    Some(parts)
}

/// The candidate's path as segments, file stem for the final component
/// (`src/fresh.rs` → ["src", "fresh"]).
fn candidate_segments(c: &Cand) -> Vec<String> {
    module_segments(&c.file, Some(c.language))
}

/// A file's module identity as path segments: directory components plus the
/// file stem, with `mod.rs` / `__init__.py` / `index.ts` named by their
/// directory. (`lib.rs` is deliberately NOT remapped: a python `pkg/lib.py`
/// is a real module named `lib`, not the directory's package root.)
/// A file's module identity as path segments, with language awareness: a Rust
/// (`src/lib.rs`, `src/main.rs`) IS the crate root directory's module, not a
/// module named `lib`/`main` beside it. (Python `pkg/lib.py` stays a real
/// module named `lib`.)
fn module_segments(path: &str, lang: Option<Language>) -> Vec<String> {
    let fname = path.rsplit(['/', '\\']).next().unwrap_or(path);
    // Strip only the final extension: `b.test.ts` stems to `b.test`.
    let stem = match fname.rsplit_once('.') {
        Some((s, e)) if is_code_extension(e) => s,
        _ => fname.split('.').next().unwrap_or(fname),
    };
    let mut segs: Vec<String> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    segs.pop(); // the filename itself
    if !segs.is_empty() {
        if matches!(stem, "mod" | "__init__" | "index") {
            return segs;
        }
        if lang == Some(Language::Rust) && matches!(stem, "lib" | "main") {
            return segs; // crate-root files ARE the root directory's module
        }
    }
    segs.push(stem.to_string());
    segs
}

/// Per-candidate in-repo evidence for a qualified import path: the parent
/// chain must anchor at the candidate's own path.
///
/// * `::`-separated (Rust-style): the full parent chain (after any
///   `crate::` root) must be a path suffix of the candidate — or, for a
///   *module* import (`use crate::a::b`), the chain including the final
///   name. `self::`/`super::` are caller-relative and anchor to the
///   importing file's own module ancestry, so `self::B` cannot bind a `B`
///   from an unrelated module.
/// * dotted/slash package paths: the full parent chain must be a path
///   suffix of the candidate's directory — `app.store.Repo` passes for
///   `app/store/Repo.java`, while `java.util.List` fails even beside a
///   `java/` directory (there is no `java/util` chain above the candidate).
fn proven_for_candidate(
    raw: &str,
    segments: &[&str],
    c: &Cand,
    caller: &Caller,
    has_lib: bool,
    has_main: bool,
) -> bool {
    let name = segments[segments.len() - 1];
    let mut parent: Vec<String> = segments[..segments.len() - 1]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // `crate::` is root-relative; `self::`/`super::` are caller-relative —
    // dropping them without anchoring would let `self::B` bind any
    // same-named symbol in the language.
    let mut supers = 0usize;
    let mut caller_relative = false;
    let mut crate_rooted = false;
    while matches!(
        parent.first().map(|s| s.as_str()),
        Some("crate") | Some("self") | Some("super")
    ) {
        match parent.remove(0).as_str() {
            "super" => {
                supers += 1;
                caller_relative = true;
            }
            "self" => caller_relative = true,
            "crate" => crate_rooted = true,
            _ => {}
        }
    }
    let bare = caller_relative && parent.is_empty();
    if caller_relative {
        let mut caller_segs = module_segments(caller.file, Some(caller.language));
        if supers > 0 {
            if caller_segs.len() < supers {
                return false; // `super::` past the repository root
            }
            caller_segs.truncate(caller_segs.len() - supers);
        }
        // `self::` at a root module and `super::` that climbed to the root
        // both leave the root itself as scope; an empty scope binds nothing.
        if caller_segs.is_empty() {
            return false;
        }
        // A bare `self::X`/`super::X` from a Rust crate-root file sees only
        // that file's own items — the sibling crate root (lib.rs vs main.rs)
        // is a different crate. PATHED imports (`self::child::X`) are
        // unaffected: they anchor at the root directory.
        if bare && caller.language == Language::Rust {
            let caller_base = caller.file.rsplit(['/', '\\']).next().unwrap_or("");
            if matches!(caller_base, "lib.rs" | "main.rs") {
                // Own items, or a direct child module (`use self::child;`).
                let prefix = crate_root_prefix(caller.file);
                let pref: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
                let csegs = candidate_segments(c);
                return c.file == caller.file
                    || (c.kind == SymbolKind::Module
                        && csegs.starts_with(&prefix[..])
                        && csegs.len() == pref.len() + 1);
            }
        }
        // Keep any remaining path (`super::child::B` from `src/a/x.rs` is
        // `src/a/child`, not merely `src/a`).
        let mut scope = caller_segs;
        scope.append(&mut parent);
        parent = scope;
    }

    if parent.is_empty() {
        // An empty scope is the CALLER's crate root. A *symbol* import
        // (`crate::B`) binds only a symbol in the caller's own crate-root
        // file (src/lib.rs, src/main.rs) — not a namesake in an unrelated
        // module or another workspace crate. A *module* import
        // (`use crate::a;`) binds the module directly under that root.
        let prefix = crate_root_prefix(caller.file);
        let pref: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
        let csegs = candidate_segments(c);
        if !csegs.starts_with(&prefix[..]) {
            return false; // another crate's tree (workspace sibling)
        }
        // The binary (main.rs) and library (lib.rs) are separate crates:
        // a `crate::` import binds only the caller's OWN root file — exactly
        // at the crate prefix (a nested `src/nested/lib.rs` is that nested
        // module's root, not the caller's). A module file abstains when both
        // roots exist (its crate ownership is unknowable), and binds the one
        // available root otherwise.
        let caller_base = caller.file.rsplit(['/', '\\']).next().unwrap_or("");
        let (want_root, ambiguous_owner) = match caller_base {
            "lib.rs" => (Some("lib.rs"), false),
            "main.rs" => (Some("main.rs"), false),
            _ if has_lib && has_main => (None, true),
            _ => (
                if has_lib {
                    Some("lib.rs")
                } else {
                    Some("main.rs")
                },
                false,
            ),
        };
        let root_file = c.file.rsplit(['/', '\\']).next().unwrap_or(&c.file);
        if !ambiguous_owner
            && want_root.is_some_and(|w| root_file == w)
            && csegs.starts_with(&prefix[..])
            && csegs.len() == prefix.len()
        {
            return true;
        }
        return c.kind == SymbolKind::Module
            && csegs.len() == pref.len() + 1
            && csegs.last().is_some_and(|s| s == name);
    }
    let parent: Vec<&str> = parent.iter().map(|s| s.as_str()).collect();
    let csegs = candidate_segments(c);
    let csegs: Vec<&str> = csegs.iter().map(|s| s.as_str()).collect();
    if csegs.len() < parent.len() {
        return false;
    }
    // `super::` that climbed to the crate root from a module file is the
    // same ambiguity as `crate::B`: with both roots present, a root-file
    // item could belong to either crate — abstain.
    if caller_relative
        && supers > 0
        && has_lib
        && has_main
        && !matches!(
            caller.file.rsplit(['/', '\\']).next().unwrap_or(""),
            "lib.rs" | "main.rs"
        )
    {
        let prefix = crate_root_prefix(caller.file);
        let pref: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
        let root_file = c.file.rsplit(['/', '\\']).next().unwrap_or("");
        if matches!(root_file, "lib.rs" | "main.rs") && parent[..] == pref[..] && csegs == pref[..]
        {
            return false;
        }
    }
    if raw.contains("::") {
        // The full parent chain must anchor at the candidate: a shorter
        // suffix would let `crate::a::b::B` bind any `B` under a `b.rs`
        // anywhere, or `serde::de::Deserialize` capture a namesake in a
        // local `de.rs`. For a *module* import (`use crate::a::b`) the final
        // name is itself part of the module path, so `parent + name` is also
        // an acceptable anchor. (Cost: an import qualified by the crate's
        // own *name* — `codescope::fresh::LiveGraph` from inside `codescope`
        // — stays unbound; use `crate::`-style paths.)
        let mut chain: Vec<&str> = parent.clone();
        chain.push(name);
        // The extended chain (`parent + name`) is a MODULE path: only a
        // module candidate may match it — otherwise `use crate::a::B` could
        // bind a struct `B` in `src/a/B.rs`.
        let chain_match = c.kind == SymbolKind::Module && csegs.ends_with(&chain);
        // A caller-relative scope is absolute within the caller's tree: a
        // same-suffix path in an unrelated workspace crate must not match.
        if caller_relative {
            return csegs == parent[..] || (chain_match && csegs == chain[..]);
        }
        // A crate-rooted path anchors at the CALLER's crate root (the last
        // `src` directory — workspaces nest crates under crates/foo/src):
        // the candidate's path must be exactly the root prefix plus the
        // matched chain.
        if crate_rooted {
            let prefix = crate_root_prefix(caller.file);
            let pref: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
            let anchored = |chain: &[&str]| {
                csegs.starts_with(&pref)
                    && csegs.ends_with(chain)
                    && csegs.len() == pref.len() + chain.len()
            };
            return anchored(&parent[..]) || (chain_match && anchored(&chain));
        }
        if csegs.ends_with(&parent[..]) {
            return true;
        }
        if chain_match {
            return true;
        }
        // A directory scope also matches its crate-root file
        // (`super::B` from `src/a.rs` scopes to `src/`, whose root module
        // is `src/lib.rs`).
        !csegs.is_empty()
            && matches!(csegs[csegs.len() - 1], "lib" | "main")
            && csegs[..csegs.len() - 1].ends_with(&parent[..])
    } else {
        // Dotted package paths name the package the class lives in: either
        // the parent chain is the candidate's entire module path (a symbol
        // inside it, `pkg.lib.run` → pkg/lib.py), or it is a suffix of the
        // candidate's directory with the file itself beyond it
        // (`app.store.Repo` → app/store/Repo.java).
        let stem = c
            .file
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&c.file)
            .split('.')
            .next()
            .unwrap_or("");
        let mut chain: Vec<&str> = parent.to_vec();
        chain.push(name);
        // A directory package (`import pkg.sub` → pkg/sub/__init__.py, whose
        // module identity is [pkg, sub]) matches the full imported chain.
        let package_match = c.kind == SymbolKind::Module && csegs.ends_with(&chain);
        // The whole-path rule (a symbol inside the module at `parent`) is a
        // type-system thing (Java/C#) — for Python it would bind a missing
        // submodule to its parent package.
        let symbol_in_module = caller.language != Language::Python && csegs.ends_with(&parent[..]);
        symbol_in_module
            || package_match
            || (csegs.len() > parent.len()
                && csegs[..csegs.len() - 1].ends_with(&parent[..])
                && stem == name)
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
                "src/demo.rs",
                "use crate::fresh::LiveGraph;\nfn main() { let _g: LiveGraph; }\n",
            ),
            ("src/fresh.rs", "pub struct LiveGraph;\n"),
        ]);
        let from = module_of(&g, "src/demo.rs");
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
    fn relative_file_import_with_missing_target_stays_unbound() {
        // `../lib/b.ts` with no c/lib/b.ts must not fall back to the
        // same-basename file in the importer's own directory.
        let g = graph(&[
            (
                "c/d/x.ts",
                "import { b } from \"../lib/b.ts\";\nexport function x() { return b(); }\n",
            ),
            ("c/d/b.ts", "export function b() { return 1; }\n"),
        ]);
        let from = module_of(&g, "c/d/x.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "../lib/b.ts with no target must not bind c/d/b.ts"
        );
    }

    #[test]
    fn relative_file_import_beats_same_basename_decoy() {
        let g = graph(&[
            (
                "c/d/x.ts",
                "import { b } from \"../lib/b.ts\";\nexport function x() { return b(); }\n",
            ),
            ("c/d/b.ts", "export function b() { return 1; }\n"),
            ("c/lib/b.ts", "export function b() { return 2; }\n"),
        ]);
        let from = module_of(&g, "c/d/x.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("exact path resolves")).unwrap();
        assert_eq!(target.file, "c/lib/b.ts");
    }

    #[test]
    fn deep_crate_path_requires_full_ancestry() {
        // `crate::a::b::B` must not bind a B that merely sits in some `b.rs`;
        // the a/b chain has to be real.
        let g = graph(&[
            (
                "src/x.rs",
                "use crate::a::b::B;\nfn main() { let _b: B; }\n",
            ),
            ("src/b.rs", "pub struct B;\n"),
            ("src/a/b.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("a/b.rs wins over plain b.rs"))
            .unwrap();
        assert_eq!(target.file, "src/a/b.rs");
    }

    #[test]
    fn external_crate_path_with_namesake_in_matched_module_stays_unbound() {
        // Even a namesake Deserialize living IN a de.rs must not capture
        // `serde::de::Deserialize`: serde:: is an external root.
        let g = graph(&[
            (
                "src/x.rs",
                "use serde::de::Deserialize;\nfn main() { let _d: Deserialize; }\n",
            ),
            ("src/de.rs", "pub struct Deserialize;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "serde::de::Deserialize must not capture the in-repo de::Deserialize"
        );
    }

    #[test]
    fn std_root_guard_is_rust_only() {
        // `core` is a Rust prelude root, but in Python `import core.utils`
        // may name a real local package.
        let g = graph(&[
            ("core/utils.py", "def helper():\n    return 1\n"),
            (
                "app.py",
                "import core.utils\n\ndef go():\n    return core.utils.helper()\n",
            ),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("core.utils binds in Python")).unwrap();
        assert_eq!(target.file, "core/utils.py");
    }

    #[test]
    fn nested_module_import_binds() {
        // `use crate::a::b;` imports a MODULE: the final name is part of the
        // module path, not a symbol inside `a`.
        let g = graph(&[
            (
                "src/x.rs",
                "use crate::a::b;\nfn main() { let _t: b::Thing; }\n",
            ),
            ("src/a/b.rs", "pub struct Thing;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("crate::a::b binds")).unwrap();
        assert_eq!(target.file, "src/a/b.rs");
    }

    #[test]
    fn self_scoped_import_stays_in_the_callers_module() {
        // `self::B` must bind only within the caller's own module — not a
        // same-named B in an unrelated file.
        let g = graph(&[
            ("src/a.rs", "use self::B;\nfn run() { let _b: B; }\n"),
            ("src/unrelated.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/a.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "self::B with no local B must not bind src/unrelated.rs"
        );
    }

    #[test]
    fn super_scoped_import_binds_the_parent_module() {
        let g = graph(&[
            ("src/a/b.rs", "use super::B;\npub fn run() { let _b: B; }\n"),
            ("src/a.rs", "pub struct B;\n"),
            ("src/other.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/a/b.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("super::B binds src/a.rs")).unwrap();
        assert_eq!(target.file, "src/a.rs");
    }

    #[test]
    fn super_with_remaining_path_binds_that_submodule() {
        // `super::child::B` from src/a/x.rs means src/a/child — not src/a.
        let g = graph(&[
            (
                "src/a/x.rs",
                "use super::child::B;\nfn run() { let _b: B; }\n",
            ),
            ("src/a.rs", "pub struct B;\n"),
            ("src/a/child.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/a/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("super::child binds src/a/child.rs"))
            .unwrap();
        assert_eq!(target.file, "src/a/child.rs");
    }

    #[test]
    fn crate_root_import_binds_only_root_files() {
        // `crate::B` names the crate root: a namesake in an unrelated
        // module must not capture it; one in the root lib.rs binds.
        let g = graph(&[
            ("src/lib.rs", "pub struct B;\n"),
            ("src/unrelated.rs", "pub struct B;\n"),
            ("src/x.rs", "use crate::B;\nfn main() { let _b: B; }\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("crate::B binds the root B")).unwrap();
        assert_eq!(target.file, "src/lib.rs");
    }

    #[test]
    fn root_level_module_import_binds() {
        // `use crate::a;` imports the root-level module src/a.rs — not a
        // symbol at the crate root.
        let g = graph(&[
            (
                "src/x.rs",
                "use crate::a;\nfn main() { let _m: a::Thing; }\n",
            ),
            ("src/a.rs", "pub struct Thing;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("crate::a binds src/a.rs")).unwrap();
        assert_eq!(target.file, "src/a.rs");
    }

    #[test]
    fn duplicate_directory_basenames_do_not_confuse_affinity() {
        // app/include/ and vendor/include/ share their final segment; the
        // include from app/include must bind app's header, not whichever
        // file got the lower symbol id.
        let g = graph(&[
            ("app/include/head.h", "int app_head(void);\n"),
            ("vendor/include/head.h", "int vendor_head(void);\n"),
            (
                "app/include/main.c",
                "#include \"head.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "app/include/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "head.h")
            .unwrap();
        let target = g.symbol(e.to.expect("same full directory wins")).unwrap();
        assert_eq!(target.file, "app/include/head.h");
    }

    #[test]
    fn multi_component_include_with_missing_path_stays_unbound() {
        // `#include "missing/b.h"` with no file at that path must not fall
        // back to an unrelated same-basename header.
        let g = graph(&[
            ("main/b.h", "int main_b(void);\n"),
            (
                "main/main.c",
                "#include \"missing/b.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "main/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(e.to.is_none(), "missing/b.h must not bind main/b.h");
    }

    #[test]
    fn crate_root_module_rejects_nested_namesake() {
        // `use crate::a;` with only src/nested/a.rs present stays unbound —
        // a root-level module is not a nested one.
        let g = graph(&[
            (
                "src/x.rs",
                "use crate::a;\nfn main() { let _m: a::Thing; }\n",
            ),
            ("src/nested/a.rs", "pub struct Thing;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(e.to.is_none(), "crate::a must not bind src/nested/a.rs");
    }

    #[test]
    fn dotted_relative_module_import_binds() {
        // `./b.test` targets b.test.ts; the lookup name is the stem before
        // the first dot (`b`), and the exact path comparison carries the
        // dotted basename.
        let g = graph(&[
            (
                "a.ts",
                "import { b } from \"./b.test\";\nexport function a() { return b(); }\n",
            ),
            ("b.test.ts", "export function b() { return 1; }\n"),
        ]);
        let from = module_of(&g, "a.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("./b.test binds b.test.ts")).unwrap();
        assert_eq!(target.file, "b.test.ts");
    }

    #[test]
    fn header_beside_importer_outranks_root_namesake() {
        // `#include "head.h"` from src/main.c: the header in the importer's
        // directory wins over a same-named file at the exact import path
        // root (C searches the includer's directory first).
        let g = graph(&[
            ("head.h", "int root_head(void);\n"),
            ("src/head.h", "int src_head(void);\n"),
            (
                "src/main.c",
                "#include \"head.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "src/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "head.h")
            .unwrap();
        let target = g.symbol(e.to.expect("src/head.h wins")).unwrap();
        assert_eq!(target.file, "src/head.h");
    }

    #[test]
    fn module_chain_binds_the_module_not_a_symbol_namesake() {
        // `use crate::a::B;` where src/a/B.rs holds a struct B: the module
        // path `crate::a::B` IS the file src/a/B.rs, so the import binds its
        // module symbol — never the struct inside it (that would be
        // `crate::a::B::B`).
        let g = graph(&[
            ("src/x.rs", "use crate::a::B;\nfn main() { let _b: B; }\n"),
            ("src/a/B.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("the module crate::a::B binds"))
            .unwrap();
        assert_eq!(target.file, "src/a/B.rs");
        assert_eq!(
            target.kind,
            SymbolKind::Module,
            "the struct namesake must not capture the import"
        );
    }

    #[test]
    fn nested_workspace_crate_paths_bind() {
        // A workspace nests crates under crates/foo/src — `crate::a::B`
        // from inside that crate resolves against ITS src root.
        let g = graph(&[
            (
                "crates/foo/src/x.rs",
                "use crate::a::B;\nfn run() { let _b: B; }\n",
            ),
            ("crates/foo/src/a.rs", "pub struct B;\n"),
            ("crates/bar/src/a.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "crates/foo/src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("binds the own-crate a.rs")).unwrap();
        assert_eq!(target.file, "crates/foo/src/a.rs");
    }

    #[test]
    fn relative_import_binds_directory_index_module() {
        // `./b` may target b/index.ts, whose module is named by the
        // directory.
        let g = graph(&[
            (
                "a.ts",
                "import { b } from \"./b\";\nexport function a() { return b(); }\n",
            ),
            ("b/index.ts", "export function b() { return 1; }\n"),
        ]);
        let from = module_of(&g, "a.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("./b binds b/index.ts")).unwrap();
        assert_eq!(target.file, "b/index.ts");
    }

    #[test]
    fn multi_component_include_prefers_importer_relative_target() {
        // `#include "sub/b.h"` from src/main.c: src/sub/b.h (the
        // importer-relative path) beats a same-suffix header at the root.
        let g = graph(&[
            ("sub/b.h", "int root_b(void);\n"),
            ("src/sub/b.h", "int src_b(void);\n"),
            (
                "src/main.c",
                "#include \"sub/b.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "src/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports && e.to_name == "sub/b.h")
            .unwrap();
        let target = g.symbol(e.to.expect("src/sub/b.h wins")).unwrap();
        assert_eq!(target.file, "src/sub/b.h");
    }

    #[test]
    fn crate_root_symbol_does_not_cross_workspace_crates() {
        // `crate::B` from crates/foo must not bind the sibling crate's
        // root file when its own has no B.
        let g = graph(&[
            (
                "crates/foo/src/x.rs",
                "use crate::B;\nfn run() { let _b: B; }\n",
            ),
            ("crates/bar/src/lib.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "crates/foo/src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "crate::B must not bind the sibling crate's lib.rs"
        );
    }

    #[test]
    fn crate_root_symbol_binds_own_root_file() {
        let g = graph(&[
            (
                "crates/foo/src/x.rs",
                "use crate::B;\nfn run() { let _b: B; }\n",
            ),
            ("crates/foo/src/lib.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "crates/foo/src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("own crate root binds")).unwrap();
        assert_eq!(target.file, "crates/foo/src/lib.rs");
    }

    #[test]
    fn self_scoped_from_root_file_reaches_submodules() {
        // `self::child::B` from src/lib.rs scopes to the crate root, so
        // src/child.rs qualifies (lib.rs is the root module, not a module
        // named `lib` beside it).
        let g = graph(&[
            (
                "src/lib.rs",
                "use self::child::B;\nfn run() { let _b: B; }\n",
            ),
            ("src/child.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/lib.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("self::child binds src/child.rs"))
            .unwrap();
        assert_eq!(target.file, "src/child.rs");
    }

    #[test]
    fn self_scoped_from_main_does_not_reach_the_library() {
        // src/main.rs is a separate (binary) crate root: `self::B` there
        // cannot reach the library crate's items in src/lib.rs.
        let g = graph(&[
            ("src/main.rs", "use self::B;\nfn main() { let _b: B; }\n"),
            ("src/lib.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/main.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "self::B from main.rs must not bind the library's B"
        );
    }

    #[test]
    fn python_dotted_chain_binds_only_modules() {
        // `import pkg.lib.run` imports the MODULE run (pkg/lib/run.py) —
        // binding the function run in pkg/lib.py would be `from pkg.lib
        // import run`, a different statement.
        let g = graph(&[
            ("pkg/lib.py", "def run(x):\n    return x\n"),
            ("app.py", "import pkg.lib.run\n\ndef go():\n    return 1\n"),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "import pkg.lib.run must not bind the function run in pkg/lib.py"
        );
    }

    #[test]
    fn python_dotted_chain_binds_a_real_submodule() {
        let g = graph(&[
            ("pkg/lib/run.py", "def go(x):\n    return x\n"),
            (
                "app.py",
                "import pkg.lib.run\n\ndef main():\n    return pkg.lib.run.go(1)\n",
            ),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("pkg.lib.run binds pkg/lib/run.py"))
            .unwrap();
        assert_eq!(target.file, "pkg/lib/run.py");
        assert_eq!(target.kind, SymbolKind::Module);
    }

    #[test]
    fn crate_import_from_main_does_not_reach_the_library() {
        // `crate::B` from the binary root binds only main.rs's own B —
        // lib.rs is a separate crate.
        let g = graph(&[
            ("src/main.rs", "use crate::B;\nfn main() { let _b: B; }\n"),
            ("src/lib.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/main.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(e.to.is_none(), "crate::B from main.rs must not bind lib.rs");

        let g2 = graph(&[
            ("src/main.rs", "use crate::B;\nfn main() { let _b: B; }\n"),
            ("src/main.rs", "fn placeholder() {}\n"),
        ]);
        let _ = g2; // single-file case covered by the next fixture
        let g3 = graph(&[
            (
                "src/main.rs",
                "use crate::B;\nfn main() { let _b: B; }\npub struct B;\n",
            ),
            ("src/lib.rs", "pub struct B;\n"),
        ]);
        let from3 = module_of(&g3, "src/main.rs");
        let e3 = g3
            .out_edges(from3)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g3.symbol(e3.to.expect("own root B binds")).unwrap();
        assert_eq!(target.file, "src/main.rs");
    }

    #[test]
    fn relative_import_prefers_file_over_directory_module() {
        // `./b` with BOTH b.ts and b/index.ts present: the file wins.
        let g = graph(&[
            (
                "a.ts",
                "import { b } from \"./b\";\nexport function a() { return b(); }\n",
            ),
            ("b.ts", "export function b() { return 1; }\n"),
            ("b/index.ts", "export function b() { return 2; }\n"),
        ]);
        let from = module_of(&g, "a.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("b.ts beats b/index.ts")).unwrap();
        assert_eq!(target.file, "b.ts");
    }

    #[test]
    fn internal_dotdot_include_resolves_components() {
        // `#include "sub/../b.h"` from src/main.c means src/b.h — the `..`
        // pops `sub`, it does not simply vanish.
        let g = graph(&[
            ("src/sub/b.h", "int sub_b(void);\n"),
            ("src/b.h", "int top_b(void);\n"),
            (
                "src/main.c",
                "#include \"sub/../b.h\"\nint main(void) { return 0; }\n",
            ),
        ]);
        let from = module_of(&g, "src/main.c");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("src/b.h wins")).unwrap();
        assert_eq!(target.file, "src/b.h");
    }

    #[test]
    fn dotted_chain_rejects_sibling_module_namesake() {
        // `pkg.lib.run` must not bind a run that lives in pkg/lib/unrelated.py
        // — its module is pkg.lib.unrelated.
        let g = graph(&[
            ("pkg/lib/unrelated.py", "def run(x):\n    return x\n"),
            (
                "app.py",
                "import pkg.lib.run\n\ndef go():\n    return pkg.lib.run(1)\n",
            ),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "pkg.lib.run must not bind pkg/lib/unrelated.py's run"
        );
    }

    #[test]
    fn self_scoped_from_main_reaches_child_modules() {
        // `self::child::B` from src/main.rs anchors at the crate root, so
        // src/child.rs qualifies (the binary root owns its child modules).
        let g = graph(&[
            (
                "src/main.rs",
                "use self::child::B;\nfn main() { let _b: B; }\n",
            ),
            ("src/child.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/main.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("self::child binds src/child.rs"))
            .unwrap();
        assert_eq!(target.file, "src/child.rs");
    }

    #[test]
    fn bare_self_import_from_root_binds_child_module() {
        // `use self::child;` from src/main.rs imports the child module
        // itself (not an item of the sibling library root).
        let g = graph(&[
            (
                "src/main.rs",
                "use self::child;\nfn main() { let _m: child::Thing; }\n",
            ),
            ("src/child.rs", "pub struct Thing;\n"),
        ]);
        let from = module_of(&g, "src/main.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g
            .symbol(e.to.expect("self::child binds src/child.rs"))
            .unwrap();
        assert_eq!(target.file, "src/child.rs");
        assert_eq!(target.kind, SymbolKind::Module);
    }

    #[test]
    fn crate_import_from_module_abstains_when_both_roots_exist() {
        // With lib.rs AND main.rs both present, a module file's crate
        // ownership is unknowable: `crate::B` from src/child.rs abstains
        // rather than guessing the library root.
        let g = graph(&[
            ("src/child.rs", "use crate::B;\nfn run() { let _b: B; }\n"),
            ("src/lib.rs", "pub struct B;\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let from = module_of(&g, "src/child.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "crate::B from a module file must abstain when both roots exist"
        );
    }

    #[test]
    fn binary_only_tree_binds_its_root() {
        // No lib.rs at all: `crate::B` from a module of the binary crate
        // binds main.rs's B.
        let g = graph(&[
            ("src/child.rs", "use crate::B;\nfn run() { let _b: B; }\n"),
            ("src/main.rs", "pub struct B;\nfn main() {}\n"),
        ]);
        let from = module_of(&g, "src/child.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("the only root binds")).unwrap();
        assert_eq!(target.file, "src/main.rs");
    }

    #[test]
    fn nested_lib_rs_is_not_the_callers_root() {
        // `crate::B` from src/x.rs must not bind a B in src/nested/lib.rs —
        // that lib.rs is the nested module's root.
        let g = graph(&[
            ("src/x.rs", "use crate::B;\nfn run() { let _b: B; }\n"),
            ("src/nested/lib.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/x.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(e.to.is_none(), "nested lib.rs is not the crate root");
    }

    #[test]
    fn super_to_root_abstains_when_both_roots_exist() {
        // `super::B` from src/child.rs climbing to the crate root: with both
        // lib.rs and main.rs present, the root item's crate is ambiguous.
        let g = graph(&[
            ("src/child.rs", "use super::B;\nfn run() { let _b: B; }\n"),
            ("src/lib.rs", "pub struct B;\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let from = module_of(&g, "src/child.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "super::B to the root must abstain when both roots exist"
        );
    }

    #[test]
    fn super_to_root_binds_the_single_root() {
        let g = graph(&[
            ("src/child.rs", "use super::B;\nfn run() { let _b: B; }\n"),
            ("src/main.rs", "pub struct B;\nfn main() {}\n"),
        ]);
        let from = module_of(&g, "src/child.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("the single root binds")).unwrap();
        assert_eq!(target.file, "src/main.rs");
    }

    #[test]
    fn explicit_entry_file_import_binds() {
        // `./b/index` names b/index.ts explicitly.
        let g = graph(&[
            (
                "a.ts",
                "import { b } from \"./b/index\";\nexport function a() { return b(); }\n",
            ),
            ("b/index.ts", "export function b() { return 1; }\n"),
        ]);
        let from = module_of(&g, "a.ts");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("./b/index binds b/index.ts")).unwrap();
        assert_eq!(target.file, "b/index.ts");
    }

    #[test]
    fn nested_lib_does_not_make_ownership_ambiguous() {
        // src/nested/lib.rs is not the caller's crate root: with only
        // src/main.rs at the root, crate::B from a module file binds it.
        let g = graph(&[
            ("src/child.rs", "use crate::B;\nfn run() { let _b: B; }\n"),
            ("src/main.rs", "pub struct B;\nfn main() {}\n"),
            ("src/nested/lib.rs", "pub mod x { pub struct Noop; }\n"),
        ]);
        let from = module_of(&g, "src/child.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("the real root binds")).unwrap();
        assert_eq!(target.file, "src/main.rs");
    }

    #[test]
    fn caller_relative_scope_is_exact() {
        // `self::child::B` from src/a.rs must not bind a same-suffix path in
        // an unrelated tree when the local target is absent.
        let g = graph(&[
            ("src/a.rs", "use self::child::B;\nfn run() { let _b: B; }\n"),
            ("vendor/other/src/a/child.rs", "pub struct B;\n"),
        ]);
        let from = module_of(&g, "src/a.rs");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "self::child::B must not bind vendor/other/src/a/child.rs"
        );
    }

    #[test]
    fn python_dotted_import_binds_directory_package() {
        // `import pkg.sub` resolves the package pkg/sub (__init__.py), whose
        // module identity is [pkg, sub].
        let g = graph(&[
            ("pkg/sub/__init__.py", "\n"),
            ("app.py", "import pkg.sub\n\ndef go():\n    return 1\n"),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        let target = g.symbol(e.to.expect("pkg.sub binds the package")).unwrap();
        assert_eq!(target.file, "pkg/sub/__init__.py");
        assert_eq!(target.kind, SymbolKind::Module);
    }

    #[test]
    fn python_missing_submodule_stays_unbound() {
        // `import pkg.missing` with only the package present must not bind
        // the parent package as the nonexistent submodule.
        let g = graph(&[
            ("pkg/__init__.py", "\n"),
            ("app.py", "import pkg.missing\n\ndef go():\n    return 1\n"),
        ]);
        let from = module_of(&g, "app.py");
        let e = g
            .out_edges(from)
            .find(|e| e.kind == EdgeKind::Imports)
            .unwrap();
        assert!(
            e.to.is_none(),
            "import pkg.missing must not bind the pkg package itself"
        );
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
