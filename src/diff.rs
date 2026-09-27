//! Change-impact analysis (ADR-0016): from a git diff to the symbols it
//! touches, what depends on them, and which tests exercise them.
//!
//! This is the question an agent (or reviewer) actually has after editing:
//! *"what did I just change, what could that break, and which tests should I
//! run?"* — answered from the structural graph instead of by re-reading files.

use crate::domain::{CodeGraph, EdgeKind, Symbol, SymbolId, SymbolKind};
use crate::query::{est_tokens, reach, SymbolView};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

/// Changed line ranges (1-based, inclusive, in the *new* file) per path.
pub type ChangeSet = BTreeMap<String, Vec<(u32, u32)>>;

/// Parse a unified diff (any context size) into per-file changed line ranges
/// on the new side. Pure deletions are recorded as a 1-line range at the
/// deletion point so the enclosing symbol is still attributed.
pub fn parse_unified_diff(diff: &str) -> ChangeSet {
    let mut out: ChangeSet = BTreeMap::new();
    let mut current: Option<String> = None;
    // For hunks with context we walk lines to find exactly which changed.
    let mut new_line: u32 = 0;
    let mut in_hunk = false;
    let mut pending_del: bool = false;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            in_hunk = false;
            let path = rest.trim();
            current = if path == "/dev/null" {
                None
            } else {
                Some(path.strip_prefix("b/").unwrap_or(path).to_string())
            };
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("diff --git") {
            in_hunk = false;
            continue;
        }
        if let Some(h) = line.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@
            let plus = h.split_whitespace().find(|t| t.starts_with('+'));
            if let Some(plus) = plus {
                let spec = &plus[1..];
                let (start, _) = spec.split_once(',').unwrap_or((spec, "1"));
                new_line = start.parse().unwrap_or(0);
                in_hunk = true;
                pending_del = false;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let Some(file) = current.as_ref() else {
            continue;
        };
        match line.as_bytes().first() {
            Some(b'+') => {
                push_range(out.entry(file.clone()).or_default(), new_line, new_line);
                new_line += 1;
                pending_del = false;
            }
            Some(b'-') => {
                if !pending_del {
                    let at = new_line.max(1);
                    push_range(out.entry(file.clone()).or_default(), at, at);
                    pending_del = true;
                }
            }
            Some(b'\\') => {} // "\ No newline at end of file"
            _ => {
                new_line += 1;
                pending_del = false;
            }
        }
    }
    out
}

/// Append a range, merging with the previous one when adjacent/overlapping.
fn push_range(v: &mut Vec<(u32, u32)>, start: u32, end: u32) {
    if let Some(last) = v.last_mut() {
        if start <= last.1 + 1 && end >= last.0 {
            last.0 = last.0.min(start);
            last.1 = last.1.max(end);
            return;
        }
    }
    v.push((start, end));
}

/// Collect the working-tree change set against `base` (default `HEAD`),
/// including untracked (not ignored) files as fully changed.
pub fn git_changes(root: &Path, base: Option<&str>) -> anyhow::Result<ChangeSet> {
    let base = base.unwrap_or("HEAD");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--unified=0",
            base,
            "--",
        ])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git diff failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut changes = parse_unified_diff(&String::from_utf8_lossy(&out.stdout));

    let untracked = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()?;
    if untracked.status.success() {
        for path in String::from_utf8_lossy(&untracked.stdout).lines() {
            changes.insert(path.to_string(), vec![(1, u32::MAX)]);
        }
    }
    Ok(changes)
}

