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

/// Fraction of the index that must be rewritten to justify a post-index compaction.
const COMPACT_THRESHOLD: f64 = 0.5;

/// Index (or incrementally re-index) the repository rooted at `root` into
/// `store`. Returns statistics about the work performed.
pub fn build_index(root: &Path, store: &mut Store) -> Result<IndexStats, StoreError> {
    let start = Instant::now();
    let discovered = walker::walk(root);
    let prior = store.file_hashes()?;

    // Read + hash every file (cheap, parallel I/O), decide what changed.
    let with_hashes: Vec<(walker::DiscoveredFile, String, u64)> = discovered
        .par_iter()
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs_path).ok()?;
            let hash = walker::hash_content(&bytes);
            let source = String::from_utf8_lossy(&bytes).into_owned();
            Some((f.clone(), source, hash))
        })
        .collect();

    let current_paths: HashSet<String> = with_hashes
        .iter()
        .map(|(f, _, _)| f.rel_path.clone())
        .collect();

    // Files that are new or changed.
    let changed: Vec<&(walker::DiscoveredFile, String, u64)> = with_hashes
        .iter()
        .filter(|(f, _, hash)| prior.get(&f.rel_path) != Some(hash))
        .collect();
    let skipped = with_hashes.len() - changed.len();

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
    let mut files_removed = 0;
    for path in prior.keys() {
        if !current_paths.contains(path) {
            store.remove_file(path)?;
            files_removed += 1;
        }
    }

    store.set_meta("root", &root.to_string_lossy())?;

    // After a large rewrite (e.g. a cold index), reclaim redb free pages so the
    // on-disk index stays small (ADR-0005 size target).
    let total = with_hashes.len().max(1);
    if files_indexed as f64 / total as f64 >= COMPACT_THRESHOLD || files_removed > 0 {
        store.compact()?;
    }

    Ok(IndexStats {
        files_indexed,
        files_skipped: skipped,
        files_removed,
        symbols,
        edges,
        elapsed_ms: start.elapsed().as_millis(),
    })
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
