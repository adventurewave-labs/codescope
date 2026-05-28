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

const PROTOCOL_VERSION: &str = "2024-11-05";

struct Server {
    root: PathBuf,
    graph: Option<CodeGraph>,
}

impl Server {
    fn new(root: PathBuf) -> Self {
        Server { root, graph: None }
    }

    /// Lazily load (and cache) the code graph from the store.
    fn graph(&mut self) -> Result<&CodeGraph> {
        if self.graph.is_none() {
            let store = Store::open(&index_path(&self.root))?;
            self.graph = Some(store.load_graph()?);
        }
        Ok(self.graph.as_ref().unwrap())
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
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "codescope", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => Ok(json!({ "tools": tool_specs() })),
        "tools/call" => call_tool(server, req.get("params").unwrap_or(&Value::Null)),
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
        return Ok(text_content(&json!({
            "files_indexed": stats.files_indexed,
            "files_skipped": stats.files_skipped,
            "files_removed": stats.files_removed,
            "symbols": stats.symbols,
            "edges": stats.edges,
            "elapsed_ms": stats.elapsed_ms,
        })));
    }

    let graph = server.graph().map_err(|e| {
        (
            -32000,
            format!("index not loaded (run cs_index first): {e}"),
        )
    })?;

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
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };

    Ok(text_content(&payload))
}

/// Wrap a JSON payload in MCP `content` (a single text block of compact JSON).
fn text_content(payload: &Value) -> Value {
    json!({
        "content": [
            { "type": "text", "text": serde_json::to_string(payload).unwrap() }
        ]
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
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn notification_gets_no_response() {
        let mut server = Server::new(PathBuf::from("."));
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle(&mut server, &note).is_none());
    }
}
