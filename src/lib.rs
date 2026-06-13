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
//! * [`interfaces`] — CLI / JSON / MCP surfaces.

pub mod domain;
pub mod extract;
pub mod index;
pub mod interfaces;
pub mod parser;
pub mod query;
pub mod resolve;
pub mod store;
pub mod walker;

use std::path::{Path, PathBuf};

/// Default location of the on-disk index for a repo root.
pub fn index_path(root: &Path) -> PathBuf {
    root.join(".codescope").join("index.redb")
}
