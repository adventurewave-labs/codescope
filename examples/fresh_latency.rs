//! Measure freshness costs on a real repo (ADR-0020):
//!   cargo run --release --example fresh_latency -- /path/to/repo [file-to-touch]
//! Reports: full graph load, stat-only staleness check, and incremental
//! refresh (re-extract + in-memory patch + re-resolve) after editing one file.

use codescope::fresh::LiveGraph;
use codescope::{index_path, store::Store, walker};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("repo path"));
    let mut live = LiveGraph::open(&root)?;
    live.check_interval = std::time::Duration::ZERO;
    live.refresh()?; // catch up

    let t = Instant::now();
    let g = Store::open(&index_path(&root))?.load_graph()?;
    let full = t.elapsed();

    std::thread::sleep(std::time::Duration::from_millis(100));
    let t = Instant::now();
    let st = live.staleness();
    let check = t.elapsed();
    assert!(!st.stale, "{st:?}");

    let target = match std::env::args().nth(2) {
        Some(p) => root.join(p),
        None => {
            walker::walk(&root)
                .into_iter()
                .nth(walker::walk(&root).len() / 2)
                .unwrap()
                .abs_path
        }
    };
    let orig = std::fs::read_to_string(&target)?;
    std::fs::write(&target, format!("{orig}\n"))?;
    let t = Instant::now();
    let info = live.ensure_fresh();
    let refresh = t.elapsed();
    std::fs::write(&target, orig)?;

    println!(
        "{}: {} files, {} symbols, {} edges",
        root.display(),
        g.files().len(),
        g.symbol_count(),
        g.edge_count()
    );
    println!(
        "  full graph load         {:>8.1} ms",
        full.as_secs_f64() * 1e3
    );
    println!(
        "  staleness check (stat)  {:>8.1} ms",
        check.as_secs_f64() * 1e3
    );
    println!(
        "  1-file refresh + patch  {:>8.1} ms  (refreshed={}, files={})",
        refresh.as_secs_f64() * 1e3,
        info.refreshed,
        info.files_changed
    );
    Ok(())
}
