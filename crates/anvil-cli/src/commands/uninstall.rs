use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anvil_agents::{remove_agent, Action, AgentContext, AgentId, AgentReport, RemoveOptions};
use anvil_core::paths::resolve_existing_install_dir;
use anvil_core::{InstallLayout, InstallState};
use anvil_env::{remove_user_env, EnvMode};
use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::UninstallArgs;
use crate::commands::agents;
use crate::output::{confirm, Outcome};

pub fn run(args: &UninstallArgs) -> Result<Outcome> {
    let install_dir = resolve_existing_install_dir(args.dest.as_deref())?;
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
        // Renames only work within a volume: keep the trash next to `root`
        // (falling back to %TEMP%), then delete it once this process has exited.
        let name = format!(
            ".{}.anvil-trash-{}",
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id()
        );
        let trash = root
            .parent()
            .map(|p| p.join(&name))
            .filter(|t| fs::create_dir_all(t).is_ok())
            .unwrap_or_else(|| {
                let t = std::env::temp_dir().join(&name);
                let _ = fs::create_dir_all(&t);
                t
            });
        for (n, file) in remaining_files(root).into_iter().enumerate() {
            if fs::remove_file(&file).is_err() {
                let _ = fs::rename(&file, trash.join(format!("{n}.bin")));
            }
        }
        let _ = fs::remove_dir_all(root);
        if fs::remove_dir_all(&trash).is_err() && trash.exists() {
            schedule_delete_after_exit(&trash);
        }
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

/// Delete `dir` from a detached `cmd.exe` a few seconds after this process
/// exits (running executables, such as this `anvil.exe`, cannot be deleted
/// while they run but are free once they have exited).
#[cfg(windows)]
fn schedule_delete_after_exit(dir: &Path) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let script = format!(r#"/D /C "ping -n 4 127.0.0.1 >NUL & rmdir /S /Q "{}"""#, dir.display());
    let _ = std::process::Command::new(cmd)
        .raw_arg(script)
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(not(windows))]
fn schedule_delete_after_exit(_dir: &Path) {}

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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn delayed_delete_removes_directory_after_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("trash with space & amp");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("a.bin"), b"x").unwrap();
        schedule_delete_after_exit(&dir);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while dir.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(!dir.exists(), "{} still exists", dir.display());
    }
}
