//! MCP tool catalogue and execution dispatcher.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anvil_agents::skills as agent_skills;
use anvil_core::{EnvMode, InstallLayout};
use anvil_env::doctor::{run_doctor, DoctorOptions};
use anvil_lsp::{LanguageKind, LspPool};
use serde_json::{json, Value};

use crate::toolchain::{
    run_cargo, run_cc_build, run_cc_lint_format, CargoRequest, CcBuildRequest, CcLintFormatRequest,
};

const MAX_REFS: usize = 100;

pub struct ToolCallOutcome {
    pub payload: Value,
    pub is_error: bool,
}

impl ToolCallOutcome {
    fn ok(payload: Value) -> Self {
        Self {
            payload,
            is_error: false,
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            payload: json!({ "ok": false, "error": message.into() }),
            is_error: true,
        }
    }
}

/// Holds Anvil layout, workspace default, and the live LSP pool shared across MCP calls.
pub struct ToolRegistry {
    layout: InstallLayout,
    default_workspace: PathBuf,
    env_mode: EnvMode,
    lsp: LspPool,
}

impl ToolRegistry {
    pub fn new(layout: InstallLayout, default_workspace: PathBuf, env_mode: EnvMode) -> Self {
        let lsp = LspPool::new(layout.clone(), Some(default_workspace.clone()));
        Self {
            layout,
            default_workspace,
            env_mode,
            lsp,
        }
    }

    pub fn with_lsp_pool(mut self, lsp: LspPool) -> Self {
        self.lsp = lsp;
        self
    }

    pub fn shutdown(&self) {
        self.lsp.shutdown_all();
    }

