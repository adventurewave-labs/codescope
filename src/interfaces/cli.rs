//! CLI surface (ADR-0009). Verbs map 1:1 to query application services.

use crate::query::{self, QueryResult, DEFAULT_MAX_TOKENS};
use crate::{index, index_path, store::Store};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "codescope",
    version,
    about = "A blazing-fast code-intelligence engine for AI agents.",
    long_about = None
)]
pub struct Cli {
    /// Repository root to operate on.
    #[arg(long, short = 'p', global = true, default_value = ".")]
    pub path: PathBuf,

    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Token budget for query answers.
    #[arg(long, global = true, default_value_t = DEFAULT_MAX_TOKENS)]
    pub max_tokens: usize,

    /// Increase log verbosity (logs go to stderr only).
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Build or incrementally refresh the index.
    Index,
    /// Who calls a symbol (transitively).
    Callers {
        symbol: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
    },
    /// What a symbol calls (transitively).
    Callees {
        symbol: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
    },
    /// What is downstream-affected if this symbol/file changes.
    #[command(name = "blast-radius")]
    BlastRadius { target: String },
    /// All references to a symbol.
    Refs { symbol: String },
    /// Where a symbol is defined.
    Def { symbol: String },
    /// File/module import graph (+ cycle detection).
    Deps,
    /// Structural search (e.g. "kind:function calls:db_query returns:Result").
    Search { query: Vec<String> },
    /// Token-bounded architectural overview.
    Summary,
    /// Start the MCP server over stdio.
    Serve {
        /// Serve the MCP protocol over stdio (default and only mode).
        #[arg(long, default_value_t = true)]
        mcp: bool,
    },
}

pub fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    let max_tokens = cli.max_tokens;

    match cli.command {
        Command::Index => {
            let mut store = open_store(&cli.path)?;
            let stats = index::build_index(&cli.path, &mut store).context("indexing failed")?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "files_indexed": stats.files_indexed,
                        "files_skipped": stats.files_skipped,
                        "files_removed": stats.files_removed,
                        "symbols": stats.symbols,
                        "edges": stats.edges,
                        "elapsed_ms": stats.elapsed_ms,
                    })
                );
            } else {
                println!(
                    "indexed {} file(s) ({} skipped, {} removed) — {} symbols, {} edges in {} ms",
                    stats.files_indexed,
                    stats.files_skipped,
                    stats.files_removed,
                    stats.symbols,
                    stats.edges,
                    stats.elapsed_ms
                );
            }
        }
        Command::Serve { mcp } => {
            anyhow::ensure!(mcp, "only --mcp (stdio) is supported");
            let path = cli.path.clone();
            crate::interfaces::mcp::serve_stdio(path)?;
        }
        other => {
            // All remaining verbs are read-only queries over the loaded graph.
            let store = open_store(&cli.path)?;
            let graph = store
                .load_graph()
                .context("failed to load index; run `codescope index` first")?;
            match other {
                Command::Callers { symbol, depth } => {
                    emit(query::callers(&graph, &symbol, depth, max_tokens), json)
                }
                Command::Callees { symbol, depth } => {
                    emit(query::callees(&graph, &symbol, depth, max_tokens), json)
                }
                Command::BlastRadius { target } => {
                    emit(query::blast_radius(&graph, &target, max_tokens), json)
                }
                Command::Refs { symbol } => {
                    emit(query::references(&graph, &symbol, max_tokens), json)
                }
                Command::Def { symbol } => {
                    emit(query::definition(&graph, &symbol, max_tokens), json)
                }
                Command::Search { query } => emit(
                    query::structural_search(&graph, &query.join(" "), max_tokens),
                    json,
                ),
                Command::Deps => {
                    let dg = query::dependency_graph(&graph, max_tokens);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&dg)?);
                    } else {
                        for e in &dg.edges {
                            println!(
                                "{} -> {}{}",
                                e.from_file,
                                e.import,
                                e.to_file
                                    .as_deref()
                                    .map(|f| format!(" ({f})"))
                                    .unwrap_or_default()
                            );
                        }
                        if !dg.cycles.is_empty() {
                            println!("\ncycles detected:");
                            for c in &dg.cycles {
                                println!("  {}", c.join(" -> "));
                            }
                        }
                    }
                }
                Command::Summary => {
                    let s = query::repo_summary(&graph, max_tokens);
                    if json {
                        println!("{}", serde_json::to_string_pretty(&s)?);
                    } else {
                        println!("# Repo summary");
                        println!(
                            "{} files, {} symbols, {} edges",
                            s.file_count, s.symbol_count, s.edge_count
                        );
                        print!("languages: ");
                        println!(
                            "{}",
                            s.languages
                                .iter()
                                .map(|(l, n)| format!("{l} ({n})"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        println!("\ntop modules:");
                        for m in &s.top_modules {
                            println!("  {:>4}  {}", m.symbols, m.file);
                        }
                        println!("\nkey symbols:");
                        for v in &s.key_symbols {
                            println!("  {} {} — {}:{}", v.kind, v.name, v.file, v.line_start);
                        }
                    }
                }
                _ => unreachable!("index/serve handled above"),
            }
        }
    }
    Ok(())
}

fn open_store(root: &std::path::Path) -> Result<Store> {
    Store::open(&index_path(root)).context("failed to open index store")
}

/// Print a [`QueryResult`] in the requested format.
fn emit(result: QueryResult, json: bool) {
    if json {
        match serde_json::to_string_pretty(&result) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error serializing result: {e}"),
        }
        return;
    }
    println!(
        "# {} ({} result{}{})",
        result.kind,
        result.count,
        if result.count == 1 { "" } else { "s" },
        if result.truncated { ", truncated" } else { "" }
    );
    for v in &result.results {
        let depth = v.depth.map(|d| format!("[d{d}] ")).unwrap_or_default();
        let site = v
            .site_line
            .map(|l| format!(" @ line {l}"))
            .unwrap_or_default();
        let conf = v.confidence.map(|c| format!(" ({c})")).unwrap_or_default();
        println!(
            "  {depth}{} {} — {}:{}-{}{site}{conf}",
            v.kind, v.name, v.file, v.line_start, v.line_end
        );
    }
}
