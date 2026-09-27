//! MCP server surface (ADR-0010): JSON-RPC 2.0 over stdio, newline-delimited.
//!
//! Implements `initialize`, `tools/list`, and `tools/call` for the nine `cs_*`
//! tools. Logs go to stderr only; stdout carries protocol messages exclusively
//! (ADR-0013).

use crate::query::{self, DEFAULT_MAX_TOKENS};
use crate::{domain::CodeGraph, index, index_path, store::Store};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Protocol revisions we speak, newest first. We echo the client's requested
/// version when supported, otherwise offer our latest (MCP version negotiation).
const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_VERSIONS.iter().find(|v| **v == r).copied())
        .unwrap_or(SUPPORTED_VERSIONS[0])
}

struct Server {
    root: PathBuf,
    graph: Option<CodeGraph>,
}

impl Server {
    fn new(root: PathBuf) -> Self {
        Server { root, graph: None }
    }

    /// Lazily load (and cache) the code graph. If no index exists yet, build
    /// one first so agents never have to remember to call `cs_index`.
    fn graph(&mut self) -> Result<&CodeGraph> {
        if self.graph.is_none() {
            if !index_path(&self.root).exists() {
                self.reindex()?;
            } else {
                let store = Store::open(&index_path(&self.root))?;
                self.graph = Some(store.load_graph()?);
            }
        }
        Ok(self.graph.as_ref().expect("graph loaded"))
    }

    fn reindex(&mut self) -> Result<index::IndexStats> {
        let mut store = Store::open(&index_path(&self.root))?;
        let stats = index::build_index(&self.root, &mut store)?;
        self.graph = Some(store.load_graph()?);
        Ok(stats)
    }
}

