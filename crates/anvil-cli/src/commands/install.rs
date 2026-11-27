use std::fmt::Write as _;
use std::fs;
use std::time::Instant;

use anvil_agents::{install_canonical_skills, AgentContext, Selection, SetupOptions};
use anvil_core::paths::{detect_source_root, resolve_install_dir};
use anvil_core::{
    extract_bundle_streaming, BundleSpec, ExtractRequest, InstallLayout, InstallState, InstalledBundle, Manifest,
    Platform, Progress, SilentProgress, StderrProgress, ANVIL_VERSION, MANIFEST_FILE,
};
use anvil_env::{apply_user_env, install_shims, EnvMode, MANAGED_TOOLS};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::InstallArgs;
use crate::commands::agents;
use crate::output::{confirm, mib, Outcome};

pub fn run(args: &InstallArgs) -> Result<Outcome> {
    let started = Instant::now();
    let platform = Platform::require_current()?;

    let source_root = detect_source_root(args.from.as_deref())?;
    let manifest_path = source_root.join(MANIFEST_FILE);
    let manifest = Manifest::load(&manifest_path).with_context(|| {
        format!("cannot load the bundle manifest (use --from <dir> to point at the drive with {MANIFEST_FILE})")
    })?;
    let only: Vec<String> = args
        .only
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let bundles = manifest.select(platform, &only)?;
    if bundles.is_empty() {
        bail!("{} has no bundles for {platform}", manifest_path.display());
    }

    let install_dir = resolve_install_dir(args.dest.as_deref())?;
    if install_dir.starts_with(&source_root) || source_root.starts_with(&install_dir) {
        bail!(
            "install directory {} overlaps the source directory {}",
            install_dir.display(),
            source_root.display()
        );
    }
    let layout = InstallLayout::new(&install_dir);

    let missing: Vec<String> = bundles
        .iter()
        .map(|b| b.archive_path(&source_root))
        .filter(|p| !p.is_file())
        .map(|p| p.display().to_string())
        .collect();
    if !missing.is_empty() {
        bail!("archive(s) listed in the manifest are missing: {}", missing.join(", "));
    }

    let state_path = layout.state_file();
    let mut state = InstallState::load(&state_path)?.unwrap_or_else(|| InstallState::new(&install_dir));
    state.install_dir = install_dir.clone();

    let plan: Vec<(&BundleSpec, bool)> = bundles
        .iter()
        .map(|b| {
            let up_to_date = !args.force
                && layout.bundle_dir(&b.target_subdir).is_dir()
                && state
                    .bundle(&b.id)
                    .is_some_and(|i| i.version == b.version && i.sha256.eq_ignore_ascii_case(&b.sha256));
            (*b, up_to_date)
        })
        .collect();
    let to_copy: u64 = plan.iter().filter(|(_, up)| !up).map(|(b, _)| b.size_bytes).sum();
    confirm(
        args.yes,
        args.json,
        &format!(
            "Install {} bundle(s) ({} to read) from {} into {}?",
            plan.len(),
            mib(to_copy),
            source_root.display(),
            install_dir.display()
        ),
    )?;

    for dir in [layout.toolchains_dir(), layout.cargo_home()] {
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }

    let mut progress: Box<dyn Progress> = if args.json {
        Box::new(SilentProgress)
    } else {
        Box::new(StderrProgress::new())
    };

    let mut results = Vec::new();
    let mut warnings = Vec::new();
    for (bundle, up_to_date) in &plan {
        let dest = layout.bundle_dir(&bundle.target_subdir);
        if *up_to_date {
            progress.message(&format!(
                "==> {} {} already installed, skipping",
                bundle.id, bundle.version
            ));
            results.push(json!({
                "id": bundle.id, "version": bundle.version, "status": "unchanged",
                "sha256": bundle.sha256, "dest": dest,
            }));
            continue;
        }
        let t = Instant::now();
        let archive = bundle.archive_path(&source_root);
        let report = extract_bundle_streaming(
            &ExtractRequest {
                id: &bundle.id,
                archive: &archive,
                expected_sha256: &bundle.sha256,
                expected_size: Some(bundle.size_bytes),
                dest: &dest,
            },
            progress.as_mut(),
        )
        .with_context(|| format!("failed to install bundle '{}' from {}", bundle.id, archive.display()))?;
        warnings.extend(report.warnings.iter().map(|w| format!("{}: {w}", bundle.id)));

        state.upsert_bundle(InstalledBundle {
            id: bundle.id.clone(),
            version: bundle.version.clone(),
            sha256: report.sha256.clone(),
            target_subdir: bundle.target_subdir.clone(),
        });
        state.save(&state_path)?; // record progress after every bundle
        results.push(json!({
            "id": bundle.id, "version": bundle.version, "status": "installed",
            "sha256": report.sha256, "archive_bytes": report.archive_bytes,
            "unpacked_bytes": report.unpacked_bytes, "files": report.files,
            "dest": dest, "duration_ms": t.elapsed().as_millis() as u64,
        }));
    }

    let exe = std::env::current_exe().context("cannot locate the running anvil executable")?;
    let tool_names: Vec<&str> = MANAGED_TOOLS.iter().map(|t| t.name).collect();
    let shims = install_shims(&layout, &exe, &tool_names)?;
    state.shims = shims.tool_names();
    for bundle in &bundles {
        for tool in bundle.tools.iter().filter(|t| !state.shims.contains(t)) {
            let reason = shims
                .skipped
                .iter()
                .find(|s| &s.tool == tool)
                .map_or("not a tool managed by anvil shims", |s| s.reason.as_str());
            warnings.push(format!(
                "bundle '{}' lists tool '{tool}' but no shim was created: {reason}",
                bundle.id
            ));
        }
    }

    let env_mode = EnvMode::from_process_env();
    let env_change = if args.skip_env {
        None
    } else {
        let change = apply_user_env(&env_mode, &layout)
            .context("failed to configure the user environment (use --skip-env to skip)")?;
        state.env_configured = true;
        Some(change)
    };

    let skills = install_canonical_skills(&layout).context("failed to write the skills into ANVIL_HOME/skills")?;

    // Agent configuration never fails the install: problems become warnings.
    let agents = if args.skip_agents {
        None
    } else {
        match AgentContext::new(&env_mode, layout.clone()) {
            Ok(ctx) => {
                let selection = if args.all_agents {
                    Selection::All
                } else {
                    Selection::Detected
                };
                let run = agents::run_setup(
                    &ctx,
                    selection,
                    SetupOptions {
                        force_all_targets: args.all_agents,
                        ..SetupOptions::default()
                    },
                );
                agents::record_setup(&mut state, &run.reports);
                warnings.extend(run.warnings.iter().cloned());
                Some(run)
            }
            Err(e) => {
                warnings.push(format!("AI agents not configured: {e}"));
                None
            }
        }
    };
    state.touch();
    state.save(&state_path)?;

    let doc = json!({
        "ok": true,
        "command": "install",
        "anvil_version": ANVIL_VERSION,
        "platform": platform,
        "source_root": source_root,
        "install_dir": install_dir,
        "state_file": state_path,
        "bundles": results,
        "shims": {
            "bin_dir": layout.bin_dir(),
            "created": shims.tool_names(),
            "skipped": shims.skipped,
            "removed": shims.removed,
        },
        "env": env_change,
        "env_configured": state.env_configured,
        "skills": {
            "dir": layout.skills_dir(),
            "installed": skills,
        },
        "agents": agents.as_ref().map(|run| json!({
            "selection": run.selection.name(),
            "agents": run.reports,
        })),
        "configured_agents": state.configured_agents,
        "warnings": warnings,
        "duration_ms": started.elapsed().as_millis() as u64,
    });
    let mut human = render_human(&doc, &layout);
    let agents_text = match &agents {
        None if args.skip_agents => "AI agents not configured (--skip-agents)\n".to_string(),
        None => String::new(),
        Some(run) => {
            // Warnings are already printed by render_human.
            let mut text = String::new();
            if !run.reports.is_empty() {
                text.push_str(&format!("AI agents ({}):\n", run.selection.name()));
            }
            for r in &run.reports {
                text.push_str(&agents::summarize(r));
                text.push('\n');
            }
            text
        }
    };
    // Keep the "next:" hint last.
    let insert_at = human.rfind("next:").unwrap_or(human.len());
    human.insert_str(insert_at, &agents_text);
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: 0,
    })
}

fn render_human(doc: &Value, layout: &InstallLayout) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "anvil installed into {}", layout.root().display());
    for b in doc["bundles"].as_array().into_iter().flatten() {
        let _ = writeln!(
            s,
            "  {:<12} {:<12} {}",
            b["id"].as_str().unwrap_or(""),
            b["version"].as_str().unwrap_or(""),
            b["status"].as_str().unwrap_or("")
        );
    }
    let created: Vec<&str> = doc["shims"]["created"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let _ = writeln!(s, "shims in {}: {}", layout.bin_dir().display(), created.join(", "));
    if doc["env"].is_null() {
        let _ = writeln!(
            s,
            "user environment not modified (--skip-env); run `anvil env` to set up a shell"
        );
    } else if doc["env"]["modified"].as_array().is_some_and(|m| !m.is_empty()) {
        let _ = writeln!(s, "user PATH updated: open a new terminal to pick it up");
    } else {
        let _ = writeln!(s, "user PATH already configured");
    }
    for w in doc["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let _ = writeln!(s, "warning: {w}");
    }
    let _ = writeln!(s, "next: anvil doctor --smoke-test");
    s
}