/// Heuristic: does this symbol live in (or constitute) test code?
pub fn is_test(graph: &CodeGraph, s: &Symbol) -> bool {
    let f = s.file.as_str();
    let fname = f.rsplit('/').next().unwrap_or(f);
    let test_path = f.starts_with("tests/")
        || f.starts_with("test/")
        || f.contains("/tests/")
        || f.contains("/test/")
        || f.contains("/__tests__/")
        || fname.starts_with("test_")
        || fname.ends_with("_test.go")
        || fname.ends_with("_test.py")
        || fname.ends_with("_test.rs")
        || fname.contains(".test.")
        || fname.contains(".spec.")
        || f.starts_with("spec/")
        || f.contains("/spec/")
        || fname.ends_with("_spec.rb")
        || fname.ends_with("_test.rb")
        || fname.ends_with("Test.java")
        || fname.ends_with("Tests.java")
        || fname.ends_with("Test.cs")
        || fname.ends_with("Tests.cs")
        || fname.ends_with("_test.c")
        || fname.ends_with("_test.cc")
        || fname.ends_with("_test.cpp")
        || fname.ends_with("_unittest.cc");
    if test_path {
        return s.kind != SymbolKind::Module;
    }
    if s.kind.is_callable() && (s.name.starts_with("test_") || s.name.starts_with("Test")) {
        return true;
    }
    // Rust convention: anything inside a `mod tests`.
    let mut cur = s.container;
    let mut hops = 0;
    while let (Some(id), true) = (cur, hops < 16) {
        match graph.symbol(id) {
            Some(c)
                if c.kind == SymbolKind::Module && c.name == "tests" && c.span.line_start > 0 =>
            {
                return s.kind.is_callable()
            }
            Some(c) => cur = c.container,
            None => break,
        }
        hops += 1;
    }
    false
}

