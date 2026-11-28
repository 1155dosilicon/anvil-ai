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
    compile_rust_bin(dir, "mock_lsp", MOCK_LSP_SRC)
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

const MOCK_RA_FLYCHECK_SRC: &str = r#"
use std::io::{self, BufRead, BufReader, Write};
use std::thread;
use std::time::Duration;

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

fn emit_flycheck<W: Write>(out: &mut W, uri: &str, has_unused: bool) {
    write_msg(
        out,
        "{\"jsonrpc\":\"2.0\",\"method\":\"$/progress\",\"params\":{\"token\":\"rust-analyzer/flycheck/0\",\"value\":{\"kind\":\"begin\",\"title\":\"cargo check\"}}}",
    );
    thread::sleep(Duration::from_millis(60));
    let diags = if has_unused {
        "[{\"range\":{\"start\":{\"line\":5,\"character\":8},\"end\":{\"line\":5,\"character\":9}},\"severity\":2,\"code\":\"unused_variables\",\"source\":\"rustc\",\"message\":\"unused variable: `x`\"}]"
    } else {
        "[]"
    };
    write_msg(
        out,
        &format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{{\"uri\":\"{uri}\",\"diagnostics\":{diags}}}}}"
        ),
    );
    write_msg(
        out,
        "{\"jsonrpc\":\"2.0\",\"method\":\"$/progress\",\"params\":{\"token\":\"rust-analyzer/flycheck/0\",\"value\":{\"kind\":\"end\"}}}",
    );
}

fn main() {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut last_uri = String::new();
    let mut has_unused = false;

    while let Some(msg) = read_msg(&mut reader) {
        let method = extract_str(&msg, "method").unwrap_or_default();
        let id = extract_id(&msg);
        match method.as_str() {
            "initialize" => {
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"serverInfo\":{{\"name\":\"rust-analyzer\",\"version\":\"mock\"}},\"capabilities\":{{\"diagnosticProvider\":{{\"interFileDependencies\":true,\"workspaceDiagnostics\":false}},\"referencesProvider\":true}}}}}}"
                    ));
                }
            }
            "textDocument/didOpen" => {
                if let Some(uri) = extract_str(&msg, "uri") {
                    last_uri = uri.clone();
                    has_unused = msg.contains("let x = 5;");
                    // Simulate rust-analyzer emitting an empty native publishDiagnostics BEFORE flycheck starts.
                    write_msg(
                        &mut out,
                        &format!("{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{{\"uri\":\"{uri}\",\"diagnostics\":[]}}}}"),
                    );
                    emit_flycheck(&mut out, &uri, has_unused);
                }
            }
            "textDocument/didChange" => {
                has_unused = msg.contains("let x = 5;");
            }
            "textDocument/didSave" => {
                if !last_uri.is_empty() {
                    emit_flycheck(&mut out, &last_uri, has_unused);
                }
            }
            "textDocument/diagnostic" => {
                // Simulate rust-analyzer pull diagnostics returning empty items (no flycheck diags).
                if let Some(id) = id {
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"kind\":\"full\",\"items\":[]}}}}"
                    ));
                }
            }
            "textDocument/references" => {
                if let Some(id) = id {
                    // Return a Windows lowercase drive URI like rust-analyzer does.
                    write_msg(&mut out, &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":[{{\"uri\":\"file:///c:/Users/demo/src/main.rs\",\"range\":{{\"start\":{{\"line\":0,\"character\":3}},\"end\":{{\"line\":0,\"character\":6}}}}}}]}}"
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

fn compile_rust_bin(dir: &Path, name: &str, src_code: &str) -> PathBuf {
    let src = dir.join(format!("{name}.rs"));
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&src, src_code).unwrap();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = Command::new(rustc);
    cmd.args(["--edition", "2021"]);
    if cfg!(all(windows, target_env = "gnu")) {
        cmd.args(["-C", "link-self-contained=yes"]);
    }
    let status = cmd
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("rustc must be available during cargo test");
    assert!(status.success(), "{name} failed to compile");
    exe
}

#[test]
fn rust_analyzer_waits_for_flycheck_and_normalizes_reference_drive_letter() {
    let tmp = tempfile::tempdir().unwrap();
    let mock_bin = compile_rust_bin(tmp.path(), "mock_ra_flycheck", MOCK_RA_FLYCHECK_SRC);

    let ws = tmp.path().join("rtest");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"rtest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let file = ws.join("src").join("main.rs");
    std::fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let x = 5;\n    println!(\"{}\", add(2, 3));\n}\n",
    )
    .unwrap();

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

    // Bug A: must not short-circuit on pull diagnostics or early empty publishDiagnostics;
    // must wait for flycheck progress end and return the rustc unused_variables warning.
    let diags = pool.diagnostics(&file, Some(Duration::from_secs(3))).unwrap();
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].line, 6);
    assert_eq!(diags[0].col, 9);
    assert_eq!(diags[0].severity, "warning");
    assert_eq!(diags[0].code.as_deref(), Some("unused_variables"));
    assert_eq!(diags[0].source.as_deref(), Some("rustc"));

    // Bug D: lowercase Windows drive URI (`file:///c:/...`) in references normalizes to uppercase `C:`.
    let refs = pool.references(&file, 1, 4, true).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].file, PathBuf::from(r"C:\Users\demo\src\main.rs"));

    // Fixing file on disk triggers didChange + didSave -> flycheck re-runs and clears diagnostics.
    std::fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let _x = 5;\n    println!(\"{}\", add(2, 3));\n}\n",
    )
    .unwrap();
    let diags_after = pool.diagnostics(&file, Some(Duration::from_secs(3))).unwrap();
    assert!(diags_after.is_empty(), "{diags_after:?}");

    pool.shutdown_all();
}

