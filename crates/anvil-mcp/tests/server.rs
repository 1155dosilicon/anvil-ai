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
        "pub fn add(a: i32, b: i32) -> i32 { let x = 5; a + b }\n#[test] fn works() { assert_eq!(add(2, 3), 5); }\n",
    )
    .unwrap();

    let registry = ToolRegistry::new(
        InstallLayout::new(tmp.path().join("anvil")),
        proj.clone(),
        EnvMode::Sandbox(tmp.path().join("sandbox")),
    );

    // 1. Passing `cargo test` via MCP: returns structured warning diagnostics, test harness
    //    output in stdout, and reports `toolchain: "path"` + warning because layout is empty.
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
    assert_eq!(payload["toolchain"], "path", "{payload}");
    let warnings = payload["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("falling back to PATH")),
        "expected PATH fallback warning: {payload}"
    );
    let diags = payload["diagnostics"].as_array().unwrap();
    assert!(
        diags.iter().any(|d| {
            d["level"] == "warning"
                && d["code"] == "unused_variables"
                && d["file"].as_str().is_some_and(|f| f.ends_with("lib.rs"))
                && d["line"] == 1
        }),
        "expected structured unused_variables warning from `cargo test`: {payload}"
    );
    let stdout = payload["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("test result: ok."),
        "missing test harness output: {stdout}"
    );
    assert!(
        !stdout.contains("\"reason\":\"compiler-message\""),
        "raw cargo JSON leaked into stdout: {stdout}"
    );

    // 2. Introduce compile error -> `rust_cargo` returns structured diagnostics for both `check` and `test`!
    std::fs::write(
        proj.join("src").join("lib.rs"),
        "pub fn broken() -> i32 { undefined_symbol }\n",
    )
    .unwrap();
    for (id, action) in [(2, "check"), (3, "test")] {
        let res = run_session(
            &registry,
            &[json!({
                "jsonrpc":"2.0",
                "id":id,
                "method":"tools/call",
                "params":{"name":"rust_cargo","arguments":{"action":action}}
            })],
        );
        let (payload, is_error) = parse_tool_payload(&res[0]);
        assert!(is_error, "action={action}: {payload}");
        assert_eq!(payload["ok"], false, "action={action}: {payload}");
        assert_eq!(payload["toolchain"], "path", "action={action}: {payload}");
        let diags = payload["diagnostics"].as_array().unwrap();
        assert!(
            !diags.is_empty(),
            "expected parsed compiler diagnostics for {action}: {payload}"
        );
        assert_eq!(diags[0]["code"], "E0425");
        assert_eq!(diags[0]["line"], 1);
    }
}

