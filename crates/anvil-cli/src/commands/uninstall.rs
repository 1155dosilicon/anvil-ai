use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anvil_agents::{remove_agent, Action, AgentContext, AgentId, AgentReport, RemoveOptions};
use anvil_core::paths::resolve_install_dir;
use anvil_core::{InstallLayout, InstallState};
use anvil_env::{remove_user_env, EnvMode};
use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::UninstallArgs;
use crate::commands::agents;
use crate::output::{confirm, Outcome};

pub fn run(args: &UninstallArgs) -> Result<Outcome> {
    let install_dir = resolve_install_dir(args.dest.as_deref())?;
    let layout = InstallLayout::new(&install_dir);
    // Safety net: only ever delete directories that carry an anvil state file.
    let Some(state) = InstallState::load(&layout.state_file())? else {
        bail!(
            "no anvil installation found at {} (state.json missing); refusing to delete anything",
            install_dir.display()
        );
    };
    confirm(
        args.yes,
        args.json,
        &format!("Remove {} and everything inside it?", install_dir.display()),
    )?;

    let env_mode = EnvMode::from_process_env();
    let mut warnings = Vec::new();
    let agents = if args.keep_agents {
        None
    } else {
        remove_agents(&env_mode, &layout, &mut warnings)
    };

    let env_change = if args.keep_env {
        None
    } else {
        Some(remove_user_env(&env_mode, &layout)?)
    };

    let leftovers = remove_tree(&install_dir);
    let removed = !install_dir.exists();
    let doc = json!({
        "ok": leftovers.is_empty(),
        "command": "uninstall",
        "install_dir": install_dir,
        "removed": removed,
        "bundles": state.bundles.iter().map(|b| &b.id).collect::<Vec<_>>(),
        "env": env_change,
        "agents": agents.as_ref().map(|(reports, backups)| json!({
            "agents": reports,
            "backups_dir": backups,
        })),
        "leftovers": leftovers,
        "warnings": warnings,
    });

    let mut human = if removed {
        format!("removed {}\n", install_dir.display())
    } else {
        format!(
            "partially removed {}; could not delete {} item(s) (in use?):\n",
            install_dir.display(),
            leftovers.len()
        )
    };
    for l in &leftovers {
        human.push_str(&format!("  {}\n", l.display()));
    }
    match &env_change {
        None => human.push_str("user environment left untouched (--keep-env)\n"),
        Some(c) if c.changed() => human.push_str("user PATH cleaned up\n"),
        Some(_) => {}
    }
    match &agents {
        None => human.push_str("AI agent configurations left untouched (--keep-agents)\n"),
        Some((reports, backups)) => {
            let changed: Vec<&AgentReport> = reports.iter().filter(|r| r.changed()).collect();
            for r in &changed {
                human.push_str(&agents::summarize(r));
                human.push('\n');
            }
            if !changed.is_empty() {
                human.push_str(&format!(
                    "anvil removed from {} AI agent(s); config backups in {}\n",
                    changed.len(),
                    backups.display()
                ));
            }
        }
    }
    for w in &warnings {
        human.push_str(&format!("warning: {w}\n"));
    }
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: if leftovers.is_empty() { 0 } else { 1 },
    })
}

/// Remove anvil's MCP entries and skills from every supported agent, but only
/// entries pointing at *this* installation. Backups cannot live in
/// `ANVIL_HOME/backups` (about to be deleted): they go to a temporary
/// directory (inside the sandbox in sandbox mode).
fn remove_agents(
    mode: &EnvMode,
    layout: &InstallLayout,
    warnings: &mut Vec<String>,
) -> Option<(Vec<AgentReport>, PathBuf)> {
    let mut ctx = match AgentContext::new(mode, layout.clone()) {
        Ok(ctx) => ctx,
        Err(e) => {
            warnings.push(format!("AI agent configurations not cleaned up: {e}"));
            return None;
        }
    };
    let base = match mode {
        EnvMode::Sandbox(root) => root.clone(),
        EnvMode::Real => std::env::temp_dir(),
    };
    ctx.backups_dir = base.join(format!("anvil-uninstall-backups-{}", anvil_agents::fsutil::timestamp()));
    let opts = RemoveOptions {
        only_this_install: true,
    };
    let reports: Vec<AgentReport> = AgentId::ALL
        .into_iter()
        .map(|id| remove_agent(&ctx, id, opts))
        .collect();
    for r in &reports {
        warnings.extend(r.errors.iter().map(|e| format!("{}: {e}", r.id)));
        if r.mcp.iter().any(|m| m.action == Action::Kept) {
            warnings.push(format!(
                "{}: its `anvil` MCP entry belongs to another installation and was kept",
                r.id
            ));
        }
    }
    Some((reports, ctx.backups_dir))
}

/// Remove `root` recursively. Returns paths that could not be deleted.
///
/// Windows refuses to delete running executables (e.g. this very `anvil.exe`
/// when launched from `ANVIL_HOME\bin`) but allows renaming them, so stubborn
/// files are moved into a trash directory under `%TEMP%` before retrying.
fn remove_tree(root: &Path) -> Vec<PathBuf> {
    for attempt in 0..3u64 {
        match fs::remove_dir_all(root) {
            Ok(()) => return Vec::new(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(_) => std::thread::sleep(Duration::from_millis(100 * (attempt + 1))),
        }
    }

    if cfg!(windows) {
        let trash = std::env::temp_dir().join(format!("anvil-trash-{}", std::process::id()));
        let _ = fs::create_dir_all(&trash);
        for (n, file) in remaining_files(root).into_iter().enumerate() {
            if fs::remove_file(&file).is_err() {
                let _ = fs::rename(&file, trash.join(format!("{n}.bin")));
            }
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(&trash); // best effort; running files stay until reboot/cleanup
    }

    if root.exists() {
        let left = remaining_files(root);
        if left.is_empty() {
            vec![root.to_path_buf()]
        } else {
            left
        }
    } else {
        Vec::new()
    }
}

fn remaining_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                _ => out.push(path),
            }
        }
    }
    out.sort();
    out
}
