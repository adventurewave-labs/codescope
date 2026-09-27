//! Indexing orchestration (ADR-0007 concurrency, ADR-0008 incremental).
//!
//! Walks the repo, extracts each file's symbols/edges in parallel with rayon,
//! and writes them to the [`Store`]. Incremental by default: a file is only
//! re-parsed when its content hash changed since the last index.

use crate::domain::SourceFile;
use crate::store::{Store, StoreError};
use crate::{extract, walker};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Default, Clone)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_removed: usize,
    pub symbols: usize,
    pub edges: usize,
    pub elapsed_ms: u128,
}

/// What an index run changed — enough to patch an in-memory graph without
/// reloading the store (ADR-0020).
#[derive(Debug)]
pub struct Delta {
    /// New or modified files, freshly extracted.
    pub upserted: Vec<SourceFile>,
    /// Paths that disappeared from disk.
    pub removed: Vec<String>,
    /// Wall-clock time the run *started* (files modified after this are stale).
    pub started_at: std::time::SystemTime,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty()
    }
}

/// Current wall-clock time in ms since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Fraction of the index that must be rewritten to justify a post-index compaction.
const COMPACT_THRESHOLD: f64 = 0.5;

/// Index (or incrementally re-index) the repository rooted at `root` into
/// `store`. Returns statistics about the work performed.
pub fn build_index(root: &Path, store: &mut Store) -> Result<IndexStats, StoreError> {
    build_index_delta(root, store).map(|(stats, _)| stats)
}

/// File timestamps come from the kernel's *coarse* clock, which can lag the
/// wall clock by a tick. Anything modified within this window before a
/// reference time is treated as possibly-after (ADR-0020).
pub const MTIME_SLACK: std::time::Duration = std::time::Duration::from_millis(50);

/// [`build_index`], also returning the [`Delta`] it applied.
pub fn build_index_delta(
    root: &Path,
    store: &mut Store,
) -> Result<(IndexStats, Delta), StoreError> {
    build_index_since(root, store, None)
}

/// Incremental index that trusts `stat` for already-indexed files: a known
/// file whose mtime predates `since` (minus [`MTIME_SLACK`]) is assumed
/// unchanged and never read. New files and recently-modified ones are read and
/// hash-compared as usual. `since = None` reads and hashes everything.
pub fn build_index_since(
    root: &Path,
    store: &mut Store,
    since: Option<std::time::SystemTime>,
) -> Result<(IndexStats, Delta), StoreError> {
    let start = Instant::now();
    let started_at = std::time::SystemTime::now();
    let started_ms = now_ms();
    let discovered = walker::walk(root);
    let prior = store.file_hashes()?;

    let cutoff = since.map(|t| t.checked_sub(MTIME_SLACK).unwrap_or(std::time::UNIX_EPOCH));
    // Read + hash candidate files (parallel I/O), decide what changed. With a
    // cutoff, known files untouched since then are skipped without a read.
    let with_hashes: Vec<(walker::DiscoveredFile, String, u64)> = discovered
        .par_iter()
        .filter(|f| match cutoff {
            Some(cut) if prior.contains_key(&f.rel_path) => std::fs::metadata(&f.abs_path)
                .and_then(|m| m.modified())
                .map_or(true, |m| m >= cut),
            _ => true,
        })
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs_path).ok()?;
            let hash = walker::hash_content(&bytes);
            let source = String::from_utf8_lossy(&bytes).into_owned();
            Some((f.clone(), source, hash))
        })
        .collect();

    let current_paths: HashSet<String> = discovered.iter().map(|f| f.rel_path.clone()).collect();

    // Files that are new or changed.
    let changed: Vec<&(walker::DiscoveredFile, String, u64)> = with_hashes
        .iter()
        .filter(|(f, _, hash)| prior.get(&f.rel_path) != Some(hash))
        .collect();
    let skipped = discovered.len() - changed.len();

    // Parse + extract changed files in parallel (CPU-bound).
    let extracted: Vec<SourceFile> = changed
        .par_iter()
        .map(|(f, source, hash)| extract::extract(f.language, &f.rel_path, source, *hash))
        .collect();

    let symbols: usize = extracted.iter().map(|f| f.symbols.len()).sum();
    let edges: usize = extracted.iter().map(|f| f.edges.len()).sum();
    let files_indexed = extracted.len();

    store.put_files(&extracted)?;

    // Remove records for files that disappeared from disk.
    let mut removed = Vec::new();
    for path in prior.keys() {
        if !current_paths.contains(path) {
            store.remove_file(path)?;
            removed.push(path.clone());
        }
    }
    let files_removed = removed.len();

    store.set_meta("root", &root.to_string_lossy())?;
    store.set_meta("indexed_at_ms", &started_ms.to_string())?;

    // After a large rewrite (e.g. a cold index), reclaim redb free pages so the
    // on-disk index stays small (ADR-0005 size target).
    let total = discovered.len().max(1);
    if files_indexed as f64 / total as f64 >= COMPACT_THRESHOLD || files_removed > 0 {
        store.compact()?;
    }

    let stats = IndexStats {
        files_indexed,
        files_skipped: skipped,
        files_removed,
        symbols,
        edges,
        elapsed_ms: start.elapsed().as_millis(),
    };
    Ok((
        stats,
        Delta {
            upserted: extracted,
            removed,
            started_at,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn incremental_skips_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        let mut store = Store::open(&dir.path().join(".codescope/idx.redb")).unwrap();

        let s1 = build_index(dir.path(), &mut store).unwrap();
        assert_eq!(s1.files_indexed, 1);

        // No change → skipped.
        let s2 = build_index(dir.path(), &mut store).unwrap();
        assert_eq!(s2.files_indexed, 0);
        assert_eq!(s2.files_skipped, 1);

        // Change the file → re-indexed.
        fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let s3 = build_index(dir.path(), &mut store).unwrap();
        assert_eq!(s3.files_indexed, 1);
    }

    #[test]
    fn removes_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.path().join("b.rs"), "fn b() {}\n").unwrap();
        let mut store = Store::open(&dir.path().join(".codescope/idx.redb")).unwrap();
        build_index(dir.path(), &mut store).unwrap();
        fs::remove_file(dir.path().join("b.rs")).unwrap();
        let s = build_index(dir.path(), &mut store).unwrap();
        assert_eq!(s.files_removed, 1);
    }
}
