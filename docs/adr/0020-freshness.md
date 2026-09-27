# ADR-0020: Freshness — Auto-Refresh, Incremental Patching, Watch

**Status:** Accepted (extends ADR-0008)

**Date:** 2026-09-26

## Context

An index that silently lags the working tree is worse than none: agents edit
code and then ask questions of a stale graph. Before this ADR, freshness was
the caller's job. The CLI loaded whatever was on disk, and the MCP server
cached its graph until someone called `cs_index`. Each reload also decoded
every record and re-resolved the whole graph.

## Decision

1. **Stat-only staleness check (`fresh::check`).** An ignore-aware parallel
   walk plus `stat` (no reads) reports the index as stale when a known file's
   mtime is at or after the last index start, or when files were added or
   removed.
   - **`MTIME_SLACK` = 50 ms.** Kernel file timestamps use a coarse clock
     that can lag by a tick. A false positive costs only a no-op refresh; a
     false negative would serve stale answers.
2. **`LiveGraph`.** Wraps a graph bound to a repo root.
   - `ensure_fresh()` runs the check at most once per `check_interval` (250 ms
     for MCP, every call for the CLI) and refreshes when stale.
   - It never fails the query. If a refresh fails, it serves the previous
     graph with `stale: true`.
3. **Incremental refresh.**
   - `build_index_since(since)` trusts `stat`: known files untouched since the
     last run are neither read nor hashed. New or recently modified files are
     hash-verified.
   - The resulting `Delta` is spliced into the live graph with
     `remove_files` (single pass) and `upsert_file`.
   - **Scoped re-resolution.** A binding depends only on the caller's file and
     on the candidate set for the called name. So only edges from touched
     files, or naming a symbol that was added or removed, are unbound and
     retried. A test (`patch_equals_full_reload`) checks this is equivalent to
     a full reload.
   - Large deltas (more than 64 files and more than 25% of files) reload from
     the store instead.
4. **Surfaces**
   - MCP: every tool result carries
     `freshness: {index_age_ms, refreshed, files_changed, refresh_ms, stale}`.
   - CLI: queries auto-refresh (opt out with `--no-refresh`).
     `codescope status` reports staleness.
   - `codescope watch`: platform file-watcher (`notify`), ignores
     unsupported files, `.codescope`, `.git`, `target` and `node_modules`,
     debounces bursts (default 200 ms), and runs an incremental `since`
     re-index per burst.
5. **Resolver throughput** (benefits every load, not just refresh):
   - Import scoring uses a per-file segment set (O(1) per candidate instead of
     re-splitting every import string for every candidate).
   - Bindings are computed in parallel (rayon) and applied in place, with no
     edge cloning.

## Measurements

Release build, 4-core container, `examples/fresh_latency.rs`:

| Repo | Files | Edges | Full load, before | Full load, after | Stat check | 1-file refresh |
|---|---|---|---|---|---|---|
| codescope | 21 | 2.8k | 9 ms | 7 ms | 3 ms | 19 ms |
| gson (Java) | 264 | 30k | 207 ms | 34 ms | 4 ms | 36 ms |
| fmt (C++) | 79 | 25k | 149 ms | 30 ms | 3 ms | 80 ms* |
| dotnet/samples (C#) | 3,166 | 129k | 674 ms | 318 ms | 46 ms | 213 ms |

\* fmt's refresh is dominated by re-parsing multi-thousand-line headers.

## Consequences

- Agents never need `cs_index`. Answers reflect edits made seconds earlier,
  and say how fresh they are.
- The redb writer lock means a CLI or MCP refresh and `codescope watch` on
  the same repo serialize. A refresh that cannot take the lock degrades to
  `stale: true` rather than failing.
- Remaining refresh cost is two walks (check + index) plus a store commit.
  A resident MCP-side watcher could skip the check walk; this is deferred,
  since the stat check is already under 50 ms at 3k files.
