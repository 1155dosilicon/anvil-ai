//! Integration tests for `anvil-mcp` stdio JSON-RPC server.

use std::io::Cursor;

use anvil_core::{EnvMode, InstallLayout};
use anvil_mcp::{serve_stdio, ToolRegistry};
use serde_json::{json, Value};

fn run_session(registry: &ToolRegistry, requests: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for req in requests {
        input.push_str(&serde_json::to_string(req).unwrap());
        input.push('\n');
    }
    let mut reader = Cursor::new(input.into_bytes());
    let mut output = Vec::new();
    serve_stdio(&mut reader, &mut output, registry).unwrap();

    String::from_utf8(output)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("valid JSON line"))
        .collect()
}

fn parse_tool_payload(resp: &Value) -> (Value, bool) {
    let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    (serde_json::from_str(text).unwrap(), is_error)
}

#[test]
fn handshake_tools_list_skills_doctor_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new(
        InstallLayout::new(tmp.path().join("anvil")),
        tmp.path().to_path_buf(),
        EnvMode::Sandbox(tmp.path().join("sandbox")),
    );

    let responses = run_session(
        &registry,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"skill_list","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"skill_read","arguments":{"name":"rust-systems"}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"anvil_doctor","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"unknown/method"}),
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}),
        ],
    );

    // Notification produced no response: 8 responses for 9 messages.
    assert_eq!(responses.len(), 8);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "anvil");
    assert_eq!(responses[1]["id"], 2);

    let tools = responses[2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 11);
    assert!(tools.iter().any(|t| t["name"] == "lsp_diagnostics"));
    assert!(tools.iter().any(|t| t["name"] == "rust_cargo"));
    assert!(tools.iter().any(|t| t["name"] == "cc_build"));

    let (skill_list, err) = parse_tool_payload(&responses[3]);
    assert!(!err);
    assert_eq!(skill_list["count"], 5);

    let (skill_doc, err) = parse_tool_payload(&responses[4]);
    assert!(!err);
    assert_eq!(skill_doc["name"], "rust-systems");
    assert!(skill_doc["content"].as_str().unwrap().len() > 200);

    let (doctor, err) = parse_tool_payload(&responses[5]);
    assert!(!err);
    assert_eq!(doctor["state"]["status"], "missing");

    assert_eq!(responses[6]["error"]["code"], -32601);

    let (unknown_tool, err) = parse_tool_payload(&responses[7]);
    assert!(err);
    assert_eq!(unknown_tool["ok"], false);
}

#[test]
fn rust_cargo_tool_returns_structured_diagnostics_on_real_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("mini_crate");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"mini_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n#[test] fn works() { assert_eq!(add(2, 3), 5); }\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(
        InstallLayout::new(tmp.path().join("anvil")),
        proj.clone(),
        EnvMode::Sandbox(tmp.path().join("sandbox")),
    );

    // 1. Passing `cargo test` via MCP.
    let res = run_session(
        &registry,
        &[json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"tools/call",
            "params":{"name":"rust_cargo","arguments":{"action":"test"}}
        })],
    );
    let (payload, is_error) = parse_tool_payload(&res[0]);
    assert!(!is_error, "{payload}");
    assert_eq!(payload["ok"], true);

    // 2. Introduce compile error -> `rust_cargo` returns structured diagnostics!
    std::fs::write(
        proj.join("src").join("lib.rs"),
        "pub fn broken() -> i32 { undefined_symbol }\n",
    )
    .unwrap();
    let res = run_session(
        &registry,
        &[json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{"name":"rust_cargo","arguments":{"action":"check"}}
        })],
    );
    let (payload, is_error) = parse_tool_payload(&res[0]);
    assert!(is_error);
    assert_eq!(payload["ok"], false);
    let diags = payload["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty(), "expected parsed compiler diagnostics: {payload}");
    assert_eq!(diags[0]["code"], "E0425");
    assert_eq!(diags[0]["line"], 1);
}
