//! End-to-end integration test of `LspPool` / `LspClient` against a real
//! child process speaking Content-Length LSP JSON-RPC.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anvil_core::InstallLayout;
use anvil_lsp::{LanguageKind, LspPool, ServerCommand};

const MOCK_LSP_SRC: &str = r#"
use std::io::{self, BufRead, BufReader, Write};

fn read_msg<R: BufRead>(r: &mut R) -> Option<String> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = r.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let t = line.trim_end_matches(&['\r', '\n'][..]);
        if t.is_empty() {
            if len.is_some() { break; }
            continue;
        }
        if let Some((k, v)) = t.split_once(':') {
            if k.trim().eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    r.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn write_msg<W: Write>(w: &mut W, json: &str) {
    let _ = write!(w, "Content-Length: {}\r\n\r\n{}", json.len(), json);
    let _ = w.flush();
}

fn extract_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_id(json: &str) -> Option<String> {
    let needle = "\"id\":";
    let start = json.find(needle)? + needle.len();
    let rest = json[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 { None } else { Some(rest[..end].to_string()) }
}

fn main() {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut last_uri = String::new();

    while let Some(msg) = read_msg(&mut reader) {
        let method = extract_str(&msg, "method").unwrap_or_default();
        let id = extract_id(&msg);
        match method.as_str() {
            "initialize" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"capabilities\":{{\"hoverProvider\":true,\"definitionProvider\":true,\"referencesProvider\":true,\"documentSymbolProvider\":true,\"workspaceSymbolProvider\":true}}}}}}"
                    ));
                }
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                if let Some(uri) = extract_str(&msg, "uri") {
                    last_uri = uri.clone();
                    let has_bug = msg.contains("BUG_HERE");
                    let diags = if has_bug {
                        "[{\"range\":{\"start\":{\"line\":1,\"character\":4},\"end\":{\"line\":1,\"character\":12}},\"severity\":1,\"code\":\"E0425\",\"source\":\"mock-lsp\",\"message\":\"cannot find value BUG_HERE\"}]"
                    } else {
                        "[]"
                    };
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{{\"uri\":\"{uri}\",\"diagnostics\":{diags}}}}}"
                    ));
                }
            }
            "textDocument/hover" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"contents\":{{\"kind\":\"markdown\",\"value\":\"```rust\\nfn answer() -> i32\\n```\"}},\"range\":{{\"start\":{{\"line\":0,\"character\":3}},\"end\":{{\"line\":0,\"character\":9}}}}}}}}"
                    ));
                }
            }
            "textDocument/definition" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":[{{\"uri\":\"{last_uri}\",\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":10}}}}}}]}}"
                    ));
                }
            }
            "textDocument/references" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":[{{\"uri\":\"{last_uri}\",\"range\":{{\"start\":{{\"line\":0,\"character\":3}},\"end\":{{\"line\":0,\"character\":9}}}}}},{{\"uri\":\"{last_uri}\",\"range\":{{\"start\":{{\"line\":2,\"character\":4}},\"end\":{{\"line\":2,\"character\":10}}}}}}]}}"
                    ));
                }
            }
            "textDocument/documentSymbol" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":[{{\"name\":\"answer\",\"kind\":12,\"detail\":\"fn() -> i32\",\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":20}}}},\"selectionRange\":{{\"start\":{{\"line\":0,\"character\":3}},\"end\":{{\"line\":0,\"character\":9}}}}}}]}}"
                    ));
                }
            }
            "workspace/symbol" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":[{{\"name\":\"answer\",\"kind\":12,\"location\":{{\"uri\":\"{last_uri}\",\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":0,\"character\":10}}}}}}}}]}}"
                    ));
                }
            }
            "shutdown" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":null}}"));
                }
            }
            "exit" => break,
            _ => {}
        }
    }
}
"#;

fn compile_mock_lsp(dir: &Path) -> PathBuf {
    let src = dir.join("mock_lsp.rs");
    let exe = dir.join(format!("mock_lsp{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&src, MOCK_LSP_SRC).unwrap();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .args(["--edition", "2021"])
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("rustc must be available during cargo test");
    assert!(status.success(), "mock_lsp failed to compile");
    exe
}

#[test]
fn full_lsp_bridge_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mock_bin = compile_mock_lsp(tmp.path());

    let ws = tmp.path().join("project");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let file = ws.join("src").join("main.rs");
    std::fs::write(&file, "fn answer() -> i32 { 42 }\nlet x = BUG_HERE;\n").unwrap();

    let pool = LspPool::new(InstallLayout::new(tmp.path().join("anvil")), Some(ws.clone()))
        .with_server_override(
            LanguageKind::Rust,
            ServerCommand {
                program: mock_bin,
                args: Vec::new(),
                env: Vec::new(),
            },
        )
        .with_timeouts(
            Duration::from_secs(10),
            Duration::from_secs(10),
            Duration::from_secs(60),
        );

    // 1. Diagnostics detect BUG_HERE (1-based line 2, col 5).
    let diags = pool.diagnostics(&file, Some(Duration::from_secs(3))).unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].line, 2);
    assert_eq!(diags[0].col, 5);
    assert_eq!(diags[0].severity, "error");
    assert_eq!(diags[0].code.as_deref(), Some("E0425"));

    // 2. Fix file on disk -> didChange sync clears diagnostics.
    std::fs::write(&file, "fn answer() -> i32 { 42 }\nfn main() { let _ = answer(); }\n").unwrap();
    let diags_after = pool.diagnostics(&file, Some(Duration::from_secs(3))).unwrap();
    assert!(diags_after.is_empty(), "{diags_after:?}");

    // 3. Hover, definition, references, document symbols, workspace symbols.
    let hover = pool.hover(&file, 1, 4).unwrap().unwrap();
    assert!(hover.contents.contains("fn answer() -> i32"));
    assert_eq!(hover.line, Some(1));
    assert_eq!(hover.col, Some(4));

    let defs = pool.definition(&file, 2, 21).unwrap();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].line, 1);

    let refs = pool.references(&file, 1, 4, true).unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[1].line, 3);

    let doc_syms = pool.document_symbols(&file).unwrap();
    assert_eq!(doc_syms.len(), 1);
    assert_eq!(doc_syms[0].name, "answer");
    assert_eq!(doc_syms[0].kind, "function");

    let ws_syms = pool
        .workspace_symbols("ans", Some(&ws), Some(LanguageKind::Rust))
        .unwrap();
    assert_eq!(ws_syms.len(), 1);
    assert_eq!(ws_syms[0].name, "answer");

    pool.shutdown_all();
}
