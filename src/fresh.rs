//! Freshness (ADR-0020): keep the in-memory graph in sync with the working
//! tree without the agent (or user) having to remember to re-index.
//!
//! * **Staleness check** — an ignore-aware walk + `stat` (no file reads):
//!   stale if any supported file's mtime is at/after the last index start, or
//!   the set of files changed. ~ms on thousands of files.
//! * **Incremental patch** — re-extract only changed files (hash-verified by
//!   the indexer), splice them into the live graph and re-resolve in memory,
//!   instead of decoding every record from the store again.
//! * **Throttling** — the check runs at most once per `check_interval`, so a
//!   burst of agent queries pays for it once.

use crate::domain::CodeGraph;
use crate::index::{self, Delta, IndexStats};
use crate::store::Store;
use crate::{index_path, resolve, walker};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Result of a cheap (stat-only) staleness check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Staleness {
    pub stale: bool,
    /// Files modified since the last index started.
    pub modified: usize,
    /// Files on disk the index doesn't know.
    pub added: usize,
    /// Indexed files no longer on disk.
    pub removed: usize,
}

use crate::index::MTIME_SLACK;

/// Compare the working tree against what an index knows.
pub fn check(root: &Path, known: &HashSet<String>, indexed_at: SystemTime) -> Staleness {
    let indexed_at = indexed_at.checked_sub(MTIME_SLACK).unwrap_or(UNIX_EPOCH);
    let mut st = Staleness::default();
    let mut seen = 0usize;
    for f in walker::walk(root) {
        if known.contains(&f.rel_path) {
            seen += 1;
            let modified = std::fs::metadata(&f.abs_path)
                .and_then(|m| m.modified())
                .map(|m| m >= indexed_at)
                .unwrap_or(true);
            if modified {
                st.modified += 1;
            }
        } else {
            st.added += 1;
        }
    }
    st.removed = known.len().saturating_sub(seen);
    st.stale = st.modified + st.added + st.removed > 0;
    st
}

/// Freshness metadata attached to query answers.
#[derive(Debug, Clone, Serialize)]
pub struct FreshInfo {
    /// Time since the index the answer is based on was (re)built.
    pub index_age_ms: u64,
    /// Whether this call refreshed the index first.
    pub refreshed: bool,
    /// Files re-extracted or removed by that refresh.
    #[serde(skip_serializing_if = "is_zero")]
    pub files_changed: usize,
    /// Refresh cost (stat check + re-extract + patch), when one happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_ms: Option<u64>,
    /// True only if the index is known to be behind the working tree (e.g. a
    /// refresh failed). Normally false: stale indexes are refreshed first.
    pub stale: bool,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// How a refresh updated the in-memory graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RefreshMode {
    /// Nothing changed on disk.
    Noop,
    /// Changed files spliced into the live graph.
    Patched,
    /// Too much changed; the graph was reloaded from the store.
    Reloaded,
}

#[derive(Debug, Clone, Serialize)]
pub struct Refresh {
    pub mode: RefreshMode,
    pub files_changed: usize,
    pub elapsed_ms: u64,
    #[serde(skip)]
    pub stats: IndexStats,
}

/// A code graph bound to a repo root that knows how to keep itself fresh.
pub struct LiveGraph {
    root: PathBuf,
    graph: CodeGraph,
    known: HashSet<String>,
    indexed_at: SystemTime,
    last_check: Option<Instant>,
    generation: u64,
    /// Minimum time between staleness checks (default 250 ms).
    pub check_interval: Duration,
}