#[test]
fn real_rust_analyzer_reports_unused_variable_diagnostic() {
    let anvil_home = std::env::var_os("ANVIL_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"D:\anvil-home"));
    let layout = InstallLayout::new(&anvil_home);
    let ra_exe = layout
        .toolchain_dir("rust")
        .join("bin")
        .join(format!("rust-analyzer{}", std::env::consts::EXE_SUFFIX));
    if !ra_exe.is_file() {
        eprintln!(
            "skipping real_rust_analyzer_reports_unused_variable_diagnostic: {} not found",
            ra_exe.display()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("rtest_real");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"rtest_real\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let file = ws.join("src").join("main.rs");
    std::fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let x = 5;\n    println!(\"{}\", add(2, 3));\n}\n",
    )
    .unwrap();

    let pool = LspPool::new(layout, Some(ws.clone()));

    // Bug A: real rust-analyzer flycheck reports `unused variable: x` at 6:9.
    let diags = pool.diagnostics(&file, Some(Duration::from_secs(15))).unwrap();
    assert!(
        diags.iter().any(|d| {
            d.severity == "warning"
                && d.code.as_deref() == Some("unused_variables")
                && d.line == 6
                && d.col == 9
                && d.message.contains("unused variable")
        }),
        "expected unused_variables warning at 6:9 from real rust-analyzer, got: {diags:?}"
    );

    // Bug D: real rust-analyzer references on Windows must have an uppercase drive letter.
    let refs = pool.references(&file, 1, 4, true).unwrap();
    assert!(!refs.is_empty(), "expected references to `add`");
    for loc in &refs {
        let s = loc.file.to_string_lossy();
        let bytes = s.as_bytes();
        assert!(
            bytes.len() >= 2 && bytes[0].is_ascii_uppercase() && bytes[1] == b':',
            "expected uppercase Windows drive letter in reference path, got: {s}"
        );
    }

    // Editing on disk + calling diagnostics again triggers didSave -> flycheck clears the warning.
    std::fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    let _x = 5;\n    println!(\"{}\", add(2, 3));\n}\n",
    )
    .unwrap();
    let diags_after = pool.diagnostics(&file, Some(Duration::from_secs(15))).unwrap();
    assert!(
        !diags_after
            .iter()
            .any(|d| d.code.as_deref() == Some("unused_variables")),
        "expected unused_variables warning to clear after fix, got: {diags_after:?}"
    );

    pool.shutdown_all();
}
