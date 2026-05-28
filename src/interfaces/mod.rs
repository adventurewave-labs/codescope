//! Interface surfaces (ADR-0009): CLI, JSON-over-stdout, and the MCP server.
//! All three are thin adapters over [`crate::query`] and [`crate::index`].

pub mod cli;
pub mod mcp;
