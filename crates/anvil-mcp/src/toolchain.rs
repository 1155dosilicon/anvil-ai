//! Structured toolchain runners for Cargo (Rust) and Clang/CMake/Ninja (C/C++).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anvil_core::InstallLayout;
use anvil_env::dispatch::ToolEnv;
use anvil_env::process::run_with_timeout;
use anvil_env::tools::{find_real_tool, managed_tool};
use serde::Serialize;
use serde_json::Value;

const MAX_OUTPUT_CHARS: usize = 12_000;
const MAX_DIAGNOSTICS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompilerDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

/// Where the executed toolchain binary was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolchainSource {
    Anvil,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
    pub path: PathBuf,
    pub source: ToolchainSource,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolchainRunReport {
    pub ok: bool,
    pub tool: String,
    pub toolchain: ToolchainSource,
    pub warnings: Vec<String>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub diagnostics: Vec<CompilerDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compile_commands: Option<PathBuf>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

/// Resolve an Anvil-managed tool binary, falling back to `PATH` so MCP works in hybrid environments too.
pub fn resolve_tool(layout: &InstallLayout, name: &str) -> Option<ResolvedTool> {
    if let Some(tool) = managed_tool(name) {
        if let Some(path) = find_real_tool(layout, tool) {
            return Some(ResolvedTool {
                path,
                source: ToolchainSource::Anvil,
            });
        }
    }
    let exe_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&exe_name);
        if candidate.is_file() {
            return Some(ResolvedTool {
                path: candidate,
                source: ToolchainSource::Path,
            });
        }
    }
    None
}

fn path_fallback_warning(layout: &InstallLayout, tool_name: &str, resolved: &ResolvedTool) -> Option<String> {
    (resolved.source == ToolchainSource::Path).then(|| {
        format!(
            "{tool_name} not found in Anvil toolchains at {}; falling back to PATH ({})",
            layout.root().display(),
            resolved.path.display()
        )
    })
}

pub struct CargoRequest {
    pub action: String,
    pub workspace: PathBuf,
    pub release: bool,
    pub package: Option<String>,
    pub test_filter: Option<String>,
    pub extra_args: Vec<String>,
    pub timeout: Duration,
}

/// Run Cargo with isolated Anvil environment and structured diagnostic parsing.
pub fn run_cargo(layout: &InstallLayout, req: &CargoRequest) -> Result<ToolchainRunReport, String> {
    let cargo = resolve_tool(layout, "cargo")
        .ok_or_else(|| "cargo not found in Anvil toolchains or PATH (run `anvil install`)".to_string())?;
    let warnings: Vec<String> = path_fallback_warning(layout, "cargo", &cargo).into_iter().collect();

    let has_message_format = req
        .extra_args
        .iter()
        .any(|a| a == "--message-format" || a.starts_with("--message-format="));

    let mut args: Vec<String> = Vec::new();
    let json_diags = match req.action.as_str() {
        "check" => {
            args.push("check".into());
            if !has_message_format {
                args.push("--message-format=json".into());
            }
            true
        }
        "build" => {
            args.push("build".into());
            if !has_message_format {
                args.push("--message-format=json".into());
            }
            true
        }
        "clippy" => {
            args.extend(["clippy".into(), "--all-targets".into()]);
            if !has_message_format {
                args.push("--message-format=json".into());
            }
            true
        }
        "test" => {
            args.push("test".into());
            if !has_message_format {
                args.push("--message-format=json".into());
            }
            true
        }
        "fmt_check" => {
            args.extend(["fmt".into(), "--check".into()]);
            false
        }
        "fmt" => {
            args.push("fmt".into());
            false
        }
        other => {
            return Err(format!(
                "unsupported cargo action '{other}' (expected: check, build, test, clippy, fmt_check, fmt)"
            ));
        }
    };

    if req.release && matches!(req.action.as_str(), "check" | "build" | "test" | "clippy") {
        args.push("--release".into());
    }
    if let Some(pkg) = &req.package {
        if !pkg.trim().is_empty() && !matches!(req.action.as_str(), "fmt" | "fmt_check") {
            args.extend(["-p".into(), pkg.clone()]);
        }
    }
    if let Some(filter) = &req.test_filter {
        if req.action == "test" && !filter.trim().is_empty() {
            args.push(filter.clone());
        }
    }
    args.extend(req.extra_args.iter().cloned());

    let mut cmd = Command::new(&cargo.path);
    cmd.args(&args).current_dir(&req.workspace);
    if cargo.source == ToolchainSource::Anvil {
        ToolEnv::for_layout(layout).apply(&mut cmd);
    }

    let out = run_with_timeout(&mut cmd, req.timeout).map_err(|e| format!("failed to run cargo: {e}"))?;

    let (diagnostics, clean_stdout) = if json_diags {
        parse_cargo_json_diagnostics(&out.stdout)
    } else {
        (Vec::new(), out.stdout.clone())
    };
    let (stdout, trunc_out) = truncate_output(&clean_stdout, MAX_OUTPUT_CHARS);
    let (stderr, trunc_err) = truncate_output(&out.stderr, MAX_OUTPUT_CHARS);

    let mut full_cmd = vec![cargo.path.display().to_string()];
    full_cmd.extend(args);

    Ok(ToolchainRunReport {
        ok: out.success(),
        tool: "cargo".into(),
        toolchain: cargo.source,
        warnings,
        command: full_cmd,
        cwd: req.workspace.clone(),
        exit_code: out.status.and_then(|s| s.code()),
        timed_out: out.timed_out(),
        duration_ms: out.elapsed.as_millis() as u64,
        diagnostics,
        compile_commands: None,
        stdout,
        stderr,
        truncated: trunc_out || trunc_err,
    })
}