#[test]
fn rust_cargo_reports_anvil_toolchain_without_path_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = InstallLayout::new(tmp.path().join("anvil"));
    let rust_bin = layout.toolchain_dir("rust").join("bin");
    std::fs::create_dir_all(&rust_bin).unwrap();

    // Compile a tiny stub binary into `toolchains/rust/bin/cargo(.exe)` so the tool resolves from Anvil.
    let stub_src = tmp.path().join("stub_cargo.rs");
    std::fs::write(
        &stub_src,
        "fn main() { println!(\"running 1 test\\ntest stub ... ok\\ntest result: ok. 1 passed; 0 failed\"); }\n",
    )
    .unwrap();
    let stub_exe = rust_bin.join(format!("cargo{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = std::process::Command::new(rustc);
    cmd.args(["--edition", "2021", "-C", "debuginfo=0"]);
    if cfg!(all(windows, target_env = "gnu")) {
        cmd.args(["-C", "link-self-contained=yes"]);
    }
    let status = cmd
        .arg("-o")
        .arg(&stub_exe)
        .arg(&stub_src)
        .status()
        .expect("failed to compile stub cargo");
    assert!(status.success());

    let registry = ToolRegistry::new(
        layout,
        tmp.path().to_path_buf(),
        EnvMode::Sandbox(tmp.path().join("sandbox")),
    );
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
    assert_eq!(payload["toolchain"], "anvil", "{payload}");
    assert_eq!(payload["warnings"], json!([]), "{payload}");
    assert_eq!(payload["command"][0], stub_exe.display().to_string());
}

#[test]
fn cc_build_single_file_outputs_next_to_source_and_deduplicates_default_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = InstallLayout::new(tmp.path().join("anvil"));
    let llvm_bin = layout.toolchain_dir("llvm").join("bin");
    std::fs::create_dir_all(&llvm_bin).unwrap();

    // Stub clang binary so this test verifies exact argv + output path deterministically.
    let stub_src = tmp.path().join("stub_clang.rs");
    std::fs::write(&stub_src, "fn main() {}\n").unwrap();
    let stub_clang = llvm_bin.join(format!("clang{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = std::process::Command::new(rustc);
    cmd.args(["--edition", "2021", "-C", "debuginfo=0"]);
    if cfg!(all(windows, target_env = "gnu")) {
        cmd.args(["-C", "link-self-contained=yes"]);
    }
    let status = cmd
        .arg("-o")
        .arg(&stub_clang)
        .arg(&stub_src)
        .status()
        .expect("failed to compile stub clang");
    assert!(status.success());

    // Server default workspace is separate from the C source file directory (Bug B).
    let server_cwd = tmp.path().join("server_cwd");
    let src_dir = tmp.path().join("other").join("ctest");
    std::fs::create_dir_all(&server_cwd).unwrap();
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("main.c");
    std::fs::write(&src_file, "int main(void) { return 0; }\n").unwrap();
    let expected_exe = src_dir.join(format!("main{}", std::env::consts::EXE_SUFFIX));

    let registry = ToolRegistry::new(layout, server_cwd.clone(), EnvMode::Sandbox(tmp.path().join("sandbox")));

    // 1. Passing extra_args=["-Wall", "-Wextra", "-O2"] without `output` or `workspace`:
    //    - Bug B: output goes next to main.c, NOT in server_cwd.
    //    - Bug C: -Wall and -Wextra each appear exactly once.
    let responses = run_session(
        &registry,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{
                    "name":"cc_build",
                    "arguments":{
                        "file": src_file.display().to_string(),
                        "extra_args": ["-Wall", "-Wextra", "-O2"]
                    }
                }
            }),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"cc_build",
                    "arguments":{
                        "file": src_file.display().to_string()
                    }
                }
            }),
            json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"tools/call",
                "params":{
                    "name":"cc_build",
                    "arguments":{
                        "file": src_file.display().to_string(),
                        "extra_args": ["-Wall", "-std=c17"]
                    }
                }
            }),
        ],
    );

    // Tool schema documents default output placement alongside the source file.
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let cc_tool = tools.iter().find(|t| t["name"] == "cc_build").unwrap();
    assert!(
        cc_tool["description"]
            .as_str()
            .unwrap()
            .contains("alongside the source file"),
        "{cc_tool}"
    );
    assert!(
        cc_tool["inputSchema"]["properties"]["output"]["description"]
            .as_str()
            .unwrap()
            .contains("alongside the source file"),
        "{cc_tool}"
    );

    // Call 1: extra_args = ["-Wall", "-Wextra", "-O2"]
    let (p1, err1) = parse_tool_payload(&responses[1]);
    assert!(!err1, "{p1}");
    let cmd1: Vec<&str> = p1["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let o_pos = cmd1.iter().position(|&a| a == "-o").expect("-o flag present");
    assert_eq!(cmd1[o_pos + 1], expected_exe.display().to_string(), "{cmd1:?}");
    assert_ne!(
        cmd1[o_pos + 1],
        server_cwd
            .join(format!("main{}", std::env::consts::EXE_SUFFIX))
            .display()
            .to_string(),
        "must not write executable into server cwd: {cmd1:?}"
    );
    assert_eq!(cmd1.iter().filter(|&&a| a == "-Wall").count(), 1, "{cmd1:?}");
    assert_eq!(cmd1.iter().filter(|&&a| a == "-Wextra").count(), 1, "{cmd1:?}");
    assert_eq!(cmd1.iter().filter(|&&a| a == "-O2").count(), 1, "{cmd1:?}");

    // Call 2: no extra_args -> defaults -Wall and -Wextra still included once each.
    let (p2, err2) = parse_tool_payload(&responses[2]);
    assert!(!err2, "{p2}");
    let cmd2: Vec<&str> = p2["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(cmd2.iter().filter(|&&a| a == "-Wall").count(), 1, "{cmd2:?}");
    assert_eq!(cmd2.iter().filter(|&&a| a == "-Wextra").count(), 1, "{cmd2:?}");

    // Call 3: extra_args = ["-Wall", "-std=c17"] -> -Wextra added once, -Wall once, -std=c17 once.
    let (p3, err3) = parse_tool_payload(&responses[3]);
    assert!(!err3, "{p3}");
    let cmd3: Vec<&str> = p3["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(cmd3.iter().filter(|&&a| a == "-Wall").count(), 1, "{cmd3:?}");
    assert_eq!(cmd3.iter().filter(|&&a| a == "-Wextra").count(), 1, "{cmd3:?}");
    assert_eq!(cmd3.iter().filter(|&&a| a == "-std=c17").count(), 1, "{cmd3:?}");
    assert!(!cmd3.contains(&"-static"), "C file should not force -static by default");
}

#[test]
fn cc_build_single_cpp_file_defaults_to_static_unless_sanitized_or_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = InstallLayout::new(tmp.path().join("anvil"));
    let llvm_bin = layout.toolchain_dir("llvm").join("bin");
    std::fs::create_dir_all(&llvm_bin).unwrap();

    let stub_src = tmp.path().join("stub_clangxx.rs");
    std::fs::write(&stub_src, "fn main() {}\n").unwrap();
    let stub_clangxx = llvm_bin.join(format!("clang++{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = std::process::Command::new(rustc);
    cmd.args(["--edition", "2021", "-C", "debuginfo=0"]);
    if cfg!(all(windows, target_env = "gnu")) {
        cmd.args(["-C", "link-self-contained=yes"]);
    }
    let status = cmd
        .arg("-o")
        .arg(&stub_clangxx)
        .arg(&stub_src)
        .status()
        .expect("failed to compile stub clang++");
    assert!(status.success());

    let cpp_file = tmp.path().join("hello.cpp");
    std::fs::write(&cpp_file, "int main() { return 0; }\n").unwrap();

    let registry = ToolRegistry::new(
        layout,
        tmp.path().to_path_buf(),
        EnvMode::Sandbox(tmp.path().join("sandbox")),
    );
    let responses = run_session(
        &registry,
        &[
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"cc_build","arguments":{"file": cpp_file.display().to_string()}}
            }),
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"cc_build","arguments":{"file": cpp_file.display().to_string(), "static_link": false}}
            }),
            json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{"name":"cc_build","arguments":{"file": cpp_file.display().to_string(), "extra_args": ["-fsanitize=address,undefined"]}}
            }),
        ],
    );

    let (p1, err1) = parse_tool_payload(&responses[0]);
    assert!(!err1, "{p1}");
    let cmd1: Vec<&str> = p1["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        cmd1.contains(&"-static"),
        "C++ single-file should default to -static: {cmd1:?}"
    );

    let (p2, err2) = parse_tool_payload(&responses[1]);
    assert!(!err2, "{p2}");
    let cmd2: Vec<&str> = p2["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        !cmd2.contains(&"-static"),
        "static_link=false should disable -static: {cmd2:?}"
    );

    let (p3, err3) = parse_tool_payload(&responses[2]);
    assert!(!err3, "{p3}");
    let cmd3: Vec<&str> = p3["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        !cmd3.contains(&"-static"),
        "sanitizers must not inject -static: {cmd3:?}"
    );
}
