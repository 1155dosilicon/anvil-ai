//! `anvil agents ...` and the agent steps shared with `install` / `uninstall`.

use std::fmt::Write as _;
use std::path::Path;

use anvil_agents::{
    detect_all, generic_snippet, parse_agent_list, remove_agent, setup_agent, Action, AgentContext, AgentId,
    AgentReport, ConfigFormat, RemoveOptions, Selection, SetupOptions,
};
use anvil_core::paths::resolve_existing_install_dir;
use anvil_core::{EnvMode, InstallLayout, InstallState};
use anyhow::Result;
use serde_json::{json, Value};

use crate::cli::{
    AgentsArgs, AgentsCommand, AgentsDetectArgs, AgentsRemoveArgs, AgentsSetupArgs, ConfigFormatArg, PrintConfigArgs,
};
use crate::output::{confirm, Outcome};

pub fn run(args: &AgentsArgs) -> Result<Outcome> {
    match &args.command {
        AgentsCommand::Detect(a) => detect(a),
        AgentsCommand::Setup(a) => setup(a),
        AgentsCommand::Remove(a) => remove(a),
        AgentsCommand::PrintConfig(a) => print_config(a),
    }
}

/// Agent context for the installation at `--dest` (auto-detected install dir otherwise).
pub fn context(dest: Option<&Path>) -> Result<AgentContext> {
    let layout = InstallLayout::new(resolve_existing_install_dir(dest)?);
    Ok(AgentContext::new(&EnvMode::from_process_env(), layout)?)
}

pub const NONE_DETECTED: &str = "no supported AI agent detected (SiliconFlower, Claude, Codex, Antigravity); \
     use `anvil agents setup --all` to configure all of them anyway, or `anvil agents print-config` \
     for a snippet to paste into any MCP client";

/// Result of configuring a set of agents.
pub struct SetupRun {
    pub selection: Selection,
    pub reports: Vec<AgentReport>,
    pub warnings: Vec<String>,
}

impl SetupRun {
    pub fn ok(&self) -> bool {
        self.reports.iter().all(|r| r.ok)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "selection": self.selection.name(),
            "agents": self.reports,
            "warnings": self.warnings,
        })
    }
}

pub fn run_setup(ctx: &AgentContext, selection: Selection, opts: SetupOptions) -> SetupRun {
    let ids = selection.resolve(ctx);
    let mut warnings = Vec::new();
    if ids.is_empty() {
        warnings.push(NONE_DETECTED.to_string());
    }
    if !opts.skip_mcp && !ctx.layout.anvil_exe().is_file() {
        warnings.push(format!(
            "{} does not exist yet: agents will fail to start the MCP server until `anvil install` has run",
            ctx.layout.anvil_exe().display()
        ));
    }
    let reports: Vec<AgentReport> = ids.into_iter().map(|id| setup_agent(ctx, id, opts)).collect();
    for r in &reports {
        warnings.extend(r.errors.iter().map(|e| format!("{}: {e}", r.id)));
        for s in &r.skills {
            if !s.user_owned.is_empty() {
                warnings.push(format!(
                    "{}: kept your own skill(s) {} in {} (no anvil marker)",
                    r.id,
                    s.user_owned.join(", "),
                    s.dir.display()
                ));
            }
        }
    }
    SetupRun {
        selection,
        reports,
        warnings,
    }
}

/// Record successfully configured agents in `state.configured_agents`.
pub fn record_setup(state: &mut InstallState, reports: &[AgentReport]) {
    for r in reports.iter().filter(|r| r.ok && r.mcp_configured()) {
        let id = r.id.as_str().to_string();
        if !state.configured_agents.contains(&id) {
            state.configured_agents.push(id);
        }
    }
    state.configured_agents.sort();
}

/// Forget agents from which anvil was cleanly removed.
pub fn record_removal(state: &mut InstallState, reports: &[AgentReport]) {
    for r in reports
        .iter()
        .filter(|r| r.ok && r.mcp.iter().all(|m| m.action != Action::Kept))
    {
        state.configured_agents.retain(|a| a != r.id.as_str());
    }
}

/// Apply `f` to `state.json` if the installation exists. Returns the new list.
fn update_state(layout: &InstallLayout, f: impl FnOnce(&mut InstallState)) -> Result<Option<Vec<String>>> {
    let path = layout.state_file();
    let Some(mut state) = InstallState::load(&path)? else {
        return Ok(None);
    };
    f(&mut state);
    state.save(&path)?;
    Ok(Some(state.configured_agents))
}

fn detect(args: &AgentsDetectArgs) -> Result<Outcome> {
    let ctx = context(args.dest.as_deref())?;
    let statuses = detect_all(&ctx);
    let detected: Vec<&str> = statuses.iter().filter(|s| s.detected).map(|s| s.id.as_str()).collect();
    let warnings: Vec<String> = statuses
        .iter()
        .filter(|s| s.detected)
        .flat_map(|s| s.problems())
        .collect();

    let mut human = String::new();
    for s in &statuses {
        let mcp: Vec<String> = s
            .mcp
            .iter()
            .filter(|m| m.exists || m.configured)
            .map(|m| {
                let state = if m.error.is_some() {
                    "unreadable"
                } else if m.points_to_this_install {
                    "configured"
                } else if m.configured {
                    "other-path"
                } else {
                    "not-configured"
                };
                format!("{}={state}", m.label)
            })
            .collect();
        let _ = writeln!(
            human,
            "{:<14} {:<12} mcp: {:<28} skills: {}/{}",
            s.id.as_str(),
            if s.detected { "detected" } else { "not found" },
            if mcp.is_empty() { "-".to_string() } else { mcp.join(" ") },
            s.skills_synced(),
            s.skills.iter().filter(|k| k.applicable).map(|k| k.total).sum::<usize>()
        );
    }
    if detected.is_empty() {
        let _ = writeln!(human, "note: {NONE_DETECTED}");
    }
    for w in &warnings {
        let _ = writeln!(human, "warning: {w}");
    }

    let doc = json!({
        "ok": true,
        "command": "agents detect",
        "install_dir": ctx.layout.root(),
        "anvil_exe": ctx.anvil_exe()?,
        "sandbox": ctx.sandbox,
        "detected": detected,
        "agents": statuses,
        "warnings": warnings,
    });
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: 0,
    })
}