pub struct CcBuildRequest {
    pub workspace: PathBuf,
    pub file: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub build_dir: String,
    pub build_type: String,
    pub target: Option<String>,
    pub extra_args: Vec<String>,
    pub timeout: Duration,
}

/// Build a C/C++ project via CMake + Ninja (generating `compile_commands.json`) or compile a single file with Clang.
pub fn run_cc_build(layout: &InstallLayout, req: &CcBuildRequest) -> Result<ToolchainRunReport, String> {
    if let Some(file) = &req.file {
        return compile_single_cc_file(layout, req, file);
    }

    let cmake_lists = req.workspace.join("CMakeLists.txt");
    if !cmake_lists.is_file() {
        return Err(format!(
            "no CMakeLists.txt in {} (pass `file` to compile a single C/C++ source file directly)",
            req.workspace.display()
        ));
    }

    let cmake = resolve_tool(layout, "cmake")
        .ok_or_else(|| "cmake not found in Anvil toolchains or PATH (run `anvil install`)".to_string())?;
    let mut warnings = Vec::new();
    let mut toolchain = cmake.source;
    if let Some(w) = path_fallback_warning(layout, "cmake", &cmake) {
        warnings.push(w);
    }

    let build_path = req.workspace.join(&req.build_dir);
    let env = ToolEnv::for_layout(layout);

    // Step 1: Configure with Ninja + compile_commands.json.
    let mut cfg_args = vec![
        "-S".into(),
        req.workspace.display().to_string(),
        "-B".into(),
        build_path.display().to_string(),
        "-G".into(),
        "Ninja".into(),
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".into(),
        format!("-DCMAKE_BUILD_TYPE={}", req.build_type),
    ];
    if let Some(clang) = resolve_tool(layout, "clang") {
        if let Some(w) = path_fallback_warning(layout, "clang", &clang) {
            toolchain = ToolchainSource::Path;
            warnings.push(w);
        }
        cfg_args.push(format!("-DCMAKE_C_COMPILER={}", clang.path.display()));
    }
    if let Some(clangxx) = resolve_tool(layout, "clang++") {
        if let Some(w) = path_fallback_warning(layout, "clang++", &clangxx) {
            toolchain = ToolchainSource::Path;
            warnings.push(w);
        }
        cfg_args.push(format!("-DCMAKE_CXX_COMPILER={}", clangxx.path.display()));
    }
    cfg_args.extend(req.extra_args.iter().cloned());

    let mut cfg_cmd = Command::new(&cmake.path);
    cfg_cmd.args(&cfg_args).current_dir(&req.workspace);
    if cmake.source == ToolchainSource::Anvil {
        env.apply(&mut cfg_cmd);
    }
    let cfg_out =
        run_with_timeout(&mut cfg_cmd, req.timeout).map_err(|e| format!("failed to run cmake configure: {e}"))?;
    if !cfg_out.success() {
        let diagnostics = parse_clang_diagnostics(&cfg_out.stderr);
        let (stdout, t1) = truncate_output(&cfg_out.stdout, MAX_OUTPUT_CHARS);
        let (stderr, t2) = truncate_output(&cfg_out.stderr, MAX_OUTPUT_CHARS);
        let mut full = vec![cmake.path.display().to_string()];
        full.extend(cfg_args);
        return Ok(ToolchainRunReport {
            ok: false,
            tool: "cmake".into(),
            toolchain,
            warnings,
            command: full,
            cwd: req.workspace.clone(),
            exit_code: cfg_out.status.and_then(|s| s.code()),
            timed_out: cfg_out.timed_out(),
            duration_ms: cfg_out.elapsed.as_millis() as u64,
            diagnostics,
            compile_commands: None,
            stdout,
            stderr,
            truncated: t1 || t2,
        });
    }

    // Step 2: Build.
    let mut build_args = vec!["--build".into(), build_path.display().to_string()];
    if let Some(target) = &req.target {
        if !target.trim().is_empty() {
            build_args.extend(["--target".into(), target.clone()]);
        }
    }
    let mut build_cmd = Command::new(&cmake.path);
    build_cmd.args(&build_args).current_dir(&req.workspace);
    if cmake.source == ToolchainSource::Anvil {
        env.apply(&mut build_cmd);
    }
    let build_out =
        run_with_timeout(&mut build_cmd, req.timeout).map_err(|e| format!("failed to run cmake build: {e}"))?;

    let cc_json = build_path.join("compile_commands.json");
    let compile_commands = cc_json.is_file().then_some(cc_json);

    let combined_diag_text = format!("{}\n{}", build_out.stdout, build_out.stderr);
    let diagnostics = parse_clang_diagnostics(&combined_diag_text);
    let (stdout, t1) = truncate_output(&build_out.stdout, MAX_OUTPUT_CHARS);
    let (stderr, t2) = truncate_output(&build_out.stderr, MAX_OUTPUT_CHARS);
    let mut full = vec![cmake.path.display().to_string()];
    full.extend(build_args);

    Ok(ToolchainRunReport {
        ok: build_out.success(),
        tool: "cmake".into(),
        toolchain,
        warnings,
        command: full,
        cwd: req.workspace.clone(),
        exit_code: build_out.status.and_then(|s| s.code()),
        timed_out: build_out.timed_out(),
        duration_ms: (cfg_out.elapsed + build_out.elapsed).as_millis() as u64,
        diagnostics,
        compile_commands,
        stdout,
        stderr,
        truncated: t1 || t2,
    })
}

