//! Ingestion context: ignore-aware repository walking and content hashing.
//!
//! See ADR-0001 (single binary) and the Ingestion bounded context in
//! `docs/ddd/bounded-contexts.md`.

use crate::domain::Language;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// A file discovered by the walker, ready to be parsed.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Absolute path on disk.
    pub abs_path: PathBuf,
    /// Path relative to the repo root (used as the stable file key).
    pub rel_path: String,
    pub language: Language,
}

/// Walk `root` respecting `.gitignore`/`.ignore` and standard ignore rules,
/// returning every file in a language we support.
pub fn walk(root: &Path) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false) // don't skip dotfiles by default; .gitignore still applies
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false) // honor .gitignore even outside a git repo
        .parents(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(language) = Language::from_path(path) else {
            continue;
        };
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(DiscoveredFile {
            abs_path: path.to_path_buf(),
            rel_path,
            language,
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

/// Stable, fast content hash for change detection (ADR-0008).
pub fn hash_content(bytes: &[u8]) -> u64 {
    seahash::hash(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_supported_files_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("b.py"), "def f(): pass").unwrap();
        fs::write(dir.path().join("c.txt"), "ignore me").unwrap();
        let files = walk(dir.path());
        let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(names.contains(&"a.rs"));
        assert!(names.contains(&"b.py"));
        assert!(!names.contains(&"c.txt"));
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(dir.path().join("kept.rs"), "fn a() {}").unwrap();
        fs::write(dir.path().join("ignored.rs"), "fn b() {}").unwrap();
        let files = walk(dir.path());
        let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(names.contains(&"kept.rs"));
        assert!(!names.contains(&"ignored.rs"));
    }
}
