# ADR-0010: MCP Server Protocol

**Status:** Accepted

**Date:** 2026-05-28

## Context

The primary distribution channel and wedge is the MCP ecosystem: one MCP server works across Claude Code, Cursor, Windsurf, VS Code/Copilot, Cline, Zed, and Continue. To get that "zero custom integration" reach, `codescope serve` must speak the protocol these hosts already launch and manage. MCP hosts overwhelmingly spawn local servers as child processes and talk to them over stdio, so transport and framing choices must match what those hosts expect rather than inventing a bespoke channel.

## Decision

Implement an **MCP server using JSON-RPC 2.0 over stdio**. The server implements the core MCP methods: **`initialize`**, **`tools/list`**, and **`tools/call`**.

**Framing:** use **newline-delimited JSON over stdio** — one JSON-RPC message per line — as used by stdio MCP servers, rather than LSP-style `Content-Length` headers. Each request and response is a single line terminated by `\n`; the server reads line-by-line from stdin and writes line-by-line to stdout.

Exposed tools map one-to-one onto the Query API (ADR-0009): **`cs_index`, `cs_callers`, `cs_callees`, `cs_blast_radius`, `cs_definition`, `cs_references`, `cs_dependency_graph`, `cs_structural_search`, `cs_repo_summary`.** Each tool advertises a JSON input schema in `tools/list` and returns the token-budgeted contract from ADR-0011.

## Consequences

**Positive**
- Zero custom per-host integration: any MCP-capable agent can launch `codescope serve` and discover its tools via `tools/list`.
- Newline-delimited JSON over stdio is simple to implement and debug (pipe lines in/out, no header parsing) and matches the dominant stdio-MCP convention.
- stdio means no port management, no network exposure, and a process lifecycle the host already controls — aligned with local-first, private operation.

**Negative / tradeoffs**
- stdout is reserved exclusively for protocol frames; all logging must go to stderr (see ADR-0013) or it corrupts the stream. This is a strict, easy-to-violate invariant.
- Newline-delimited framing requires that no embedded literal newlines appear in a serialized message; we guarantee compact single-line JSON serialization.
- One client per process; concurrent multi-client/persistent sharing is out of scope for v1 (a team-tier daemon concern).

## Alternatives Considered

- **`Content-Length`-header framing (LSP style):** robust for payloads with embedded newlines, but adds parsing complexity and is not the prevailing stdio-MCP convention. Rejected in favor of newline-delimited JSON.
- **HTTP/SSE transport:** needed for remote/hosted servers but adds networking, ports, and auth concerns irrelevant to a local single-user binary. Deferred to the team tier.
- **A custom non-MCP protocol:** would forfeit the entire zero-integration distribution advantage. Rejected.
