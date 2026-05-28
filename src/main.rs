use anyhow::Result;
use clap::Parser;
use codescope::interfaces::cli::{self, Cli};

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    cli::run(cli)
}

/// Logs go to stderr only so stdout stays clean for JSON/MCP output (ADR-0013).
fn init_tracing(verbose: bool) {
    let level = if verbose { "debug" } else { "warn" };
    let filter = std::env::var("CODESCOPE_LOG").unwrap_or_else(|_| level.to_string());
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}