impl LiveGraph {
    /// Load the index for `root`, building it first if none exists.
    pub fn open(root: &Path) -> Result<LiveGraph> {
        let path = index_path(root);
        let fresh_build = !path.exists();
        let mut store = Store::open(&path).context("failed to open index store")?;
        if fresh_build {
            index::build_index(root, &mut store).context("indexing failed")?;
        }
        let graph = store.load_graph().context("failed to load index")?;
        let indexed_at = store
            .get_meta("indexed_at_ms")?
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms))
            .unwrap_or(UNIX_EPOCH); // unknown age → everything counts as stale
        let known = graph.files().into_iter().collect();
        Ok(LiveGraph {
            root: root.to_path_buf(),
            graph,
            known,
            indexed_at,
            last_check: fresh_build.then(Instant::now),
            generation: 0,
            check_interval: Duration::from_millis(250),
        })
    }

    pub fn graph(&self) -> &CodeGraph {
        &self.graph
    }

    /// Increments whenever the graph's contents change (cache key for derived
    /// structures such as the search index).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index_age_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(self.indexed_at)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Stat-only staleness check against the live graph.
    pub fn staleness(&self) -> Staleness {
        check(&self.root, &self.known, self.indexed_at)
    }

    /// Incrementally re-index and patch the live graph.
    pub fn refresh(&mut self) -> Result<Refresh> {
        let t = Instant::now();
        let mut store =
            Store::open(&index_path(&self.root)).context("failed to open index store")?;
        let (stats, delta) =
            index::build_index_since(&self.root, &mut store, Some(self.indexed_at))?;
        self.indexed_at = delta.started_at;
        self.last_check = Some(Instant::now());
        let files_changed = delta.upserted.len() + delta.removed.len();
        let mode = if delta.is_empty() {
            RefreshMode::Noop
        } else if files_changed > (self.known.len() / 4).max(64) {
            // Large change (branch switch, mass rename): a full reload is
            // cheaper than splicing.
            self.graph = store.load_graph()?;
            RefreshMode::Reloaded
        } else {
            patch(&mut self.graph, delta);
            RefreshMode::Patched
        };
        if mode != RefreshMode::Noop {
            self.known = self.graph.files().into_iter().collect();
            self.generation += 1;
        }
        Ok(Refresh {
            mode,
            files_changed,
            elapsed_ms: t.elapsed().as_millis() as u64,
            stats,
        })
    }

    /// Refresh if (throttled) staleness check says so. Never fails: a refresh
    /// error is reported as `stale: true` and the previous graph is kept.
    pub fn ensure_fresh(&mut self) -> FreshInfo {
        let due = self
            .last_check
            .map_or(true, |t| t.elapsed() >= self.check_interval);
        if !due {
            return self.info(false, 0, None, false);
        }
        let t = Instant::now();
        let st = self.staleness();
        self.last_check = Some(Instant::now());
        if !st.stale {
            return self.info(false, 0, None, false);
        }
        match self.refresh() {
            Ok(r) => self.info(
                r.mode != RefreshMode::Noop,
                r.files_changed,
                Some(t.elapsed().as_millis() as u64),
                false,
            ),
            Err(e) => {
                tracing::warn!("codescope: refresh failed, serving previous index: {e:#}");
                self.info(false, 0, None, true)
            }
        }
    }

    fn info(
        &self,
        refreshed: bool,
        files_changed: usize,
        refresh_ms: Option<u64>,
        stale: bool,
    ) -> FreshInfo {
        FreshInfo {
            index_age_ms: self.index_age_ms(),
            refreshed,
            files_changed,
            refresh_ms,
            stale,
        }
    }
}