/// Summary counts for quick triage.
#[derive(Debug, Clone, Serialize)]
pub struct RiskSummary {
    pub files_changed: usize,
    pub symbols_changed: usize,
    pub symbols_impacted: usize,
    pub tests_impacted: usize,
    /// Distinct files containing impacted (non-test) symbols.
    pub files_impacted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffImpact {
    pub base: String,
    pub risk: RiskSummary,
    pub changed_files: Vec<String>,
    /// Innermost symbols whose span overlaps a changed line.
    pub changed_symbols: Vec<SymbolView>,
    /// Tests that are changed or transitively depend on a changed symbol —
    /// the ones worth running.
    pub impacted_tests: Vec<SymbolView>,
    /// Non-test symbols that transitively depend on a changed symbol.
    pub impacted: Vec<SymbolView>,
    pub truncated: bool,
}

/// Map a change set onto the graph and compute its transitive impact.
pub fn diff_impact(
    graph: &CodeGraph,
    changes: &ChangeSet,
    base: &str,
    max_tokens: usize,
) -> DiffImpact {
    // 1. Changed symbols: for each changed range, the innermost overlapping
    //    non-module symbol(s). Changes outside any symbol attribute to the module.
    let mut changed: Vec<SymbolId> = Vec::new();
    let mut seen: HashSet<SymbolId> = HashSet::new();
    for (file, ranges) in changes {
        let syms: Vec<&Symbol> = graph
            .by_file(file)
            .iter()
            .filter_map(|id| graph.symbol(*id))
            .collect();
        for &(a, b) in ranges {
            let overlapping: Vec<&Symbol> = syms
                .iter()
                .copied()
                .filter(|s| s.kind != SymbolKind::Import)
                .filter(|s| s.span.line_start <= b && s.span.line_end >= a)
                .collect();
            let non_module: Vec<&Symbol> = overlapping
                .iter()
                .copied()
                .filter(|s| s.kind != SymbolKind::Module)
                .collect();
            let pool = if non_module.is_empty() {
                overlapping
            } else {
                non_module
            };
            // Keep only innermost: drop any symbol that strictly contains another in the pool.
            for s in &pool {
                let has_inner = pool.iter().any(|o| {
                    o.id != s.id
                        && o.span.byte_start >= s.span.byte_start
                        && o.span.byte_end <= s.span.byte_end
                        && (o.span.byte_end - o.span.byte_start)
                            < (s.span.byte_end - s.span.byte_start)
                        && o.span.line_start <= b
                        && o.span.line_end >= a
                });
                if !has_inner && seen.insert(s.id) {
                    changed.push(s.id);
                }
            }
        }
    }

    // 2. Transitive dependents.
    let kinds = [
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Imports,
        EdgeKind::Defines,
    ];
    let reached = reach(graph, &changed, &kinds, true, u32::MAX);

    // 3. Split into tests vs. production code.
    let mut tests: Vec<SymbolView> = Vec::new();
    let mut impacted: Vec<SymbolView> = Vec::new();
    let mut impacted_files: HashSet<&str> = HashSet::new();
    for id in &changed {
        if let Some(s) = graph.symbol(*id) {
            if is_test(graph, s) {
                let mut v = SymbolView::from_symbol(s);
                v.depth = Some(0);
                tests.push(v);
            }
        }
    }
    for (id, depth) in &reached {
        let Some(s) = graph.symbol(*id) else { continue };
        if s.kind == SymbolKind::Module {
            continue;
        }
        let mut v = SymbolView::from_symbol(s);
        v.depth = Some(*depth);
        if is_test(graph, s) {
            tests.push(v);
        } else {
            impacted_files.insert(s.file.as_str());
            impacted.push(v);
        }
    }

    let risk = RiskSummary {
        files_changed: changes.len(),
        symbols_changed: changed.len(),
        symbols_impacted: impacted.len(),
        tests_impacted: tests.len(),
        files_impacted: impacted_files.len(),
    };

    // 4. Budget: changed symbols first, then tests, then impacted.
    let mut used = est_tokens(128 + changes.keys().map(|k| k.len() + 4).sum::<usize>());
    let mut truncated = false;
    let mut take = |items: Vec<SymbolView>| -> Vec<SymbolView> {
        let mut kept = Vec::new();
        for v in items {
            let cost = v.est();
            if used + cost > max_tokens && !truncated {
                truncated = true;
            }
            if truncated {
                break;
            }
            used += cost;
            kept.push(v);
        }
        kept
    };
    let changed_symbols = take(
        changed
            .iter()
            .filter_map(|id| graph.symbol(*id))
            .map(SymbolView::from_symbol)
            .collect(),
    );
    let impacted_tests = take(tests);
    let impacted = take(impacted);

    DiffImpact {
        base: base.to_string(),
        risk,
        changed_files: changes.keys().cloned().collect(),
        changed_symbols,
        impacted_tests,
        impacted,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;
    use crate::resolve;

    fn graph_from(files: &[(&str, &str)]) -> CodeGraph {
        let mut g = CodeGraph::new();
        for (path, src) in files {
            let lang = Language::from_path(Path::new(path)).unwrap();
            g.upsert_file(extract(lang, path, src, 0));
        }
        g.reindex();
        resolve::resolve(&mut g);
        g
    }

    #[test]
    fn parses_zero_context_hunks() {
        let d = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,0 +4,2 @@\n+x\n+y\n@@ -10 +12 @@\n-old\n+new\n@@ -20,2 +21,0 @@\n-gone\n-gone\n";
        let c = parse_unified_diff(d);
        assert_eq!(c["src/a.rs"], vec![(4, 5), (12, 12), (21, 21)]);
    }

    #[test]
    fn parses_context_hunks_and_new_files() {
        let d = "--- a/x.py\n+++ b/x.py\n@@ -1,4 +1,4 @@\n ctx\n-a\n+b\n ctx\n ctx\n--- /dev/null\n+++ b/n.go\n@@ -0,0 +1,2 @@\n+package n\n+func F() {}\n--- a/del.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fn x() {}\n";
        let c = parse_unified_diff(d);
        assert_eq!(c["x.py"], vec![(2, 2)]);
        assert_eq!(c["n.go"], vec![(1, 2)]);
        assert!(!c.contains_key("del.rs"));
    }

    #[test]
    fn impact_finds_dependents_and_tests() {
        let g = graph_from(&[
            (
                "src/lib.rs",
                "fn core() {\n    1;\n}\nfn api() { core(); }\nfn unrelated() {}\n",
            ),
            ("tests/it.rs", "fn test_api() { api(); }\n"),
        ]);
        let mut cs = ChangeSet::new();
        cs.insert("src/lib.rs".into(), vec![(2, 2)]);
        let r = diff_impact(&g, &cs, "HEAD", 4000);
        let changed: Vec<&str> = r.changed_symbols.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(changed, vec!["core"]);
        let impacted: Vec<&str> = r.impacted.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(impacted, vec!["api"]);
        let tests: Vec<&str> = r.impacted_tests.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(tests, vec!["test_api"]);
        assert!(!impacted.contains(&"unrelated"));
        assert_eq!(r.risk.tests_impacted, 1);
    }

    #[test]
    fn rust_mod_tests_are_tests() {
        let g = graph_from(&[(
            "src/a.rs",
            "fn f() {}\nmod tests {\n    fn checks_f() { super::f(); }\n}\n",
        )]);
        let mut cs = ChangeSet::new();
        cs.insert("src/a.rs".into(), vec![(1, 1)]);
        let r = diff_impact(&g, &cs, "HEAD", 4000);
        assert_eq!(r.impacted_tests.len(), 1);
        assert_eq!(r.impacted_tests[0].name, "checks_f");
    }
}
