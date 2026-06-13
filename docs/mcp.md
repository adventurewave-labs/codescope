# codescope MCP tool reference

`codescope serve --mcp` speaks the [Model Context Protocol](https://modelcontextprotocol.io)
over **stdio** as newline-delimited **JSON-RPC 2.0** (ADR-0010). Logs go to
stderr only; stdout carries protocol messages exclusively (ADR-0013).

## Lifecycle

```sh
codescope serve --mcp -p /abs/path/to/repo
```

Supported JSON-RPC methods:

| Method | Purpose |
|---|---|
| `initialize` | Handshake. Returns `protocolVersion` (`2024-11-05`), `capabilities.tools`, and `serverInfo {name, version}`. |
| `tools/list` | Lists the nine `cs_*` tools with their JSON input schemas. |
| `tools/call` | Invokes a tool by `name` with `arguments`. |
| `ping` | Returns `{}`. |

Notifications (messages without an `id`, e.g. `notifications/initialized`)
receive no response.

The graph is loaded from `<repo>/.codescope/index.redb` **once** and cached for
the life of the process, so every query after the first pays only the
in-process cost (sub-millisecond for graph queries). Call `cs_index` first if
the index is stale or absent.

## Common conventions

- Every query tool accepts an optional `max_tokens` integer (default **4000**).
  Results are token-budgeted: when the budget is hit, output is truncated and a
  `truncated: true` flag is set instead of returning the whole graph (ADR-0011).
- `cs_callers` / `cs_callees` accept an optional `depth` integer (default **3**)
  bounding transitive traversal.
- Tool results are returned as MCP `content` — a single `text` block whose body
  is compact JSON with `symbol`, `kind`, `file`, `line_start`/`line_end`, edge
  lists, and a `truncated` flag.

## Tools

| Tool | Required args | Optional args | Returns |
|---|---|---|---|
| `cs_index` | — | — | Index stats: `files_indexed`, `files_skipped`, `files_removed`, `symbols`, `edges`, `elapsed_ms`. |
| `cs_callers` | `symbol` | `depth`, `max_tokens` | Symbols that transitively call `symbol`. |
| `cs_callees` | `symbol` | `depth`, `max_tokens` | Symbols `symbol` transitively calls. |
| `cs_blast_radius` | `target` | `max_tokens` | Everything downstream-affected if `target` (symbol name or file path) changes. |
| `cs_definition` | `symbol` | `max_tokens` | Where `symbol` is defined. |
| `cs_references` | `symbol` | `max_tokens` | All references to `symbol`. |
| `cs_dependency_graph` | — | `max_tokens` | File/module import graph with cycle detection. |
| `cs_structural_search` | `query` | `max_tokens` | Structural matches (see query syntax below). |
| `cs_repo_summary` | — | `max_tokens` | Token-bounded architectural overview to read before editing. |

### Structural query syntax (`cs_structural_search`)

Space-separated terms. `key:value` are filters; bare words match the symbol
name/signature.

- `kind:function|method|struct|enum|trait|interface|class|module|type|constant|field`
- `lang:rust|typescript|javascript|python|go`
- `file:<substr>` · `name:<substr>` · `calls:<callee>` · `returns:<type-substr>`

Example: `kind:method lang:rust calls:spawn returns:Result`

## Examples

Handshake:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

List tools:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

Build the index:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cs_index","arguments":{}}}
```

Who calls `build_index`, two levels deep:

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call",
 "params":{"name":"cs_callers","arguments":{"symbol":"build_index","depth":2}}}
```

Blast radius of a file under a tight token budget:

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call",
 "params":{"name":"cs_blast_radius","arguments":{"target":"src/store.rs","max_tokens":1500}}}
```

## Error responses

Errors use standard JSON-RPC error objects:

| Code | Meaning |
|---|---|
| `-32601` | Method not found. |
| `-32602` | Invalid params (missing tool name, missing required argument, or unknown tool). |
| `-32000` | Server error (e.g. index not loaded — run `cs_index` first). |