/// Splice a [`Delta`] into a graph and re-resolve.
pub fn patch(graph: &mut CodeGraph, delta: Delta) {
    let touched: HashSet<String> = delta
        .upserted
        .iter()
        .map(|f| f.path.clone())
        .chain(delta.removed.iter().cloned())
        .collect();
    // Names whose candidate sets change: everything removed or added.
    let mut names: HashSet<String> = touched
        .iter()
        .flat_map(|p| graph.by_file(p))
        .filter_map(|id| graph.symbol(*id))
        .map(|s| s.name.clone())
        .collect();
    names.extend(
        delta
            .upserted
            .iter()
            .flat_map(|f| f.symbols.iter().map(|s| s.name.clone())),
    );
    graph.remove_files(&touched);
    for f in delta.upserted {
        graph.upsert_file(f);
    }
    graph.reindex();
    resolve::re_resolve_scoped(
        graph,
        &resolve::Scope {
            files: touched,
            names,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::EdgeKind;
    use std::fs;

    fn callers_of(g: &CodeGraph, name: &str) -> Vec<String> {
        let mut v: Vec<String> = g
            .by_name(name)
            .iter()
            .flat_map(|id| g.in_edges(*id))
            .filter(|e| e.kind == EdgeKind::Calls)
            .filter_map(|e| g.symbol(e.from).map(|s| s.name.clone()))
            .collect();
        v.sort();
        v
    }

    fn touch_later(path: &Path, contents: &str) {
        std::thread::sleep(Duration::from_millis(20));
        fs::write(path, contents).unwrap();
    }

    /// Let written files age past the mtime slack window.
    fn settle() {
        std::thread::sleep(MTIME_SLACK + Duration::from_millis(30));
    }

    #[test]
    fn detects_and_patches_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "pub fn core() {}\n").unwrap();
        fs::write(root.join("b.rs"), "fn user() { core(); }\n").unwrap();
        settle();

        let mut live = LiveGraph::open(root).unwrap();
        assert!(!live.staleness().stale, "fresh after build");
        assert_eq!(callers_of(live.graph(), "core"), vec!["user"]);

        // Modify, add and remove files.
        touch_later(&root.join("b.rs"), "fn user() {}\n");
        fs::write(root.join("c.rs"), "fn other() { core(); }\n").unwrap();
        let st = live.staleness();
        assert_eq!((st.modified, st.added, st.removed), (1, 1, 0));

        let r = live.refresh().unwrap();
        settle();
        assert_eq!(r.mode, RefreshMode::Patched);
        assert_eq!(r.files_changed, 2);
        assert_eq!(
            callers_of(live.graph(), "core"),
            vec!["other"],
            "stale edge dropped, new edge bound"
        );

        fs::remove_file(root.join("c.rs")).unwrap();
        assert_eq!(live.staleness().removed, 1);
        live.refresh().unwrap();
        settle();
        assert!(callers_of(live.graph(), "core").is_empty());
        assert!(!live.staleness().stale);
    }

    #[test]
    fn patch_equals_full_reload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..6 {
            fs::write(
                root.join(format!("m{i}.rs")),
                format!(
                    "pub fn f{i}() {{ f{}(); shared(); }}\npub fn shared() {{}}\n",
                    (i + 1) % 6
                ),
            )
            .unwrap();
        }
        let mut live = LiveGraph::open(root).unwrap();
        touch_later(
            &root.join("m2.rs"),
            "pub fn f2() {}\npub fn extra() { f0(); }\n",
        );
        live.refresh().unwrap();

        let reloaded = Store::open(&index_path(root))
            .unwrap()
            .load_graph()
            .unwrap();
        let sig = |g: &CodeGraph| {
            let mut v: Vec<(String, String, Option<String>)> = g
                .edges()
                .iter()
                .map(|e| {
                    (
                        g.symbol(e.from)
                            .map(|s| s.qualified_name() + "@" + &s.file)
                            .unwrap_or_default(),
                        e.to_name.clone(),
                        e.to.and_then(|t| g.symbol(t)).map(|s| s.file.clone()),
                    )
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(live.graph().symbol_count(), reloaded.symbol_count());
        assert_eq!(sig(live.graph()), sig(&reloaded));
    }

    #[test]
    fn ensure_fresh_is_throttled_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        let mut live = LiveGraph::open(root).unwrap();
        live.check_interval = Duration::from_secs(3600);
        fs::write(root.join("b.rs"), "fn b() {}\n").unwrap();
        // Within the interval: no check, no refresh.
        assert!(!live.ensure_fresh().refreshed);
        live.check_interval = Duration::ZERO;
        let info = live.ensure_fresh();
        assert!(info.refreshed && !info.stale);
        assert_eq!(info.files_changed, 1);
        assert!(info.refresh_ms.is_some());
        assert!(live.graph().symbols().any(|s| s.name == "b"));
        // Nothing new → no refresh.
        assert!(!live.ensure_fresh().refreshed);
    }
}