fn compile_single_cc_file(
    layout: &InstallLayout,
    req: &CcBuildRequest,
    file: &Path,
) -> Result<ToolchainRunReport, String> {
    let abs_file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        req.workspace.join(file)
    };
    let is_cpp = matches!(
        abs_file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cc" | "cpp" | "cxx" | "mm")
    );
    let tool_name = if is_cpp { "clang++" } else { "clang" };
    let compiler = resolve_tool(layout, tool_name)
        .ok_or_else(|| format!("{tool_name} not found in Anvil toolchains or PATH (run `anvil install`)"))?;
    let warnings: Vec<String> = path_fallback_warning(layout, tool_name, &compiler)
        .into_iter()
        .collect();

    let source_dir = abs_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(&req.workspace);
    let out_path = match &req.output {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => req.workspace.join(p),
        None => {
            let stem = abs_file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "a".into());
            source_dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
        }
    };

    let mut args = Vec::new();
    for default_flag in ["-Wall", "-Wextra"] {
        if !req.extra_args.iter().any(|a| a == default_flag) {
            args.push(default_flag.into());
        }
    }
    args.push(abs_file.display().to_string());
    args.push("-o".into());
    args.push(out_path.display().to_string());
    args.extend(req.extra_args.iter().cloned());

    let mut cmd = Command::new(&compiler.path);
    cmd.args(&args).current_dir(&req.workspace);
    if compiler.source == ToolchainSource::Anvil {
        ToolEnv::for_layout(layout).apply(&mut cmd);
    }

    let out = run_with_timeout(&mut cmd, req.timeout).map_err(|e| format!("failed to run {tool_name}: {e}"))?;
    let diagnostics = parse_clang_diagnostics(&out.stderr);
    let (stdout, t1) = truncate_output(&out.stdout, MAX_OUTPUT_CHARS);
    let (stderr, t2) = truncate_output(&out.stderr, MAX_OUTPUT_CHARS);
    let mut full = vec![compiler.path.display().to_string()];
    full.extend(args);

    Ok(ToolchainRunReport {
        ok: out.success(),
        tool: tool_name.into(),
        toolchain: compiler.source,
        warnings,
        command: full,
        cwd: req.workspace.clone(),
        exit_code: out.status.and_then(|s| s.code()),
        timed_out: out.timed_out(),
        duration_ms: out.elapsed.as_millis() as u64,
        diagnostics,
        compile_commands: None,
        stdout,
        stderr,
        truncated: t1 || t2,
    })
}