/// Run the MCP server, reading newline-delimited JSON-RPC from stdin and writing
/// responses to stdout, until EOF.
pub fn serve_stdio(root: PathBuf) -> Result<()> {
    let mut server = Server::new(root);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("codescope-mcp: malformed JSON: {e}");
                continue;
            }
        };
        if let Some(resp) = handle(&mut server, &req) {
            serde_json::to_writer(&mut out, &resp)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Dispatch a single JSON-RPC message. Returns `None` for notifications.
fn handle(server: &mut Server, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str())?;
    let id = req.get("id").cloned();

    // Notifications (no id) get no response.
    let id = id?;

    let result: std::result::Result<Value, (i64, String)> = match method {
        "initialize" => {
            let requested = req
                .pointer("/params/protocolVersion")
                .and_then(|v| v.as_str());
            Ok(json!({
                "protocolVersion": negotiate(requested),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "codescope",
                    "title": "codescope code intelligence",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": "Structural code graph of this repository. Start with cs_repo_map (optionally with focus = the files/symbols you are editing) to orient; use cs_callers / cs_blast_radius before changing a symbol; run cs_diff_impact after editing to see what you affected and which tests to run. All answers are token-budgeted via max_tokens."
            }))
        }
        "tools/list" => Ok(json!({ "tools": tool_specs() })),
        "tools/call" => match call_tool(server, req.get("params").unwrap_or(&Value::Null)) {
            // Tool *execution* failures are reported in-band with isError so
            // the model can see and recover from them (MCP spec); protocol
            // errors (unknown tool, bad params) stay JSON-RPC errors.
            Err((-32000, msg)) => Ok(json!({
                "content": [{ "type": "text", "text": msg }],
                "isError": true
            })),
            other => other,
        },
        "ping" => Ok(json!({})),
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

fn call_tool(server: &mut Server, params: &Value) -> std::result::Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "missing tool name".to_string()))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let max_tokens = args
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let str_arg = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;

    // cs_index doesn't need a preloaded graph.
    if name == "cs_index" {
        let stats = server.reindex().map_err(|e| (-32000, e.to_string()))?;
        return Ok(tool_result(json!({
            "files_indexed": stats.files_indexed,
            "files_skipped": stats.files_skipped,
            "files_removed": stats.files_removed,
            "symbols": stats.symbols,
            "edges": stats.edges,
            "elapsed_ms": stats.elapsed_ms,
        })));
    }

    let graph = server
        .graph()
        .map_err(|e| (-32000, format!("index unavailable: {e}")))?;

    // cs_diff_impact refreshes the index first so spans match the working tree.
    if name == "cs_diff_impact" {
        let base = str_arg("base");
        let changes = match str_arg("diff") {
            Some(d) => crate::diff::parse_unified_diff(&d),
            None => crate::diff::git_changes(&server.root, base.as_deref())
                .map_err(|e| (-32000, e.to_string()))?,
        };
        server.reindex().map_err(|e| (-32000, e.to_string()))?;
        let graph = server.graph().map_err(|e| (-32000, e.to_string()))?;
        let base = base.unwrap_or_else(|| "HEAD".into());
        let r = crate::diff::diff_impact(graph, &changes, &base, max_tokens);
        return Ok(tool_result(serde_json::to_value(r).unwrap()));
    }

    let payload: Value = match name {
        "cs_callers" => {
            let s = str_arg("symbol").ok_or((-32602, "missing 'symbol'".into()))?;
            serde_json::to_value(query::callers(graph, &s, depth, max_tokens)).unwrap()
        }
        "cs_callees" => {
            let s = str_arg("symbol").ok_or((-32602, "missing 'symbol'".into()))?;
            serde_json::to_value(query::callees(graph, &s, depth, max_tokens)).unwrap()
        }
        "cs_blast_radius" => {
            let t = str_arg("target").ok_or((-32602, "missing 'target'".into()))?;
            serde_json::to_value(query::blast_radius(graph, &t, max_tokens)).unwrap()
        }
        "cs_definition" => {
            let s = str_arg("symbol").ok_or((-32602, "missing 'symbol'".into()))?;
            serde_json::to_value(query::definition(graph, &s, max_tokens)).unwrap()
        }
        "cs_references" => {
            let s = str_arg("symbol").ok_or((-32602, "missing 'symbol'".into()))?;
            serde_json::to_value(query::references(graph, &s, max_tokens)).unwrap()
        }
        "cs_dependency_graph" => {
            serde_json::to_value(query::dependency_graph(graph, max_tokens)).unwrap()
        }
        "cs_structural_search" => {
            let q = str_arg("query").ok_or((-32602, "missing 'query'".into()))?;
            serde_json::to_value(query::structural_search(graph, &q, max_tokens)).unwrap()
        }
        "cs_repo_summary" => serde_json::to_value(query::repo_summary(graph, max_tokens)).unwrap(),
        "cs_repo_map" => {
            let focus: Vec<String> = match args.get("focus") {
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            };
            serde_json::to_value(query::repo_map(graph, &focus, max_tokens)).unwrap()
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };

    Ok(tool_result(payload))
}

/// Wrap a JSON payload as an MCP tool result: `structuredContent` for clients
/// on 2025-06-18+, plus the same JSON as a text block for older clients.
fn tool_result(payload: Value) -> Value {
    json!({
        "content": [
            { "type": "text", "text": serde_json::to_string(&payload).unwrap() }
        ],
        "structuredContent": payload
    })
}

fn sym_schema(arg: &str, desc: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            arg: { "type": "string", "description": desc },
            "max_tokens": { "type": "integer", "description": "Token budget for the answer." }
        },
        "required": [arg]
    })
}

fn tool_specs() -> Vec<Value> {
    let mut specs = base_specs();
    for spec in &mut specs {
        let name = spec["name"].as_str().unwrap_or_default().to_string();
        let read_only = name != "cs_index";
        spec["title"] = json!(name.trim_start_matches("cs_").replace('_', " "));
        spec["annotations"] = json!({
            "readOnlyHint": read_only,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        });
    }
    specs
}

