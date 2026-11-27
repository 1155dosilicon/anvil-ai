//! `anvil doctor`: installation health checks and compile/run smoke tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anvil_agents::{AgentContext, AgentStatus};
use anvil_core::{InstallLayout, InstallState, InstalledBundle, Platform, ANVIL_VERSION};
use serde::Serialize;

use crate::dispatch::ToolEnv;
use crate::process::{run_with_timeout, Captured};
use crate::tools::{find_real_tool, managed_tool, ManagedTool};
use crate::userenv::{user_env_configured, EnvMode};
use crate::winpath;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub layout: InstallLayout,
    pub smoke_test: bool,
    pub env_mode: EnvMode,
    /// Inspect AI agent configurations (informational, never fails the report).
    pub check_agents: bool,
    /// Timeout for each `--version` probe.
    pub probe_timeout: Duration,
    /// Timeout for each compile / run step of the smoke tests.
    pub smoke_timeout: Duration,
}

impl DoctorOptions {
    pub fn new(layout: InstallLayout) -> Self {
        Self {
            layout,
            smoke_test: false,
            env_mode: EnvMode::Real,
            check_agents: true,
            probe_timeout: Duration::from_secs(20),
            smoke_timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Missing,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// `true` when the installation is usable (state valid, every tool working, no smoke test failed).
    pub ok: bool,
    pub anvil_version: String,
    pub platform: Option<Platform>,
    pub install_dir: PathBuf,
    pub state: StateCheck,
    pub path: PathCheck,
    pub tools: Vec<ToolCheck>,
    /// Empty unless smoke tests were requested.
    pub smoke_tests: Vec<SmokeTest>,
    /// AI agent integration (informational: problems are warnings, not errors).
    pub agents: Option<AgentsCheck>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentsCheck {
    /// Agents found on this machine.
    pub detected: Vec<String>,
    /// Agents whose MCP configuration registers this installation.
    pub configured: Vec<String>,
    /// Agents recorded in `state.json` by `anvil agents setup` / `anvil install`.
    pub recorded: Vec<String>,
    pub agents: Vec<AgentStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateCheck {
    pub status: Status,
    pub file: PathBuf,
    pub installed_at: Option<String>,
    pub anvil_version: Option<String>,
    pub env_configured: Option<bool>,
    pub bundles: Vec<BundleCheck>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleCheck {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathCheck {
    pub bin_dir: PathBuf,
    /// `bin/` is on the PATH of the current process.
    pub in_process_path: bool,
    /// `bin/` is in the persistent user environment (registry / profile files).
    pub in_user_env: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCheck {
    pub name: String,
    pub toolchain: String,
    pub status: Status,
    pub shim: Option<PathBuf>,
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmokeTest {
    /// `c`, `c++` or `rust`.
    pub language: String,
    pub status: Status,
    pub compiler: Option<PathBuf>,
    pub duration_ms: u64,
    pub detail: Option<String>,
}

pub fn run_doctor(opts: &DoctorOptions) -> DoctorReport {
    let layout = &opts.layout;
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let (state, loaded) = check_state(layout);
    if let Some(err) = &state.error {
        errors.push(err.clone());
    }
    for b in state.bundles.iter().filter(|b| b.status != Status::Ok) {
        errors.push(format!(
            "bundle '{}' is recorded in state.json but {} is missing",
            b.id,
            b.dir.display()
        ));
    }

    let path = check_path(opts, &mut warnings);

    let tools: Vec<ToolCheck> = loaded
        .as_ref()
        .map(|s| s.shims.iter().map(|name| check_tool(opts, name)).collect())
        .unwrap_or_default();
    for t in tools.iter().filter(|t| t.status != Status::Ok) {
        errors.push(format!(
            "tool '{}' is {:?}{}",
            t.name,
            t.status,
            t.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default()
        ));
    }

    let agents = opts
        .check_agents
        .then(|| check_agents(opts, loaded.as_ref(), &mut warnings));

    let smoke_tests = if opts.smoke_test {
        run_smoke_tests(opts)
    } else {
        Vec::new()
    };
    for s in smoke_tests.iter().filter(|s| s.status == Status::Failed) {
        errors.push(format!(
            "{} smoke test failed: {}",
            s.language,
            s.detail.as_deref().unwrap_or("unknown error")
        ));
    }

    DoctorReport {
        ok: errors.is_empty(),
        anvil_version: ANVIL_VERSION.to_string(),
        platform: Platform::current(),
        install_dir: layout.root().to_path_buf(),
        state,
        path,
        tools,
        smoke_tests,
        agents,
        warnings,
        errors,
    }
}

fn check_agents(opts: &DoctorOptions, state: Option<&InstallState>, warnings: &mut Vec<String>) -> AgentsCheck {
    let recorded = state.map(|s| s.configured_agents.clone()).unwrap_or_default();
    let mut check = AgentsCheck {
        detected: Vec::new(),
        configured: Vec::new(),
        recorded: recorded.clone(),
        agents: Vec::new(),
        error: None,
    };
    let ctx = match AgentContext::new(&opts.env_mode, opts.layout.clone()) {
        Ok(ctx) => ctx,
        Err(e) => {
            warnings.push(format!("could not inspect AI agents: {e}"));
            check.error = Some(e.to_string());
            return check;
        }
    };
    check.agents = anvil_agents::detect_all(&ctx);
    for status in &check.agents {
        let id = status.id.as_str().to_string();
        let is_recorded = recorded.contains(&id);
        if status.detected {
            check.detected.push(id.clone());
        }
        if status.configured {
            check.configured.push(id.clone());
        }
        if status.detected || is_recorded {
            let problems = status.problems();
            if problems.is_empty() && is_recorded && !status.configured {
                warnings.push(format!(
                    "{id}: configured by anvil earlier but its MCP entry is missing (run `anvil agents setup --only {id}`)"
                ));
            }
            warnings.extend(problems);
        }
    }
    check
}

fn check_state(layout: &InstallLayout) -> (StateCheck, Option<InstallState>) {
    let file = layout.state_file();
    let mut check = StateCheck {
        status: Status::Missing,
        file: file.clone(),
        installed_at: None,
        anvil_version: None,
        env_configured: None,
        bundles: Vec::new(),
        error: None,
    };
    match InstallState::load(&file) {
        Ok(Some(state)) => {
            check.status = Status::Ok;
            check.installed_at = Some(state.installed_at.clone());
            check.anvil_version = Some(state.anvil_version.clone());
            check.env_configured = Some(state.env_configured);
            check.bundles = state.bundles.iter().map(|b| check_bundle(layout, b)).collect();
            (check, Some(state))
        }
        Ok(None) => {
            check.error = Some(format!(
                "no anvil installation at {} (state.json not found; run `anvil install`)",
                layout.root().display()
            ));
            (check, None)
        }
        Err(e) => {
            check.status = Status::Failed;
            check.error = Some(e.to_string());
            (check, None)
        }
    }
}

fn check_bundle(layout: &InstallLayout, b: &InstalledBundle) -> BundleCheck {
    let dir = layout.bundle_dir(&b.target_subdir);
    BundleCheck {
        id: b.id.clone(),
        version: b.version.clone(),
        status: if dir.is_dir() { Status::Ok } else { Status::Missing },
        dir,
    }
}

fn check_path(opts: &DoctorOptions, warnings: &mut Vec<String>) -> PathCheck {
    let bin_dir = opts.layout.bin_dir();
    let in_process_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|entry| same_dir(&entry, &bin_dir)))
        .unwrap_or(false);
    let in_user_env = match user_env_configured(&opts.env_mode, &opts.layout) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("could not read the persistent user environment: {e}"));
            None
        }
    };
    if !in_process_path {
        warnings.push(format!(
            "{} is not on PATH of this process (open a new terminal or run `anvil env`)",
            bin_dir.display()
        ));
    }
    if in_user_env == Some(false) {
        warnings.push(
            "the persistent user environment does not include anvil (run `anvil install` without --skip-env)".into(),
        );
    }
    PathCheck {
        bin_dir,
        in_process_path,
        in_user_env,
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        winpath::entries_equal(&a.to_string_lossy(), &b.to_string_lossy())
    } else {
        let trim = |p: &Path| p.to_string_lossy().trim_end_matches('/').to_string();
        trim(a) == trim(b)
    }
}

/// Arguments that make a tool print its version and exit.
fn version_args(tool: &str) -> &'static [&'static str] {
    match tool {
        // `lld` alone is only a dispatcher; pick the ELF flavour explicitly.
        "lld" => &["-flavor", "gnu", "--version"],
        _ => &["--version"],
    }
}

fn check_tool(opts: &DoctorOptions, name: &str) -> ToolCheck {
    let layout = &opts.layout;
    let mut check = ToolCheck {
        name: name.to_string(),
        toolchain: String::new(),
        status: Status::Missing,
        shim: None,
        binary: None,
        version: None,
        detail: None,
    };
    let Some(tool) = managed_tool(name) else {
        check.status = Status::Failed;
        check.detail = Some("not a managed tool".into());
        return check;
    };
    check.toolchain = tool.toolchain.to_string();
    let shim = layout.shim_path(tool.name);
    check.shim = shim.exists().then(|| shim.clone());
    check.binary = find_real_tool(layout, tool);

    let Some(binary) = &check.binary else {
        check.detail = Some(format!("no binary in toolchains/{}/bin", tool.toolchain));
        return check;
    };
    if check.shim.is_none() {
        check.status = Status::Missing;
        check.detail = Some(format!("shim {} is missing (re-run `anvil install`)", shim.display()));
        return check;
    }

    // Probe through the shim: this validates the whole dispatch chain.
    let mut cmd = Command::new(&shim);
    cmd.args(version_args(tool.name));
    ToolEnv::for_layout(layout).apply(&mut cmd);
    match run_with_timeout(&mut cmd, opts.probe_timeout) {
        Ok(out) if out.success() => {
            check.status = Status::Ok;
            check.version = out.first_line();
        }
        Ok(out) => {
            check.status = Status::Failed;
            check.detail = Some(out.describe_failure());
        }
        Err(e) => {
            check.status = Status::Failed;
            check.detail = Some(format!("cannot run {} ({}): {e}", shim.display(), binary.display()));
        }
    }
    check
}

const MARKER: &str = "hello from anvil";

struct SmokeCase {
    language: &'static str,
    compiler: &'static str,
    file: &'static str,
    source: &'static str,
    extra_args: &'static [&'static str],
}

const SMOKE_CASES: &[SmokeCase] = &[
    SmokeCase {
        language: "c",
        compiler: "clang",
        file: "hello.c",
        source: "#include <stdio.h>\nint main(void) { printf(\"hello from anvil (c)\\n\"); return 0; }\n",
        extra_args: &[],
    },
    SmokeCase {
        language: "c++",
        compiler: "clang++",
        file: "hello.cpp",
        source: "#include <iostream>\nint main() { std::cout << \"hello from anvil (c++)\" << std::endl; return 0; }\n",
        extra_args: &[],
    },
    SmokeCase {
        language: "rust",
        compiler: "rustc",
        file: "hello.rs",
        source: "fn main() { println!(\"hello from anvil (rust)\"); }\n",
        extra_args: &["--edition", "2021"],
    },
];

fn run_smoke_tests(opts: &DoctorOptions) -> Vec<SmokeTest> {
    let workdir = match tempfile::Builder::new().prefix("anvil-smoke-").tempdir() {
        Ok(d) => d,
        Err(e) => {
            return SMOKE_CASES
                .iter()
                .map(|c| SmokeTest {
                    language: c.language.into(),
                    status: Status::Failed,
                    compiler: None,
                    duration_ms: 0,
                    detail: Some(format!("cannot create temporary directory: {e}")),
                })
                .collect()
        }
    };
    SMOKE_CASES
        .iter()
        .map(|case| run_smoke_case(opts, case, workdir.path()))
        .collect()
}

fn run_smoke_case(opts: &DoctorOptions, case: &SmokeCase, dir: &Path) -> SmokeTest {
    let mut result = SmokeTest {
        language: case.language.into(),
        status: Status::Skipped,
        compiler: None,
        duration_ms: 0,
        detail: None,
    };
    let tool: &ManagedTool = managed_tool(case.compiler).expect("smoke compilers are managed tools");
    let Some(compiler) = find_real_tool(&opts.layout, tool) else {
        result.detail = Some(format!(
            "{} is not installed (toolchains/{}/bin)",
            case.compiler, tool.toolchain
        ));
        return result;
    };
    result.compiler = Some(compiler.clone());

    let src = dir.join(case.file);
    let exe = dir.join(format!(
        "hello-{}{}",
        case.language.replace('+', "p"),
        std::env::consts::EXE_SUFFIX
    ));
    if let Err(e) = std::fs::write(&src, case.source) {
        result.status = Status::Failed;
        result.detail = Some(format!("cannot write {}: {e}", src.display()));
        return result;
    }

    let env = ToolEnv::for_layout(&opts.layout);
    let mut compile = Command::new(&compiler);
    compile
        .current_dir(dir)
        .args(case.extra_args)
        .arg(&src)
        .arg("-o")
        .arg(&exe);
    env.apply(&mut compile);

    let step = |cmd: &mut Command, what: &str| -> Result<Captured, String> {
        match run_with_timeout(cmd, opts.smoke_timeout) {
            Ok(out) if out.success() => Ok(out),
            Ok(out) => Err(format!("{what} failed ({})", out.describe_failure())),
            Err(e) => Err(format!("{what} could not start: {e}")),
        }
    };

    let outcome = step(&mut compile, "compilation").and_then(|c| {
        let mut run = Command::new(&exe);
        run.current_dir(dir);
        env.apply(&mut run);
        step(&mut run, "execution").map(|r| (c.elapsed + r.elapsed, r))
    });
    match outcome {
        Ok((elapsed, run)) if run.stdout.contains(MARKER) => {
            result.status = Status::Ok;
            result.duration_ms = elapsed.as_millis() as u64;
            result.detail = run.first_line();
        }
        Ok((elapsed, run)) => {
            result.status = Status::Failed;
            result.duration_ms = elapsed.as_millis() as u64;
            result.detail = Some(format!("unexpected program output: {:?}", run.stdout.trim()));
        }
        Err(detail) => {
            result.status = Status::Failed;
            result.detail = Some(detail);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(root: &Path, smoke: bool) -> DoctorOptions {
        let mut o = DoctorOptions::new(InstallLayout::new(root));
        o.smoke_test = smoke;
        o.env_mode = EnvMode::Sandbox(root.join("sandbox"));
        o
    }

    #[test]
    fn missing_installation_is_reported_not_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let report = run_doctor(&opts(dir.path(), true));
        assert!(!report.ok);
        assert_eq!(report.state.status, Status::Missing);
        assert!(report.tools.is_empty());
        assert_eq!(report.smoke_tests.len(), 3);
        assert!(report.smoke_tests.iter().all(|s| s.status == Status::Skipped));
        assert_eq!(report.path.in_user_env, Some(false));
        // Must serialize to JSON.
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["state"]["status"], "missing");
        assert_eq!(json["smoke_tests"][2]["language"], "rust");
    }

    #[test]
    fn healthy_empty_installation() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let mut state = InstallState::new(dir.path());
        state.upsert_bundle(InstalledBundle {
            id: "build-tools".into(),
            version: "1".into(),
            sha256: "a".repeat(64),
            target_subdir: "toolchains/build-tools".into(),
        });
        std::fs::create_dir_all(layout.toolchain_dir("build-tools")).unwrap();
        state.save(&layout.state_file()).unwrap();
        let report = run_doctor(&opts(dir.path(), false));
        assert!(report.ok, "{:?}", report.errors);
        assert_eq!(report.state.bundles[0].status, Status::Ok);
        assert!(report.smoke_tests.is_empty());

        std::fs::remove_dir(layout.toolchain_dir("build-tools")).unwrap();
        let report = run_doctor(&opts(dir.path(), false));
        assert!(!report.ok);
        assert_eq!(report.state.bundles[0].status, Status::Missing);
    }

