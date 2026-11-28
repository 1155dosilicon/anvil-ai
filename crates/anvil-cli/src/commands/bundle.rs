use std::fmt::Write as _;

use anvil_bundle::{init_usb, pack_bundle, sync_usb, verify_usb, PackRequest, RecipeBook, SyncOptions};
use anvil_core::paths::absolutize;
use anvil_core::Platform;
use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::{BundleArgs, BundleCommand};
use crate::output::{mib, Outcome};

pub fn run(args: &BundleArgs) -> Result<Outcome> {
    match &args.command {
        BundleCommand::InitUsb(a) => {
            let usb = absolutize(&a.usb)?;
            let current = std::env::current_exe().ok();
            let win_exe = a.win_exe.as_deref().or(current.as_deref());

            let report = init_usb(&usb, win_exe)?;
            let mut human = format!(
                "USB initialized at {}\n  templates: {}\n  skills:    {}\n  binaries:  {}\n",
                report.usb_dir.display(),
                report.templates_written.join(", "),
                report.skills_written.join(", "),
                if report.binaries_copied.is_empty() {
                    "(none)".into()
                } else {
                    report.binaries_copied.join(", ")
                }
            );
            if !report.removed_legacy.is_empty() {
                let _ = writeln!(human, "  removed (legacy Linux): {}", report.removed_legacy.join(", "));
            }
            Ok(Outcome {
                json: Some(json!({ "ok": true, "command": "bundle init-usb", "report": report })),
                human,
                exit_code: 0,
            })
        }
        BundleCommand::Pack(a) => {
            let platform: Platform = a
                .platform
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid platform '{}': {e}", a.platform))?;
            let usb = absolutize(&a.usb)?;
            let source = absolutize(&a.source)?;
            if !source.is_dir() {
                bail!("source directory does not exist: {}", source.display());
            }
            let report = pack_bundle(&PackRequest {
                usb_dir: &usb,
                platform,
                id: &a.id,
                version: &a.version,
                source_dir: &source,
                target_subdir: None,
                archive_name: a.archive_name.as_deref(),
                level: a.level,
            })?;
            let human = format!(
                "packed {} ({}, {}) -> {} ({}, sha256 {})\n  tools: {}\n",
                report.id,
                report.platform,
                report.version,
                report.archive.display(),
                mib(report.size_bytes),
                &report.sha256[..12],
                if report.tools.is_empty() {
                    "(none detected)".into()
                } else {
                    report.tools.join(", ")
                }
            );
            Ok(Outcome {
                json: Some(json!({ "ok": true, "command": "bundle pack", "report": report })),
                human,
                exit_code: 0,
            })
        }
        BundleCommand::Sync(a) => {
            let usb = absolutize(&a.usb)?;
            let mut targets = Vec::new();
            for raw in &a.targets {
                let t = raw.trim();
                if t.is_empty() {
                    continue;
                }
                let p: Platform = t.parse().map_err(|e| anyhow::anyhow!("invalid target '{t}': {e}"))?;
                if !targets.contains(&p) {
                    targets.push(p);
                }
            }
            let book = match &a.recipe {
                Some(path) => RecipeBook::load(path)?,
                None => RecipeBook::builtin(),
            };
            let cache_dir = match &a.cache {
                Some(c) => absolutize(c)?,
                None => usb.join(".anvil-cache"),
            };
            let report = sync_usb(
                &book,
                &SyncOptions {
                    usb_dir: usb,
                    targets,
                    only: a.only.clone(),
                    cache_dir,
                    level: a.level,
                    dry_run: a.dry_run,
                },
            )?;
            let mut human = String::new();
            let _ = writeln!(
                human,
                "bundle sync ({}) for {}",
                if report.dry_run { "dry-run" } else { "complete" },
                report.usb_dir.display()
            );
            for item in &report.items {
                let _ = writeln!(
                    human,
                    "  {:<12} {:<15} {:<24} {}",
                    item.id, item.platform, item.version, item.status
                );
            }
            Ok(Outcome {
                json: Some(json!({ "ok": report.ok, "command": "bundle sync", "report": report })),
                human,
                exit_code: if report.ok { 0 } else { 1 },
            })
        }
        BundleCommand::Verify(a) => {
            let usb = absolutize(&a.usb)?;
            let report = verify_usb(&usb)?;
            let mut human = String::new();
            let _ = writeln!(
                human,
                "USB verify {}: {} ({} bundles, {})",
                if report.ok { "OK" } else { "FAILED" },
                report.usb_dir.display(),
                report.bundles.len(),
                mib(report.total_bytes)
            );
            for b in &report.bundles {
                let _ = writeln!(
                    human,
                    "  [{}] {:<12} {:<15} {}",
                    if b.ok { "OK" } else { "ERR" },
                    b.id,
                    b.platform,
                    b.version
                );
            }
            for w in &report.warnings {
                let _ = writeln!(human, "warning: {w}");
            }
            for e in &report.errors {
                let _ = writeln!(human, "error: {e}");
            }
            Ok(Outcome {
                json: Some(json!({ "ok": report.ok, "command": "bundle verify", "report": report })),
                human,
                exit_code: if report.ok { 0 } else { 1 },
            })
        }
        BundleCommand::PrintRecipe => {
            let book = RecipeBook::builtin();
            Ok(Outcome {
                json: Some(serde_json::to_value(book)?),
                human: String::new(),
                exit_code: 0,
            })
        }
    }
}