fn setup(args: &AgentsSetupArgs) -> Result<Outcome> {
    let ctx = context(args.dest.as_deref())?;
    let selection = Selection::from_flags(parse_agent_list(&args.only)?, args.all);
    let targets = selection.resolve(&ctx);
    if !targets.is_empty() {
        confirm(
            args.yes,
            args.json,
            &format!(
                "Register {} as MCP server{} in: {}?",
                ctx.anvil_exe()?.display(),
                if args.skip_skills { "" } else { " and install skills" },
                names(&targets)
            ),
        )?;
    }
    let opts = SetupOptions {
        force_all_targets: args.all,
        skip_skills: args.skip_skills,
        skip_mcp: false,
    };
    let run = run_setup(&ctx, selection, opts);
    let configured = update_state(&ctx.layout, |s| record_setup(s, &run.reports))?;
    let human = render_reports(&run);
    let mut doc = json!({
        "ok": run.ok(),
        "command": "agents setup",
        "install_dir": ctx.layout.root(),
        "anvil_exe": ctx.anvil_exe()?,
        "sandbox": ctx.sandbox,
        "configured_agents": configured,
    });
    merge(&mut doc, run.to_json());
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: if run.ok() { 0 } else { 1 },
    })
}

fn remove(args: &AgentsRemoveArgs) -> Result<Outcome> {
    let ctx = context(args.dest.as_deref())?;
    let only = parse_agent_list(&args.only)?;
    let ids = if only.is_empty() { AgentId::ALL.to_vec() } else { only };
    confirm(
        args.yes,
        args.json,
        &format!("Remove anvil's MCP entry and skills from: {}?", names(&ids)),
    )?;
    let reports: Vec<AgentReport> = ids
        .iter()
        .map(|id| remove_agent(&ctx, *id, RemoveOptions::default()))
        .collect();
    let ok = reports.iter().all(|r| r.ok);
    let configured = update_state(&ctx.layout, |s| record_removal(s, &reports))?;
    let mut human = String::new();
    for r in &reports {
        let _ = writeln!(human, "{}", summarize(r));
        for e in &r.errors {
            let _ = writeln!(human, "  error: {e}");
        }
    }
    let doc = json!({
        "ok": ok,
        "command": "agents remove",
        "install_dir": ctx.layout.root(),
        "sandbox": ctx.sandbox,
        "agents": reports,
        "configured_agents": configured,
    });
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: if ok { 0 } else { 1 },
    })
}

fn print_config(args: &PrintConfigArgs) -> Result<Outcome> {
    let ctx = context(args.dest.as_deref())?;
    let format = match args.format {
        ConfigFormatArg::Json => ConfigFormat::Json,
        ConfigFormatArg::Toml => ConfigFormat::Toml,
    };
    Ok(Outcome {
        json: None,
        human: generic_snippet(format, &ctx.entry()?),
        exit_code: 0,
    })
}

fn names(ids: &[AgentId]) -> String {
    ids.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ")
}

fn merge(doc: &mut Value, extra: Value) {
    if let (Value::Object(doc), Value::Object(extra)) = (doc, extra) {
        doc.extend(extra);
    }
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Created => "created",
        Action::Updated => "updated",
        Action::Unchanged => "unchanged",
        Action::Removed => "removed",
        Action::Absent => "absent",
        Action::Skipped => "skipped",
        Action::Kept => "kept (other install)",
        Action::Failed => "FAILED",
    }
}

/// One line per agent: `claude: mcp cli=updated desktop=skipped; skills 5 written`.
pub fn summarize(r: &AgentReport) -> String {
    let mcp: Vec<String> = r
        .mcp
        .iter()
        .map(|m| format!("{}={}", m.label, action_name(m.action)))
        .collect();
    let written: usize = r.skills.iter().map(|s| s.written.len()).sum();
    let unchanged: usize = r.skills.iter().map(|s| s.unchanged.len()).sum();
    let removed: usize = r.skills.iter().map(|s| s.removed.len()).sum();
    let mut line = format!("  {:<14}", r.id.as_str());
    if !mcp.is_empty() {
        let _ = write!(line, " mcp {}", mcp.join(" "));
    }
    if !r.skills.is_empty() {
        let _ = write!(line, "; skills");
        if removed > 0 {
            let _ = write!(line, " {removed} removed");
        } else {
            let _ = write!(line, " {written} written, {unchanged} unchanged");
        }
    }
    line
}

pub fn render_reports(run: &SetupRun) -> String {
    let mut s = String::new();
    if !run.reports.is_empty() {
        let _ = writeln!(s, "AI agents ({}):", run.selection.name());
    }
    for r in &run.reports {
        let _ = writeln!(s, "{}", summarize(r));
    }
    for w in &run.warnings {
        let _ = writeln!(s, "warning: {w}");
    }
    s
}