pub struct CcLintFormatRequest {
    pub workspace: PathBuf,
    pub tool: String,
    pub mode: String,
    pub files: Vec<String>,
    pub build_dir: Option<String>,
    pub timeout: Duration,
}

/// Run `clang-format` or `clang-tidy` on one or more C/C++ files.
pub fn run_cc_lint_format(layout: &InstallLayout, req: &CcLintFormatRequest) -> Result<ToolchainRunReport, String> {
    if req.files.is_empty() {
        return Err("at least one file path must be passed in `files`".into());
    }
    let tool_name = match req.tool.as_str() {
        "clang-format" | "format" => "clang-format",
        "clang-tidy" | "tidy" => "clang-tidy",
        other => {
            return Err(format!(
                "unsupported tool '{other}' (expected: clang-format, clang-tidy)"
            ))
        }
    };
    let bin = resolve_tool(layout, tool_name)
        .ok_or_else(|| format!("{tool_name} not found in Anvil toolchains or PATH (run `anvil install`)"))?;
    let warnings: Vec<String> = path_fallback_warning(layout, tool_name, &bin).into_iter().collect();

    let mut args: Vec<String> = Vec::new();
    match (tool_name, req.mode.as_str()) {
        ("clang-format", "fix") => args.push("-i".into()),
        ("clang-format", "check") => args.extend(["--dry-run".into(), "--Werror".into()]),
        ("clang-tidy", "fix") => {
            if let Some(bd) = &req.build_dir {
                args.extend(["-p".into(), bd.clone()]);
            }
            args.push("--fix".into());
        }
        ("clang-tidy", "check") => {
            if let Some(bd) = &req.build_dir {
                args.extend(["-p".into(), bd.clone()]);
            }
        }
        (_, mode) => return Err(format!("unsupported mode '{mode}' (expected: check, fix)")),
    }
    args.extend(req.files.iter().cloned());

    let mut cmd = Command::new(&bin.path);
    cmd.args(&args).current_dir(&req.workspace);
    if bin.source == ToolchainSource::Anvil {
        ToolEnv::for_layout(layout).apply(&mut cmd);
    }

    let out = run_with_timeout(&mut cmd, req.timeout).map_err(|e| format!("failed to run {tool_name}: {e}"))?;
    let diagnostics = parse_clang_diagnostics(&format!("{}\n{}", out.stdout, out.stderr));
    let (stdout, t1) = truncate_output(&out.stdout, MAX_OUTPUT_CHARS);
    let (stderr, t2) = truncate_output(&out.stderr, MAX_OUTPUT_CHARS);
    let mut full = vec![bin.path.display().to_string()];
    full.extend(args);

    Ok(ToolchainRunReport {
        ok: out.success(),
        tool: tool_name.into(),
        toolchain: bin.source,
        warnings,
        command: full,
        cwd: req.workspace.clone(),
        exit_code: out.status.and_then(|s| s.code()),
        timed_out: out.timed_out(),
        duration_ms: out.elapsed.as_millis() as u64,
        diagnostics,
        compile_commands: None,
        stdout,
        stderr,
        truncated: t1 || t2,
    })
}

