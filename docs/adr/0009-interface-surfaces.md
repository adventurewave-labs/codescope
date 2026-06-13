# ADR-0009: Interface Surfaces

**Status:** Accepted

**Date:** 2026-05-28

## Context

The PRD requires every core capability to be reachable three ways: by humans and shell scripts (CLI), by programmatic callers (JSON over stdout), and by AI agents (MCP tools). The risk is that three surfaces drift into three subtly different behaviors, or that logic gets duplicated and fixed in only one place. We need the surfaces to be genuinely coequal — none privileged, none a second-class wrapper — while sharing a single source of truth for query semantics, token budgeting, and output shape.

## Decision

Implement **one internal Query API** (a Rust library crate) that owns all query logic and produces structured result types. Expose **three coequal surfaces** that are thin adapters over that core:

1. **CLI** via `clap` — human-readable output by default, scriptable.
2. **JSON over stdout** — the same commands with a `--json` flag emit the compact result contract (see ADR-0011) for programmatic consumers.
3. **MCP tools** — the agent-facing surface (see ADR-0010), mapping each tool to the same Query API calls.

All three call the identical Query API; surfaces only handle parsing inputs and formatting outputs. The CLI verbs are: **`index`, `callers`, `callees`, `blast-radius`, `refs`, `def`, `deps`, `search`, `summary`, `serve`** (`serve` starts the MCP server).

## Consequences

**Positive**
- Single implementation of query semantics, token budgeting, and result schema — fix once, correct everywhere; behavior is identical across surfaces by construction.
- `--json` makes the CLI itself a programmatic interface, so scripts and tool builders don't need the MCP server.
- New capabilities are added in the core once and surfaced by adding a verb and a tool definition, keeping the three surfaces in lockstep.

**Negative / tradeoffs**
- The core must expose a stable, well-typed API; ad-hoc shortcuts in one surface are disallowed, which is more upfront design discipline.
- Some duplication of argument plumbing (clap args, JSON-RPC tool schemas) is unavoidable even with shared logic.
- Human-friendly CLI formatting and machine output must be kept strictly separate so the `--json` path never interleaves decorative text (reinforced by the stdout/stderr split in ADR-0013).

## Alternatives Considered

- **MCP-only, with CLI as a debug afterthought:** matches the agent-first wedge but alienates the scripting/DevEx and tool-builder personas in the PRD, and makes local debugging painful. Rejected.
- **Separate binaries per surface:** clean isolation but multiplies release artifacts and invites behavioral drift. Rejected.
- **HTTP/REST as a fourth first-class surface in v1:** useful for the future team tier but unnecessary for local-first v1 where stdio MCP suffices. Deferred.