fn base_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "cs_index",
            "description": "Build or incrementally refresh the structural index of the repository.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "cs_callers",
            "description": "Who (transitively) calls a symbol.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "depth": { "type": "integer", "description": "Max transitive depth (default 3)." },
                    "max_tokens": { "type": "integer" }
                },
                "required": ["symbol"]
            }
        }),
        json!({
            "name": "cs_callees",
            "description": "What a symbol (transitively) calls.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "depth": { "type": "integer" },
                    "max_tokens": { "type": "integer" }
                },
                "required": ["symbol"]
            }
        }),
        json!({
            "name": "cs_blast_radius",
            "description": "Everything downstream-affected if a symbol or file changes.",
            "inputSchema": sym_schema("target", "Symbol name or file path.")
        }),
        json!({
            "name": "cs_definition",
            "description": "Where a symbol is defined.",
            "inputSchema": sym_schema("symbol", "Symbol name.")
        }),
        json!({
            "name": "cs_references",
            "description": "All references to a symbol.",
            "inputSchema": sym_schema("symbol", "Symbol name.")
        }),
        json!({
            "name": "cs_dependency_graph",
            "description": "File/module import graph with cycle detection.",
            "inputSchema": { "type": "object", "properties": { "max_tokens": { "type": "integer" } } }
        }),
        json!({
            "name": "cs_structural_search",
            "description": "Structural search, e.g. 'kind:function calls:db_query returns:Result'.",
            "inputSchema": sym_schema("query", "Structural query string.")
        }),
        json!({
            "name": "cs_repo_summary",
            "description": "Token-bounded architectural overview to read before editing.",
            "inputSchema": { "type": "object", "properties": { "max_tokens": { "type": "integer" } } }
        }),
        json!({
            "name": "cs_repo_map",
            "description": "PageRank-ranked map of the repo's most important signatures, grouped by file, within the token budget. Pass `focus` (files or symbols you are working on) to personalize the ranking to what matters around them. Best first call when orienting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "focus": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Symbol names or repo-relative file paths to center the map on."
                    },
                    "max_tokens": { "type": "integer", "description": "Token budget (default 4000)." }
                }
            }
        }),
        json!({
            "name": "cs_diff_impact",
            "description": "Change impact of uncommitted work (vs. a git base, default HEAD) or of a supplied unified diff: the symbols changed, everything that transitively depends on them, and the tests worth running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "base": { "type": "string", "description": "Git ref to diff the working tree against (default HEAD)." },
                    "diff": { "type": "string", "description": "Optional unified diff text to analyze instead of running git." },
                    "max_tokens": { "type": "integer" }
                }
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_tools_list() {
        let mut server = Server::new(PathBuf::from("."));
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        let resp = handle(&mut server, &init).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "codescope");

        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let resp = handle(&mut server, &list).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 11);
        assert!(tools
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"].is_boolean()));
    }

    #[test]
    fn negotiates_protocol_version() {
        let mut server = Server::new(PathBuf::from("."));
        for (asked, want) in [
            ("2025-06-18", "2025-06-18"),
            ("2024-11-05", "2024-11-05"),
            ("1999-01-01", "2025-06-18"),
        ] {
            let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion": asked}});
            let resp = handle(&mut server, &init).unwrap();
            assert_eq!(resp["result"]["protocolVersion"], want);
        }
    }

    #[test]
    fn auto_indexes_and_returns_structured_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hub() {}\nfn a() { hub(); }\n").unwrap();
        let mut server = Server::new(dir.path().to_path_buf());
        let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"cs_callers","arguments":{"symbol":"hub"}}});
        let resp = handle(&mut server, &call).unwrap();
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["results"][0]["name"], "a");
        assert!(resp["result"]["content"][0]["text"].is_string());

        let map = json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"cs_repo_map","arguments":{"focus":["a"]}}});
        let resp = handle(&mut server, &map).unwrap();
        assert_eq!(
            resp["result"]["structuredContent"]["files"][0]["file"],
            "a.rs"
        );

        let diff = "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-fn hub() {}\n+fn hub() { }\n";
        let di = json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"cs_diff_impact","arguments":{"diff": diff}}});
        let resp = handle(&mut server, &di).unwrap();
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["changed_symbols"][0]["name"], "hub");
        assert_eq!(sc["impacted"][0]["name"], "a");
    }

    #[test]
    fn tool_failure_is_in_band() {
        let mut server = Server::new(PathBuf::from("/nonexistent/definitely/not/here"));
        let call = json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
            "params":{"name":"cs_diff_impact","arguments":{}}});
        let resp = handle(&mut server, &call).unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn notification_gets_no_response() {
        let mut server = Server::new(PathBuf::from("."));
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle(&mut server, &note).is_none());
    }
}
