//! codescope — a single-binary code-intelligence engine for AI agents.
//!
//! The crate is organized by bounded context (see `docs/ddd/bounded-contexts.md`):
//!
//! * [`domain`] — the model (aggregate root [`domain::CodeGraph`]).
//! * [`walker`] — ignore-aware repository ingestion.
//! * [`parser`] — tree-sitter parsing.
//! * [`extract`] — symbol/edge extraction from parse trees.
//! * [`resolve`] — graph-wide name resolution (Tier 1).
//! * [`store`] — embedded redb persistence.
//! * [`index`] — indexing orchestration (parallel + incremental).
//! * [`query`] — agent-facing query application services.
//! * [`rank`] — PageRank centrality for repo maps and summaries.
//! * [`diff`] — git-diff change-impact analysis.
//! * [`search`] — hybrid BM25F + PageRank natural-language symbol search.
//! * [`fresh`] — staleness checks + incremental in-memory patching.
//! * [`watch`] — file-watcher driven re-indexing.
//! * [`interfaces`] — CLI / JSON / MCP surfaces.

pub mod diff;
pub mod domain;
pub mod extract;
pub mod fresh;
pub mod index;
pub mod interfaces;
pub mod parser;
pub mod query;
pub mod rank;
pub mod resolve;
pub mod search;
pub mod store;
pub mod walker;
pub mod watch;

use std::path::{Path, PathBuf};

/// Default location of the on-disk index for a repo root.
pub fn index_path(root: &Path) -> PathBuf {
    root.join(".codescope").join("index.redb")
}