    /// MCP `tools/list` schema definitions — concise and explicit so even small models pick accurately.
    pub fn list_tools(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "anvil_doctor",
                "description": "Check installed Anvil toolchains (Clang, Rust, CMake, Ninja, LSPs), versions, and optionally compile+run C/C++/Rust smoke tests.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "smoke_test": { "type": "boolean", "description": "Compile hello-world smoke tests in C, C++ (-static), Rust and Rust gnullvm and run them with a clean system PATH to catch missing DLLs (default false)." }
                    }
                }
            }),
            json!({
                "name": "rust_cargo",
                "description": "Run Cargo using the offline Anvil Rust toolchain and return structured compiler diagnostics (file, line, col, code, message).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["check", "build", "test", "clippy", "fmt_check", "fmt"],
                            "description": "Cargo operation to run."
                        },
                        "workspace": { "type": "string", "description": "Project directory containing Cargo.toml (defaults to server workspace)." },
                        "release": { "type": "boolean", "description": "Pass --release." },
                        "package": { "type": "string", "description": "Optional -p <package> name." },
                        "test_filter": { "type": "string", "description": "Optional test name filter when action='test'." },
                        "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional arguments passed to cargo." },
                        "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 180)." }
                    },
                    "required": ["action"]
                }
            }),
            json!({
                "name": "cc_build",
                "description": "Build a C/C++ project with CMake+Ninja (generating compile_commands.json for clangd) or compile a single C/C++ file directly with Clang (when `file` is set and `output` is omitted, writes `<stem>.exe` alongside the source file; C++ executables default to `-static` (standalone `.exe`) unless sanitizers or shared libraries are used).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workspace": { "type": "string", "description": "Project root directory (defaults to server workspace)." },
                        "file": { "type": "string", "description": "Optional single C/C++ file to compile directly with clang/clang++." },
                        "output": { "type": "string", "description": "Optional output executable path for single-file compilation (defaults to `<stem>.exe` alongside the source file)." },
                        "build_dir": { "type": "string", "description": "CMake build directory relative to workspace (default 'build')." },
                        "build_type": { "type": "string", "description": "CMAKE_BUILD_TYPE (default 'Debug')." },
                        "target": { "type": "string", "description": "Optional CMake target name." },
                        "static_link": { "type": "boolean", "description": "Link C/C++ runtime statically (-static) for standalone Windows executables without libc++.dll/libunwind.dll. Default (omitted): on for single-file C++ and for fresh CMake build directories (seeds CMAKE_EXE_LINKER_FLAGS_INIT=-static, never overriding flags already cached); off with sanitizers (-fsanitize=, -D...SANITIZ...=ON) or BUILD_SHARED_LIBS=ON. true forces CMAKE_EXE_LINKER_FLAGS=-static; false disables it." },
                        "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Extra CMake configure or compiler flags (single-file mode adds -Wall -Wextra unless already provided)." },
                        "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 180)." }
                    }
                }
            }),
            json!({
                "name": "cc_lint_format",
                "description": "Run clang-format or clang-tidy on C/C++ files in check or fix mode.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tool": { "type": "string", "enum": ["clang-format", "clang-tidy"], "description": "Tool to run." },
                        "mode": { "type": "string", "enum": ["check", "fix"], "description": "Check only or apply fixes in place (default 'check')." },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Source files to check or format." },
                        "workspace": { "type": "string", "description": "Working directory (defaults to server workspace)." },
                        "build_dir": { "type": "string", "description": "Build directory with compile_commands.json (for clang-tidy -p)." }
                    },
                    "required": ["tool", "files"]
                }
            }),
            json!({
                "name": "lsp_diagnostics",
                "description": "Get live compiler/LSP diagnostics (errors and warnings, 1-based line/col) for a Rust, C or C++ file via rust-analyzer or clangd.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "Path to the .rs, .c, .cpp or header file." },
                        "wait_ms": { "type": "integer", "description": "Milliseconds to wait for fresh diagnostics after syncing the file (default 10000 for Rust, 1200 for C/C++)." }
                    },
                    "required": ["file"]
                }
            }),
            json!({
                "name": "lsp_hover",
                "description": "Get type signature and documentation at a 1-based (line, col) position in a Rust, C or C++ file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "Path to the source file." },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line number." },
                        "col": { "type": "integer", "minimum": 1, "description": "1-based column number." }
                    },
                    "required": ["file", "line", "col"]
                }
            }),
            json!({
                "name": "lsp_definition",
                "description": "Go to symbol definition at a 1-based (line, col) position in a Rust, C or C++ file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "Path to the source file." },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line number." },
                        "col": { "type": "integer", "minimum": 1, "description": "1-based column number." }
                    },
                    "required": ["file", "line", "col"]
                }
            }),
            json!({
                "name": "lsp_references",
                "description": "Find all references to the symbol at a 1-based (line, col) position in a Rust, C or C++ file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "Path to the source file." },
                        "line": { "type": "integer", "minimum": 1, "description": "1-based line number." },
                        "col": { "type": "integer", "minimum": 1, "description": "1-based column number." },
                        "include_declaration": { "type": "boolean", "description": "Include the declaration itself (default true)." }
                    },
                    "required": ["file", "line", "col"]
                }
            }),
            json!({
                "name": "lsp_symbols",
                "description": "List symbols in a file (when `file` is given) or search symbols across the workspace (when `query` is given).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "File path to list document symbols." },
                        "query": { "type": "string", "description": "Search string for workspace-wide symbol lookup." },
                        "workspace": { "type": "string", "description": "Workspace directory for workspace symbol search." },
                        "language": { "type": "string", "enum": ["rust", "c", "cpp"], "description": "Optional language hint for workspace search." }
                    }
                }
            }),
            json!({
                "name": "skill_list",
                "description": "List built-in Anvil coding skills (Rust, Modern C, Modern C++, CMake/Ninja/Debug, Anvil Toolkit).",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }),
            json!({
                "name": "skill_read",
                "description": "Read the full Markdown instructions of a named Anvil skill.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name from skill_list (e.g. 'rust-systems', 'c-modern')." }
                    },
                    "required": ["name"]
                }
            }),
        ]
    }

    pub fn call_tool(&self, name: &str, args: &Value) -> ToolCallOutcome {
        match name {
            "anvil_doctor" => self.call_doctor(args),
            "rust_cargo" => self.call_rust_cargo(args),
            "cc_build" => self.call_cc_build(args),
            "cc_lint_format" => self.call_cc_lint_format(args),
            "lsp_diagnostics" => self.call_lsp_diagnostics(args),
            "lsp_hover" => self.call_lsp_hover(args),
            "lsp_definition" => self.call_lsp_definition(args),
            "lsp_references" => self.call_lsp_references(args),
            "lsp_symbols" => self.call_lsp_symbols(args),
            "skill_list" => self.call_skill_list(),
            "skill_read" => self.call_skill_read(args),
            other => ToolCallOutcome::err(format!("unknown tool '{other}' (call tools/list to see valid tools)")),
        }
    }

    fn resolve_workspace(&self, args: &Value) -> PathBuf {
        match args
            .get("workspace")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            Some(w) => {
                let p = PathBuf::from(w);
                if p.is_absolute() {
                    p
                } else {
                    self.default_workspace.join(p)
                }
            }
            None => self.default_workspace.clone(),
        }
    }

    fn call_doctor(&self, args: &Value) -> ToolCallOutcome {
        let mut opts = DoctorOptions::new(self.layout.clone());
        opts.smoke_test = args.get("smoke_test").and_then(Value::as_bool).unwrap_or(false);
        opts.env_mode = self.env_mode.clone();
        let report = run_doctor(&opts);
        ToolCallOutcome::ok(serde_json::to_value(report).expect("doctor report serializes"))
    }

    fn call_rust_cargo(&self, args: &Value) -> ToolCallOutcome {
        let Some(action) = args.get("action").and_then(Value::as_str) else {
            return ToolCallOutcome::err("missing required parameter `action`");
        };
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(180)
            .clamp(5, 900);
        let req = CargoRequest {
            action: action.to_string(),
            workspace: self.resolve_workspace(args),
            release: args.get("release").and_then(Value::as_bool).unwrap_or(false),
            package: args.get("package").and_then(Value::as_str).map(str::to_string),
            test_filter: args.get("test_filter").and_then(Value::as_str).map(str::to_string),
            extra_args: string_vec(args, "extra_args"),
            timeout: Duration::from_secs(timeout_secs),
        };
        match run_cargo(&self.layout, &req) {
            Ok(report) => ToolCallOutcome {
                is_error: !report.ok,
                payload: serde_json::to_value(report).expect("report serializes"),
            },
            Err(e) => ToolCallOutcome::err(e),
        }
    }

    fn call_cc_build(&self, args: &Value) -> ToolCallOutcome {
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(180)
            .clamp(5, 900);
        let req = CcBuildRequest {
            workspace: self.resolve_workspace(args),
            file: args.get("file").and_then(Value::as_str).map(PathBuf::from),
            output: args.get("output").and_then(Value::as_str).map(PathBuf::from),
            build_dir: args
                .get("build_dir")
                .and_then(Value::as_str)
                .unwrap_or("build")
                .to_string(),
            build_type: args
                .get("build_type")
                .and_then(Value::as_str)
                .unwrap_or("Debug")
                .to_string(),
            target: args.get("target").and_then(Value::as_str).map(str::to_string),
            static_link: args.get("static_link").and_then(Value::as_bool),
            extra_args: string_vec(args, "extra_args"),
            timeout: Duration::from_secs(timeout_secs),
        };
        match run_cc_build(&self.layout, &req) {
            Ok(report) => ToolCallOutcome {
                is_error: !report.ok,
                payload: serde_json::to_value(report).expect("report serializes"),
            },
            Err(e) => ToolCallOutcome::err(e),
        }
    }

    fn call_cc_lint_format(&self, args: &Value) -> ToolCallOutcome {
        let Some(tool) = args.get("tool").and_then(Value::as_str) else {
            return ToolCallOutcome::err("missing required parameter `tool`");
        };
        let req = CcLintFormatRequest {
            workspace: self.resolve_workspace(args),
            tool: tool.to_string(),
            mode: args.get("mode").and_then(Value::as_str).unwrap_or("check").to_string(),
            files: string_vec(args, "files"),
            build_dir: args.get("build_dir").and_then(Value::as_str).map(str::to_string),
            timeout: Duration::from_secs(120),
        };
        match run_cc_lint_format(&self.layout, &req) {
            Ok(report) => ToolCallOutcome {
                is_error: !report.ok,
                payload: serde_json::to_value(report).expect("report serializes"),
            },
            Err(e) => ToolCallOutcome::err(e),
        }
    }

    fn call_lsp_diagnostics(&self, args: &Value) -> ToolCallOutcome {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolCallOutcome::err("missing required parameter `file`");
        };
        let wait = args
            .get("wait_ms")
            .and_then(Value::as_u64)
            .map(|ms| Duration::from_millis(ms.clamp(50, 60_000)));
        match self.lsp.diagnostics(Path::new(file), wait) {
            Ok(items) => ToolCallOutcome::ok(json!({
                "ok": true,
                "file": file,
                "count": items.len(),
                "diagnostics": items
            })),
            Err(e) => ToolCallOutcome::err(e.to_string()),
        }
    }

    fn call_lsp_hover(&self, args: &Value) -> ToolCallOutcome {
        let (file, line, col) = match extract_position(args) {
            Ok(v) => v,
            Err(e) => return ToolCallOutcome::err(e),
        };
        match self.lsp.hover(Path::new(file), line, col) {
            Ok(hover) => ToolCallOutcome::ok(json!({
                "ok": true,
                "file": file,
                "line": line,
                "col": col,
                "hover": hover
            })),
            Err(e) => ToolCallOutcome::err(e.to_string()),
        }
    }

    fn call_lsp_definition(&self, args: &Value) -> ToolCallOutcome {
        let (file, line, col) = match extract_position(args) {
            Ok(v) => v,
            Err(e) => return ToolCallOutcome::err(e),
        };
        match self.lsp.definition(Path::new(file), line, col) {
            Ok(locations) => ToolCallOutcome::ok(json!({
                "ok": true,
                "count": locations.len(),
                "locations": locations
            })),
            Err(e) => ToolCallOutcome::err(e.to_string()),
        }
    }

    fn call_lsp_references(&self, args: &Value) -> ToolCallOutcome {
        let (file, line, col) = match extract_position(args) {
            Ok(v) => v,
            Err(e) => return ToolCallOutcome::err(e),
        };
        let include_decl = args.get("include_declaration").and_then(Value::as_bool).unwrap_or(true);
        match self.lsp.references(Path::new(file), line, col, include_decl) {
            Ok(mut locations) => {
                let total = locations.len();
                let truncated = total > MAX_REFS;
                locations.truncate(MAX_REFS);
                ToolCallOutcome::ok(json!({
                    "ok": true,
                    "total": total,
                    "truncated": truncated,
                    "locations": locations
                }))
            }
            Err(e) => ToolCallOutcome::err(e.to_string()),
        }
    }

    fn call_lsp_symbols(&self, args: &Value) -> ToolCallOutcome {
        if let Some(file) = args
            .get("file")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return match self.lsp.document_symbols(Path::new(file)) {
                Ok(symbols) => ToolCallOutcome::ok(json!({
                    "ok": true,
                    "scope": "document",
                    "file": file,
                    "count": symbols.len(),
                    "symbols": symbols
                })),
                Err(e) => ToolCallOutcome::err(e.to_string()),
            };
        }
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        let ws = self.resolve_workspace(args);
        let hint = args
            .get("language")
            .and_then(Value::as_str)
            .and_then(LanguageKind::parse_hint);
        match self.lsp.workspace_symbols(query, Some(&ws), hint) {
            Ok(mut symbols) => {
                let total = symbols.len();
                let truncated = total > MAX_REFS;
                symbols.truncate(MAX_REFS);
                ToolCallOutcome::ok(json!({
                    "ok": true,
                    "scope": "workspace",
                    "query": query,
                    "total": total,
                    "truncated": truncated,
                    "symbols": symbols
                }))
            }
            Err(e) => ToolCallOutcome::err(e.to_string()),
        }
    }

    fn call_skill_list(&self) -> ToolCallOutcome {
        let items: Vec<Value> = agent_skills::embedded()
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "title": s.title,
                    "description": s.description,
                })
            })
            .collect();
        ToolCallOutcome::ok(json!({
            "ok": true,
            "count": items.len(),
            "skills": items
        }))
    }

    fn call_skill_read(&self, args: &Value) -> ToolCallOutcome {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolCallOutcome::err("missing required parameter `name`");
        };
        if let Some(skill) = agent_skills::embedded_skill(name) {
            return ToolCallOutcome::ok(json!({
                "ok": true,
                "name": skill.name,
                "title": skill.title,
                "description": skill.description,
                "content": skill.body,
            }));
        }
        // Fallback to user/local skills in ANVIL_HOME/skills/<name>.md.
        if agent_skills::is_valid_skill_name(name) {
            let local = self.layout.skills_dir().join(format!("{name}.md"));
            if let Ok(text) = std::fs::read_to_string(&local) {
                return ToolCallOutcome::ok(json!({
                    "ok": true,
                    "name": name,
                    "content": text,
                }));
            }
        }
        ToolCallOutcome::err(format!(
            "unknown skill '{name}' (available: {})",
            agent_skills::managed_names().join(", ")
        ))
    }
}

fn extract_position(args: &Value) -> Result<(&str, u32, u32), String> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing required parameter `file`".to_string())?;
    let line = args
        .get("line")
        .and_then(Value::as_u64)
        .filter(|&n| n >= 1)
        .ok_or_else(|| "missing or invalid 1-based parameter `line`".to_string())? as u32;
    let col = args
        .get("col")
        .and_then(Value::as_u64)
        .filter(|&n| n >= 1)
        .ok_or_else(|| "missing or invalid 1-based parameter `col`".to_string())? as u32;
    Ok((file, line, col))
}

fn string_vec(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}
