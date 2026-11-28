use std::fmt::Write as _;

use anvil_core::paths::resolve_existing_install_dir;
use anvil_core::InstallLayout;
use anvil_env::{run_doctor, DoctorOptions, DoctorReport, EnvMode, Status};
use anyhow::Result;

use crate::cli::DoctorArgs;
use crate::output::Outcome;

pub fn run(args: &DoctorArgs) -> Result<Outcome> {
    let layout = InstallLayout::new(resolve_existing_install_dir(args.dest.as_deref())?);
    let mut opts = DoctorOptions::new(layout);
    opts.smoke_test = args.smoke_test;
    opts.env_mode = EnvMode::from_process_env();
    if args.smoke_test && !args.json {
        eprintln!("running smoke tests (C, C++, Rust, Rust gnullvm)...");
    }
    let report = run_doctor(&opts);
    Ok(Outcome {
        human: render(&report),
        exit_code: if report.ok { 0 } else { 1 },
        json: Some(serde_json::to_value(&report)?),
    })
}

fn mark(status: Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::Missing => "MISSING",
        Status::Failed => "FAILED",
        Status::Skipped => "skipped",
    }
}

fn render(r: &DoctorReport) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "anvil {} ({})",
        r.anvil_version,
        r.platform.map_or("unsupported platform", |p| p.as_str())
    );
    let _ = writeln!(s, "install dir : {}", r.install_dir.display());
    let _ = writeln!(s, "state.json  : {}", mark(r.state.status));
    for b in &r.state.bundles {
        let _ = writeln!(s, "  bundle {:<12} {:<12} {}", b.id, b.version, mark(b.status));
    }
    let _ = writeln!(
        s,
        "PATH        : process={} user-env={}",
        if r.path.in_process_path { "yes" } else { "no" },
        r.path.in_user_env.map_or("unknown", |v| if v { "yes" } else { "no" })
    );
    if !r.tools.is_empty() {
        let _ = writeln!(s, "tools:");
        for t in &r.tools {
            let info = t.version.as_deref().or(t.detail.as_deref()).unwrap_or("");
            let _ = writeln!(s, "  {:<14} {:<8} {}", t.name, mark(t.status), info);
        }
    }
    if !r.smoke_tests.is_empty() {
        let _ = writeln!(s, "smoke tests:");
        for t in &r.smoke_tests {
            let _ = writeln!(
                s,
                "  {:<12} {:<8} {}",
                t.language,
                mark(t.status),
                t.detail.as_deref().unwrap_or("")
            );
        }
    }
    if let Some(agents) = &r.agents {
        let list = |v: &[String]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
        let _ = writeln!(
            s,
            "AI agents   : detected={} configured={}",
            list(&agents.detected),
            list(&agents.configured)
        );
    }
    for w in &r.warnings {
        let _ = writeln!(s, "warning: {w}");
    }
    for e in &r.errors {
        let _ = writeln!(s, "error: {e}");
    }
    let _ = writeln!(s, "{}", if r.ok { "healthy" } else { "problems found" });
    s
}