/// Parse Cargo `--message-format=json*` lines into clean structured diagnostics.
pub fn parse_cargo_json_diagnostics(raw: &str) -> (Vec<CompilerDiagnostic>, String) {
    let mut diags = Vec::new();
    let mut non_json_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            if !trimmed.is_empty() {
                non_json_lines.push(line);
            }
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            non_json_lines.push(line);
            continue;
        };
        match val.get("reason").and_then(Value::as_str) {
            Some("compiler-message") => {}
            Some("compiler-artifact" | "build-script-executed" | "build-finished") => continue,
            _ => {
                // Not a Cargo JSON message (e.g. JSON printed by a test): preserve in stdout.
                non_json_lines.push(line);
                continue;
            }
        }
        let Some(msg) = val.get("message") else {
            continue;
        };
        let level = msg.get("level").and_then(Value::as_str).unwrap_or("info").to_string();
        if level == "failure-note" {
            continue;
        }
        let text = msg.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let code = msg.pointer("/code/code").and_then(Value::as_str).map(str::to_string);
        let primary = msg.get("spans").and_then(Value::as_array).and_then(|spans| {
            spans
                .iter()
                .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
                .or_else(|| spans.first())
        });
        let file = primary
            .and_then(|s| s.get("file_name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let line_no = primary
            .and_then(|s| s.get("line_start"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        let col_no = primary
            .and_then(|s| s.get("column_start"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);

        let diag = CompilerDiagnostic {
            file,
            line: line_no,
            col: col_no,
            level,
            code,
            message: text,
        };
        if diags.len() < MAX_DIAGNOSTICS && !diags.contains(&diag) {
            diags.push(diag);
        }
    }

    (diags, non_json_lines.join("\n"))
}

/// Parse Clang / clang-tidy lines of the form `<file>:<line>:<col>: <error|warning|note>: <message>`.
pub fn parse_clang_diagnostics(raw: &str) -> Vec<CompilerDiagnostic> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if out.len() >= MAX_DIAGNOSTICS {
            break;
        }
        if let Some(diag) = parse_clang_line(line) {
            out.push(diag);
        }
    }
    out
}

fn parse_clang_line(line: &str) -> Option<CompilerDiagnostic> {
    for level in ["fatal error", "error", "warning"] {
        let marker = format!(": {level}: ");
        let Some((lhs, rhs)) = line.split_once(&marker) else {
            continue;
        };
        // lhs is `<file>:<line>:<col>` or `<file>:<line>`. Handle Windows `C:\...` by rsplitn.
        let mut parts = lhs.rsplitn(3, ':');
        let last = parts.next()?.trim();
        let prev = parts.next()?.trim();
        let (file, line_no, col_no) = match (parts.next(), prev.parse::<u32>(), last.parse::<u32>()) {
            (Some(f), Ok(l), Ok(c)) => (f, l, Some(c)),
            (None, _, Ok(l)) => (prev, l, None),
            _ => continue,
        };
        return Some(CompilerDiagnostic {
            file: Some(file.to_string()),
            line: Some(line_no),
            col: col_no,
            level: if level == "fatal error" {
                "error".into()
            } else {
                level.into()
            },
            code: None,
            message: rhs.trim().to_string(),
        });
    }
    None
}

fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        return (text.to_string(), false);
    }
    // Keep the tail (compiler errors and test summaries are usually at the end).
    let mut start = text.len() - max_chars;
    while !text.is_char_boundary(start) && start < text.len() {
        start += 1;
    }
    (format!("...[truncated]...\n{}", &text[start..]), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_compiler_messages() {
        let sample = concat!(
            "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"demo\"}}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":{\"code\":\"E0425\"},\"message\":\"cannot find value `x`\",\"spans\":[{\"file_name\":\"src/main.rs\",\"line_start\":4,\"column_start\":9,\"is_primary\":true}]}}\n",
            "{\"reason\":\"build-finished\",\"success\":false}\n"
        );
        let (diags, rest) = parse_cargo_json_diagnostics(sample);
        assert!(rest.is_empty());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(diags[0].line, Some(4));
        assert_eq!(diags[0].col, Some(9));
        assert_eq!(diags[0].code.as_deref(), Some("E0425"));
    }

    #[test]
    fn parses_windows_and_unix_clang_lines() {
        let sample = "C:\\proj\\main.c:12:5: error: use of undeclared identifier 'foo'\n/src/app.cpp:3:1: warning: unused variable 'x'\n";
        let diags = parse_clang_diagnostics(sample);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file.as_deref(), Some(r"C:\proj\main.c"));
        assert_eq!(diags[0].line, Some(12));
        assert_eq!(diags[0].col, Some(5));
        assert_eq!(diags[0].level, "error");
        assert_eq!(diags[1].file.as_deref(), Some("/src/app.cpp"));
        assert_eq!(diags[1].level, "warning");
    }

    #[test]
    fn parses_cargo_test_json_separating_harness_and_deduplicating() {
        let warn = "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"code\":{\"code\":\"unused_variables\"},\"message\":\"unused variable: `x`\",\"spans\":[{\"file_name\":\"src/lib.rs\",\"line_start\":2,\"column_start\":9,\"is_primary\":true}]}}\n";
        let sample = format!(
            concat!(
                "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"demo\"}}}}\n",
                "{warn}",
                "{warn}",
                "{{\"reason\":\"build-finished\",\"success\":true}}\n",
                "running 1 test\n",
                "{{\"custom_test_output\":true}}\n",
                "test works ... ok\n",
                "test result: ok. 1 passed; 0 failed; 0 ignored\n",
            ),
            warn = warn
        );
        let (diags, rest) = parse_cargo_json_diagnostics(&sample);
        assert_eq!(diags.len(), 1, "duplicate lib/test diagnostic should be deduplicated");
        assert_eq!(diags[0].level, "warning");
        assert_eq!(diags[0].code.as_deref(), Some("unused_variables"));
        assert_eq!(diags[0].file.as_deref(), Some("src/lib.rs"));
        assert_eq!(diags[0].line, Some(2));
        assert!(rest.contains("running 1 test"), "{rest}");
        assert!(rest.contains("{\"custom_test_output\":true}"), "{rest}");
        assert!(rest.contains("test result: ok."), "{rest}");
        assert!(!rest.contains("compiler-message"), "{rest}");
    }

    #[test]
    fn resolve_tool_distinguishes_anvil_and_path_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(tmp.path());

        // Empty layout falls back to PATH (cargo is on PATH during `cargo test`).
        let from_path = resolve_tool(&layout, "cargo").expect("cargo available on PATH");
        assert_eq!(from_path.source, ToolchainSource::Path);
        assert!(path_fallback_warning(&layout, "cargo", &from_path).is_some());

        // Once present in Anvil's toolchains/rust/bin, source is Anvil with no warning.
        let anvil_cargo = layout
            .toolchain_dir("rust")
            .join("bin")
            .join(format!("cargo{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(anvil_cargo.parent().unwrap()).unwrap();
        std::fs::write(&anvil_cargo, b"stub").unwrap();
        let from_anvil = resolve_tool(&layout, "cargo").unwrap();
        assert_eq!(from_anvil.source, ToolchainSource::Anvil);
        assert_eq!(from_anvil.path, anvil_cargo);
        assert!(path_fallback_warning(&layout, "cargo", &from_anvil).is_none());
    }
}