    #[test]
    fn agents_block_is_informational() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let mut state = InstallState::new(dir.path());
        state.configured_agents = vec!["claude".into()];
        state.save(&layout.state_file()).unwrap();
        // Sandboxed Antigravity whose anvil entry points to a deleted installation.
        let config = dir.path().join("sandbox/home/.gemini/antigravity/mcp_config.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            r#"{"mcpServers":{"anvil":{"command":"/gone/bin/anvil","args":["mcp"]}}}"#,
        )
        .unwrap();

        let report = run_doctor(&opts(dir.path(), false));
        assert!(report.ok, "agent problems never fail doctor: {:?}", report.errors);
        let agents = report.agents.as_ref().unwrap();
        assert_eq!(agents.detected, vec!["antigravity"]);
        assert!(agents.configured.is_empty());
        assert_eq!(agents.recorded, vec!["claude"]);
        assert!(
            report.warnings.iter().any(|w| w.contains("missing executable")),
            "{:?}",
            report.warnings
        );
        assert!(
            report.warnings.iter().any(|w| w.starts_with("claude:")),
            "{:?}",
            report.warnings
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["agents"]["agents"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn shim_without_binary_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let check = check_tool(&opts(dir.path(), false), "clang");
        assert_eq!(check.status, Status::Missing);
        assert_eq!(check.toolchain, "llvm");
    }
}
