//! `anvil skills list | read | sync`.

use std::fmt::Write as _;
use std::fs;

use anvil_agents::skills::is_valid_skill_name;
use anvil_agents::{
    embedded, embedded_skill, install_canonical_skills, parse_agent_list, Selection, SetupOptions, Skill,
};
use anvil_core::paths::resolve_install_dir;
use anvil_core::InstallLayout;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::{SkillsArgs, SkillsCommand, SkillsListArgs, SkillsReadArgs, SkillsSyncArgs};
use crate::commands::agents::{context, render_reports, run_setup};
use crate::output::Outcome;

pub fn run(args: &SkillsArgs) -> Result<Outcome> {
    match &args.command {
        SkillsCommand::List(a) => list(a),
        SkillsCommand::Read(a) => read(a),
        SkillsCommand::Sync(a) => sync(a),
    }
}

/// Skills in `ANVIL_HOME/skills/` that are not embedded in this binary.
/// The install directory is optional here: `skills` must work from the USB drive.
fn extra_skills(dest: Option<&std::path::Path>) -> Vec<(Skill, std::path::PathBuf)> {
    let Ok(dir) = resolve_install_dir(dest).map(|d| InstallLayout::new(d).skills_dir()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(Skill, std::path::PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| {
            let skill = Skill::parse(&fs::read_to_string(&p).ok()?).ok()?;
            (embedded_skill(&skill.name).is_none()).then_some((skill, p))
        })
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

fn skill_json(skill: &Skill, source: &str, path: Option<&std::path::Path>) -> Value {
    json!({
        "name": skill.name,
        "title": skill.title,
        "description": skill.description,
        "source": source,
        "path": path,
    })
}

fn list(args: &SkillsListArgs) -> Result<Outcome> {
    let extra = extra_skills(args.dest.as_deref());
    let mut items: Vec<Value> = embedded().iter().map(|s| skill_json(s, "embedded", None)).collect();
    items.extend(extra.iter().map(|(s, p)| skill_json(s, "anvil-home", Some(p))));

    let mut human = String::new();
    for s in embedded().iter().chain(extra.iter().map(|(s, _)| s)) {
        let _ = writeln!(human, "{:<20} {}", s.name, s.description);
    }
    let _ = writeln!(human, "\nread one with: anvil skills read <name>");
    Ok(Outcome {
        json: Some(json!({ "ok": true, "command": "skills list", "skills": items })),
        human,
        exit_code: 0,
    })
}

fn read(args: &SkillsReadArgs) -> Result<Outcome> {
    if !is_valid_skill_name(&args.name) {
        bail!("invalid skill name '{}' (run `anvil skills list`)", args.name);
    }
    let (skill, source, path) = match embedded_skill(&args.name) {
        Some(skill) => (skill.clone(), "embedded", None),
        None => {
            let dir = InstallLayout::new(resolve_install_dir(args.dest.as_deref())?).skills_dir();
            let path = dir.join(format!("{}.md", args.name));
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(anvil_agents::AgentError::UnknownSkill(args.name.clone()).into())
                }
                Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
            };
            (Skill::parse(&text)?, "anvil-home", Some(path))
        }
    };
    let content = skill.canonical();
    let mut doc = skill_json(&skill, source, path.as_deref());
    doc["ok"] = json!(true);
    doc["command"] = json!("skills read");
    doc["content"] = json!(content);
    Ok(Outcome {
        json: Some(doc),
        human: content,
        exit_code: 0,
    })
}

fn sync(args: &SkillsSyncArgs) -> Result<Outcome> {
    let ctx = context(args.dest.as_deref())?;
    let selection = Selection::from_flags(parse_agent_list(&args.only)?, args.all);
    let canonical = if ctx.layout.state_file().is_file() {
        Some(install_canonical_skills(&ctx.layout)?)
    } else {
        None
    };
    let run = run_setup(
        &ctx,
        selection,
        SetupOptions {
            force_all_targets: args.all,
            skip_skills: false,
            skip_mcp: true,
        },
    );
    let mut human = render_reports(&run);
    if let Some(names) = &canonical {
        let _ = writeln!(
            human,
            "{} skills refreshed in {}",
            names.len(),
            ctx.layout.skills_dir().display()
        );
    }
    let doc = json!({
        "ok": run.ok(),
        "command": "skills sync",
        "install_dir": ctx.layout.root(),
        "sandbox": ctx.sandbox,
        "anvil_home_skills": canonical,
        "selection": run.selection.name(),
        "agents": run.reports,
        "warnings": run.warnings,
    });
    Ok(Outcome {
        json: Some(doc),
        human,
        exit_code: if run.ok() { 0 } else { 1 },
    })
}
