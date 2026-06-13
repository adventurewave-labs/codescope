//! Storage context (ADR-0005): embedded, single-file, memory-mapped persistence
//! via `redb`.
//!
//! Each [`SourceFile`]'s extracted symbols/edges are stored as one LZ4-compressed
//! bincode blob keyed by its repo-relative path. This per-file granularity is
//! what makes incremental re-index cheap (ADR-0008): a changed file replaces
//! exactly one record. Content hashes live in a separate table so incremental
//! change detection never has to decompress a blob.

use crate::domain::{CodeGraph, SourceFile};
use crate::resolve;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const HASHES: TableDefinition<&str, u64> = TableDefinition::new("hashes");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Serialize + LZ4-compress a file record for on-disk storage.
fn encode(file: &SourceFile) -> Result<Vec<u8>, StoreError> {
    let raw = bincode::serialize(file)?;
    Ok(lz4_flex::compress_prepend_size(&raw))
}

/// Decompress + deserialize a stored file record.
fn decode(bytes: &[u8]) -> Result<SourceFile, StoreError> {
    let raw = lz4_flex::decompress_size_prepended(bytes)
        .map_err(|e| StoreError::Db(format!("decompress: {e}")))?;
    Ok(bincode::deserialize(&raw)?)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(String),
    #[error("serialization error: {0}")]
    Serde(#[from] bincode::Error),
}

impl From<redb::Error> for StoreError {
    fn from(e: redb::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}
impl From<redb::DatabaseError> for StoreError {
    fn from(e: redb::DatabaseError) -> Self {
        StoreError::Db(e.to_string())
    }
}
impl From<redb::TransactionError> for StoreError {
    fn from(e: redb::TransactionError) -> Self {
        StoreError::Db(e.to_string())
    }
}
impl From<redb::TableError> for StoreError {
    fn from(e: redb::TableError) -> Self {
        StoreError::Db(e.to_string())
    }
}
impl From<redb::StorageError> for StoreError {
    fn from(e: redb::StorageError) -> Self {
        StoreError::Db(e.to_string())
    }
}
impl From<redb::CommitError> for StoreError {
    fn from(e: redb::CommitError) -> Self {
        StoreError::Db(e.to_string())
    }
}

/// The on-disk index. The default location is `<repo>/.codescope/index.redb`.
pub struct Store {
    db: Database,
}

impl Store {
    /// Open (creating if needed) the index at `path`.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = Database::create(path)?;
        // Ensure tables exist so reads on a fresh db don't error.
        let wtxn = db.begin_write()?;
        {
            wtxn.open_table(FILES)?;
            wtxn.open_table(HASHES)?;
            wtxn.open_table(META)?;
        }
        wtxn.commit()?;
        Ok(Store { db })
    }

    /// Insert or replace a file's record.
    pub fn put_file(&self, file: &SourceFile) -> Result<(), StoreError> {
        self.put_files(std::slice::from_ref(file))
    }

    /// Batch insert/replace many file records in a single transaction.
    pub fn put_files(&self, files: &[SourceFile]) -> Result<(), StoreError> {
        let wtxn = self.db.begin_write()?;
        {
            let mut t = wtxn.open_table(FILES)?;
            let mut h = wtxn.open_table(HASHES)?;
            for f in files {
                let bytes = encode(f)?;
                t.insert(f.path.as_str(), bytes.as_slice())?;
                h.insert(f.path.as_str(), f.content_hash)?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Remove a file's record (e.g. when it is deleted on disk).
    pub fn remove_file(&self, path: &str) -> Result<(), StoreError> {
        let wtxn = self.db.begin_write()?;
        {
            let mut t = wtxn.open_table(FILES)?;
            let mut h = wtxn.open_table(HASHES)?;
            t.remove(path)?;
            h.remove(path)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Map of path -> content hash for incremental change detection. Reads the
    /// dedicated hash table, so no blob is decompressed.
    pub fn file_hashes(&self) -> Result<HashMap<String, u64>, StoreError> {
        let rtxn = self.db.begin_read()?;
        let t = rtxn.open_table(HASHES)?;
        let mut out = HashMap::new();
        for entry in t.iter()? {
            let (k, v) = entry?;
            out.insert(k.value().to_string(), v.value());
        }
        Ok(out)
    }

    /// Load every file record into a fully-resolved in-memory [`CodeGraph`].
    pub fn load_graph(&self) -> Result<CodeGraph, StoreError> {
        let rtxn = self.db.begin_read()?;
        let t = rtxn.open_table(FILES)?;
        let mut graph = CodeGraph::new();
        for entry in t.iter()? {
            let (_, v) = entry?;
            let file = decode(v.value())?;
            graph.upsert_file(file);
        }
        graph.reindex();
        resolve::resolve(&mut graph);
        Ok(graph)
    }

    /// Number of indexed files.
    pub fn file_count(&self) -> Result<usize, StoreError> {
        let rtxn = self.db.begin_read()?;
        let t = rtxn.open_table(FILES)?;
        Ok(t.len()? as usize)
    }

    /// Reclaim free pages, shrinking the on-disk file. Worth doing after a bulk
    /// (cold) index where many pages were rewritten (ADR-0005). Returns whether
    /// any compaction happened.
    pub fn compact(&mut self) -> Result<bool, StoreError> {
        // redb reclaims pages incrementally; call until it reports no more work.
        let mut did = false;
        for _ in 0..8 {
            match self
                .db
                .compact()
                .map_err(|e| StoreError::Db(e.to_string()))?
            {
                true => did = true,
                false => break,
            }
        }
        Ok(did)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let wtxn = self.db.begin_write()?;
        {
            let mut t = wtxn.open_table(META)?;
            t.insert(key, value.as_bytes())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let rtxn = self.db.begin_read()?;
        let t = rtxn.open_table(META)?;
        Ok(t.get(key)?
            .map(|v| String::from_utf8_lossy(v.value()).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Language;
    use crate::extract::extract;

    #[test]
    fn round_trips_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.redb")).unwrap();
        let f = extract(Language::Rust, "a.rs", "fn foo() {}\n", 7);
        store.put_file(&f).unwrap();
        assert_eq!(store.file_count().unwrap(), 1);
        let hashes = store.file_hashes().unwrap();
        assert_eq!(hashes.get("a.rs"), Some(&7));
        let g = store.load_graph().unwrap();
        assert!(g.symbols().any(|s| s.name == "foo"));
    }

    #[test]
    fn remove_file_drops_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.redb")).unwrap();
        store
            .put_file(&extract(Language::Rust, "a.rs", "fn foo() {}\n", 1))
            .unwrap();
        store.remove_file("a.rs").unwrap();
        assert_eq!(store.file_count().unwrap(), 0);
    }
}
