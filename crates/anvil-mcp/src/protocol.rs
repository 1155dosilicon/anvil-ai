//! MCP JSON-RPC 2.0 over stdio (newline-delimited JSON messages).

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::tools::ToolRegistry;

pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP stdio loop reading line-delimited JSON-RPC from `reader` and writing responses to `writer`.
pub fn serve_stdio<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    registry: &ToolRegistry,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_line(trimmed, registry) {
            let mut encoded = serde_json::to_string(&response).expect("JSON-RPC response serializes");
            encoded.push('\n');
            writer.write_all(encoded.as_bytes())?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Process one JSON-RPC line and return `Some(response)` for requests, or `None` for notifications.
pub fn handle_line(raw: &str, registry: &ToolRegistry) -> Option<Value> {
    let msg: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return Some(json_error(Value::Null, -32700, format!("Parse error: {e}")));
        }
    };

    let Some(obj) = msg.as_object() else {
        return Some(json_error(Value::Null, -32600, "Invalid Request: expected JSON object"));
    };

    let id = obj.get("id").cloned();
    let method = match obj.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => {
            // Responses or malformed messages without method: ignore notifications, error if id present.
            return id.map(|id| json_error(id, -32600, "Invalid Request: missing method"));
        }
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    // Notifications (no `id`) must never receive a response per JSON-RPC 2.0.
    let id = id?;

    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                DEFAULT_PROTOCOL_VERSION
            };
            Some(json_ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "anvil",
                        "version": anvil_core::ANVIL_VERSION
                    },
                    "instructions": "Anvil offline native toolchain server (Clang/LLVM, Rust, CMake/Ninja, clangd, rust-analyzer, and coding skills). All line and column numbers are 1-based."
                }),
            ))
        }
        "ping" => Some(json_ok(id, json!({}))),
        "tools/list" => Some(json_ok(id, json!({ "tools": registry.list_tools() }))),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(json_error(id, -32602, "Invalid params: missing tool `name`"));
            };
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let outcome = registry.call_tool(name, &args);
            let text = serde_json::to_string_pretty(&outcome.payload).expect("tool payload serializes");
            Some(json_ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": outcome.is_error
                }),
            ))
        }
        other => Some(json_error(id, -32601, format!("Method not found: {other}"))),
    }
}

fn json_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn json_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into()
        }
    })
}
