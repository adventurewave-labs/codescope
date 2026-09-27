//! `codescope watch` (ADR-0020): keep the on-disk index fresh as files change.
//!
//! Uses the platform file-watcher (inotify / FSEvents / ReadDirectoryChangesW
//! via `notify`), ignores events for unsupported files and the index itself,
//! debounces bursts (editor saves, `git checkout`) into one incremental
//! re-index, and reports each update.

use crate::domain::Language;
use crate::index::{self, IndexStats};
use crate::{index_path, store::Store};
use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// Does this event path matter for the index?
fn relevant(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if rel.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".codescope" | ".git" | "target" | "node_modules")
        )
    }) {
        return false;
    }
    Language::from_path(path).is_some()
}

/// Watch `root`, re-indexing after each debounced burst of relevant changes.
/// Calls `on_update` after every re-index; stops after `max_updates` updates
/// when given (used by tests), otherwise runs until the watcher disconnects.
pub fn watch(
    root: &Path,
    debounce: Duration,
    max_updates: Option<usize>,
    mut on_update: impl FnMut(&IndexStats),
) -> Result<()> {
    let root = root.canonicalize().context("watch root")?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let _ = tx.send(res);
    })
    .context("failed to start file watcher")?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .context("failed to watch repository")?;

    let mut updates = 0usize;
    let mut since: Option<std::time::SystemTime> = None;
    loop {
        // Block for the first relevant event of a burst.
        let first = match rx.recv() {
            Ok(Ok(ev)) => ev,
            Ok(Err(e)) => {
                tracing::warn!("watch error: {e}");
                continue;
            }
            Err(_) => return Ok(()), // watcher gone
        };
        if !is_change(&first.kind) || !first.paths.iter().any(|p| relevant(&root, p)) {
            continue;
        }
        // Debounce: absorb everything until the tree is quiet.
        while rx.recv_timeout(debounce).is_ok() {}

        let mut store = Store::open(&index_path(&root))?;
        // After the first pass, trust stat for files untouched since the
        // previous run started (no re-read of the whole tree per burst).
        let (stats, delta) = index::build_index_since(&root, &mut store, since)?;
        since = Some(delta.started_at);
        drop(store);
        if stats.files_indexed + stats.files_removed > 0 {
            on_update(&stats);
            updates += 1;
            if max_updates.is_some_and(|m| updates >= m) {
                return Ok(());
            }
        }
    }
}

fn is_change(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn filters_irrelevant_paths() {
        let root = Path::new("/r");
        assert!(relevant(root, Path::new("/r/src/a.rs")));
        assert!(!relevant(root, Path::new("/r/README.md")));
        assert!(!relevant(root, Path::new("/r/.codescope/index.redb")));
        assert!(!relevant(root, Path::new("/r/target/debug/build/x.rs")));
        assert!(!relevant(root, Path::new("/r/node_modules/p/index.js")));
    }

    #[test]
    fn reindexes_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        {
            let mut store = Store::open(&index_path(&root)).unwrap();
            index::build_index(&root, &mut store).unwrap();
        }
        let (done_tx, done_rx) = mpsc::channel();
        let r = root.clone();
        std::thread::spawn(move || {
            let res = watch(&r, Duration::from_millis(100), Some(1), |s| {
                let _ = done_tx.send(s.files_indexed);
            });
            if let Err(e) = res {
                eprintln!("watch failed: {e:#}");
            }
        });
        // Let the watcher register, then change a file (retry in case the
        // first write raced watcher startup).
        let mut got = None;
        for i in 0..5 {
            std::thread::sleep(Duration::from_millis(300));
            fs::write(root.join("a.rs"), format!("fn a() {{}}\nfn b{i}() {{}}\n")).unwrap();
            if let Ok(n) = done_rx.recv_timeout(Duration::from_secs(3)) {
                got = Some(n);
                break;
            }
        }
        assert_eq!(got, Some(1), "watch re-indexed the changed file");
        let g = Store::open(&index_path(&root))
            .unwrap()
            .load_graph()
            .unwrap();
        assert!(g.symbols().any(|s| s.name.starts_with('b')));
    }
}
