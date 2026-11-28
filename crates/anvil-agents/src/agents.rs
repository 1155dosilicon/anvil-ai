//! Agent adapters: detection, setup (MCP + skills) and surgical removal.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anvil_core::error::IoContext;
use anvil_core::{EnvMode, InstallLayout, ANVIL_HOME_ENV};
use serde::Serialize;

use crate::config::{self, ConfigFormat, McpEntry};
use crate::error::{AgentError, Result};
use crate::fsutil::{backup, read_optional, remove_dir_if_empty, write_atomic};
use crate::paths::AgentPaths;
use crate::skills::{embedded, is_managed, Skill, SkillLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    SiliconFlower,
    Claude,
    Codex,
    Antigravity,
}

impl AgentId {
    pub const ALL: [AgentId; 4] = [
        AgentId::SiliconFlower,
        AgentId::Claude,
        AgentId::Codex,
        AgentId::Antigravity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::SiliconFlower => "siliconflower",
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
            AgentId::Antigravity => "antigravity",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AgentId::SiliconFlower => "SiliconFlower",
            AgentId::Claude => "Claude Code / Claude Desktop",
            AgentId::Codex => "Codex",
            AgentId::Antigravity => "Antigravity",
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentId {
    type Err = AgentError;
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        AgentId::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or(AgentError::UnknownAgent(s))
    }
}

/// Parse `--only a,b` values (each element may itself contain commas). Deduplicated.
pub fn parse_agent_list<S: AsRef<str>>(items: &[S]) -> Result<Vec<AgentId>> {
    let mut out = Vec::new();
    for part in items.iter().flat_map(|s| s.as_ref().split(',')) {
        if part.trim().is_empty() {
            continue;
        }
        let id: AgentId = part.parse()?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// When an optional target is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    Always,
    /// Only when the parent directory exists, unless every target is forced (`--all`).
    ParentExistsOrForced,
    /// Only when the given directory already exists (never created by anvil).
    DirExists,
}

#[derive(Debug, Clone)]
struct McpTarget {
    label: &'static str,
    path: PathBuf,
    format: ConfigFormat,
    stdio_type: bool,
    condition: Condition,
}

#[derive(Debug, Clone)]
struct SkillTarget {
    dir: PathBuf,
    layout: SkillLayout,
    condition: Condition,
    /// Directory whose existence enables a [`Condition::DirExists`] target.
    gate: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct AgentSpec {
    /// Any of these existing means the agent is installed / has been used.
    detect: Vec<PathBuf>,
    mcp: Vec<McpTarget>,
    skills: Vec<SkillTarget>,
}

fn spec(paths: &AgentPaths, id: AgentId) -> AgentSpec {
    let folder = |dir: PathBuf| SkillTarget {
        dir,
        layout: SkillLayout::Folder,
        condition: Condition::Always,
        gate: None,
    };
    let json = |label, path, stdio_type, condition| McpTarget {
        label,
        path,
        format: ConfigFormat::Json,
        stdio_type,
        condition,
    };
    match id {
        AgentId::SiliconFlower => {
            let dir = paths.siliconflower_dir();
            AgentSpec {
                detect: vec![dir.clone()],
                mcp: vec![json("config", dir.join("config.json"), false, Condition::Always)],
                skills: vec![SkillTarget {
                    dir: dir.join("skills"),
                    layout: SkillLayout::Flat,
                    condition: Condition::Always,
                    gate: None,
                }],
            }
        }
        AgentId::Claude => {
            let desktop = paths.claude_desktop_dir();
            AgentSpec {
                detect: vec![paths.claude_cli_config(), paths.claude_dir(), desktop.clone()],
                mcp: vec![
                    json("cli", paths.claude_cli_config(), true, Condition::Always),
                    json(
                        "desktop",
                        desktop.join("claude_desktop_config.json"),
                        false,
                        Condition::ParentExistsOrForced,
                    ),
                ],
                skills: vec![folder(paths.claude_dir().join("skills"))],
            }
        }
        AgentId::Codex => {
            let dir = paths.codex_dir();
            let shared = paths.shared_agents_dir();
            AgentSpec {
                detect: vec![dir.clone()],
                mcp: vec![McpTarget {
                    label: "config",
                    path: dir.join("config.toml"),
                    format: ConfigFormat::Toml,
                    stdio_type: false,
                    condition: Condition::Always,
                }],
                skills: vec![
                    folder(dir.join("skills")),
                    SkillTarget {
                        dir: shared.join("skills"),
                        layout: SkillLayout::Folder,
                        condition: Condition::DirExists,
                        gate: Some(shared),
                    },
                ],
            }
        }
        AgentId::Antigravity => {
            let global = paths.gemini_config_dir();
            let legacy = paths.antigravity_dir();
            AgentSpec {
                detect: vec![global.clone(), legacy.clone()],
                mcp: vec![
                    json("global", global.join("mcp_config.json"), false, Condition::Always),
                    json(
                        "legacy",
                        legacy.join("mcp_config.json"),
                        false,
                        Condition::ParentExistsOrForced,
                    ),
                ],
                skills: vec![
                    folder(global.join("skills")),
                    SkillTarget {
                        dir: legacy.join("skills"),
                        layout: SkillLayout::Folder,
                        condition: Condition::DirExists,
                        gate: Some(legacy),
                    },
                ],
            }
        }
    }
}

fn mcp_applies(t: &McpTarget, force_all: bool) -> bool {
    match t.condition {
        Condition::Always => true,
        Condition::ParentExistsOrForced => force_all || t.path.parent().is_some_and(Path::is_dir),
        Condition::DirExists => t.path.parent().is_some_and(Path::is_dir),
    }
}

fn skills_apply(t: &SkillTarget, force_all: bool) -> bool {
    match t.condition {
        Condition::Always => true,
        Condition::ParentExistsOrForced => force_all || t.dir.parent().is_some_and(Path::is_dir),
        Condition::DirExists => t.gate.as_deref().unwrap_or(&t.dir).is_dir(),
    }
}

/// Paths are compared case- and separator-insensitively on Windows.
pub fn same_path(a: &str, b: &Path) -> bool {
    let b = b.to_string_lossy();
    let norm = |s: &str| s.replace('/', "\\").to_lowercase().trim_end_matches('\\').to_string();
    norm(a) == norm(&b)
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b.as_ref())),
            (Ok(x), Ok(y)) if x == y
        )
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Everything an agent operation needs: where agents live and which anvil to register.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub paths: AgentPaths,
    pub layout: InstallLayout,
    pub sandbox: bool,
    /// Where config files are copied before every change
    /// (default `ANVIL_HOME/backups`; `anvil uninstall` points it elsewhere).
    pub backups_dir: PathBuf,
}

impl AgentContext {
    pub fn new(mode: &EnvMode, layout: InstallLayout) -> Result<Self> {
        Ok(Self {
            paths: AgentPaths::from_mode(mode)?,
            backups_dir: layout.backups_dir(),
            layout,
            sandbox: mode.is_sandbox(),
        })
    }

    pub fn with_paths(paths: AgentPaths, layout: InstallLayout) -> Self {
        Self {
            paths,
            backups_dir: layout.backups_dir(),
            layout,
            sandbox: true,
        }
    }

    /// Absolute path of `ANVIL_HOME/bin/anvil(.exe)`: GUI agents do not read
    /// shell profiles, so a bare `anvil` would not be found.
    pub fn anvil_exe(&self) -> Result<PathBuf> {
        let exe = self.layout.anvil_exe();
        let exe = if exe.is_absolute() {
            exe
        } else {
            anvil_core::paths::absolutize(&exe)?
        };
        Ok(exe)
    }

    /// Absolute path of `ANVIL_HOME`.
    pub fn install_dir(&self) -> Result<PathBuf> {
        let root = self.layout.root();
        if root.is_absolute() {
            Ok(root.to_path_buf())
        } else {
            Ok(anvil_core::paths::absolutize(root)?)
        }
    }

    pub fn entry(&self) -> Result<McpEntry> {
        let home = self.install_dir()?.display().to_string();
        Ok(McpEntry::new(self.anvil_exe()?.display().to_string()).with_env(ANVIL_HOME_ENV, home))
    }

    pub fn is_detected(&self, id: AgentId) -> bool {
        spec(&self.paths, id).detect.iter().any(|p| p.exists())
    }

    pub fn detected(&self) -> Vec<AgentId> {
        AgentId::ALL.into_iter().filter(|a| self.is_detected(*a)).collect()
    }
}

/// Which agents a command applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Detected,
    All,
    Only(Vec<AgentId>),
}

impl Selection {
    pub fn from_flags(only: Vec<AgentId>, all: bool) -> Self {
        if !only.is_empty() {
            Selection::Only(only)
        } else if all {
            Selection::All
        } else {
            Selection::Detected
        }
    }

    pub fn resolve(&self, ctx: &AgentContext) -> Vec<AgentId> {
        match self {
            Selection::Detected => ctx.detected(),
            Selection::All => AgentId::ALL.to_vec(),
            Selection::Only(ids) => ids.clone(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Selection::Detected => "detected",
            Selection::All => "all",
            Selection::Only(_) => "only",
        }
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Created,
    Updated,
    Unchanged,
    Removed,
    /// Nothing to do (file or entry not present).
    Absent,
    /// Target not applicable (e.g. Claude Desktop not installed).
    Skipped,
    /// An `anvil` entry belonging to another installation was left alone.
    Kept,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpFileReport {
    pub label: &'static str,
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub action: Action,
    pub backup: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SkillDirReport {
    pub dir: PathBuf,
    pub layout: Option<SkillLayout>,
    pub skipped: bool,
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    pub removed: Vec<String>,
    /// Same-name skills authored by the user (no anvil marker): never touched.
    pub user_owned: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentReport {
    pub id: AgentId,
    pub name: &'static str,
    pub detected: bool,
    pub ok: bool,
    pub mcp: Vec<McpFileReport>,
    pub skills: Vec<SkillDirReport>,
    pub errors: Vec<String>,
}

impl AgentReport {
    fn new(ctx: &AgentContext, id: AgentId) -> Self {
        Self {
            id,
            name: id.display_name(),
            detected: ctx.is_detected(id),
            ok: true,
            mcp: Vec::new(),
            skills: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn finish(mut self) -> Self {
        self.errors.extend(self.mcp.iter().filter_map(|m| m.error.clone()));
        self.errors.extend(self.skills.iter().filter_map(|s| s.error.clone()));
        self.ok = self.errors.is_empty();
        self
    }

    /// Whether anything on disk changed.
    pub fn changed(&self) -> bool {
        self.mcp
            .iter()
            .any(|m| matches!(m.action, Action::Created | Action::Updated | Action::Removed))
            || self
                .skills
                .iter()
                .any(|s| !s.written.is_empty() || !s.removed.is_empty())
    }

    /// At least one MCP config now registers anvil.
    pub fn mcp_configured(&self) -> bool {
        self.mcp
            .iter()
            .any(|m| matches!(m.action, Action::Created | Action::Updated | Action::Unchanged))
    }

    pub fn skills_written(&self) -> usize {
        self.skills.iter().map(|s| s.written.len() + s.unchanged.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetupOptions {
    /// Also configure optional targets whose parent directory does not exist (`--all`).
    pub force_all_targets: bool,
    pub skip_skills: bool,
    pub skip_mcp: bool,
}

/// Configure one agent: register the MCP server and write the skills. Every
/// target is processed independently; failures are collected in the report.
pub fn setup_agent(ctx: &AgentContext, id: AgentId, opts: SetupOptions) -> AgentReport {
    let spec = spec(&ctx.paths, id);
    let mut report = AgentReport::new(ctx, id);
    if !opts.skip_mcp {
        match ctx.entry() {
            Ok(base) => {
                for target in &spec.mcp {
                    let entry = if target.stdio_type {
                        base.clone().with_stdio_type()
                    } else {
                        base.clone()
                    };
                    report.mcp.push(if mcp_applies(target, opts.force_all_targets) {
                        upsert_mcp(ctx, id, target, &entry)
                    } else {
                        mcp_report(target, Action::Skipped)
                    });
                }
            }
            Err(e) => report.errors.push(e.to_string()),
        }
    }
    if !opts.skip_skills {
        for target in &spec.skills {
            report.skills.push(if skills_apply(target, opts.force_all_targets) {
                write_skills(target, embedded())
            } else {
                SkillDirReport {
                    dir: target.dir.clone(),
                    layout: Some(target.layout),
                    skipped: true,
                    ..SkillDirReport::default()
                }
            });
        }
    }
    report.finish()
}

fn mcp_report(target: &McpTarget, action: Action) -> McpFileReport {
    McpFileReport {
        label: target.label,
        path: target.path.clone(),
        format: target.format,
        action,
        backup: None,
        error: None,
    }
}

fn failed(target: &McpTarget, err: AgentError) -> McpFileReport {
    McpFileReport {
        error: Some(err.to_string()),
        ..mcp_report(target, Action::Failed)
    }
}

fn upsert_mcp(ctx: &AgentContext, id: AgentId, target: &McpTarget, entry: &McpEntry) -> McpFileReport {
    let run = || -> Result<McpFileReport> {
        let old = read_optional(&target.path)?;
        let Some(new) = config::upsert(target.format, &target.path, old.as_deref(), entry)? else {
            return Ok(mcp_report(target, Action::Unchanged));
        };
        let backup = match &old {
            Some(_) => Some(backup(&ctx.backups_dir, id.as_str(), &target.path)?),
            None => None,
        };
        write_atomic(&target.path, new.as_bytes())?;
        Ok(McpFileReport {
            backup,
            ..mcp_report(
                target,
                if old.is_some() {
                    Action::Updated
                } else {
                    Action::Created
                },
            )
        })
    };
    run().unwrap_or_else(|e| failed(target, e))
}

fn skill_file(dir: &Path, layout: SkillLayout, name: &str) -> PathBuf {
    match layout {
        SkillLayout::Folder => dir.join(name).join("SKILL.md"),
        SkillLayout::Flat => dir.join(format!("{name}.md")),
    }
}

fn write_skills(target: &SkillTarget, skills: &[Skill]) -> SkillDirReport {
    let mut report = SkillDirReport {
        dir: target.dir.clone(),
        layout: Some(target.layout),
        ..SkillDirReport::default()
    };
    for skill in skills {
        let path = skill_file(&target.dir, target.layout, &skill.name);
        let expected = skill.render(target.layout);
        let result = (|| -> Result<()> {
            match read_optional(&path)? {
                Some(current) if current == expected => report.unchanged.push(skill.name.clone()),
                Some(current) if !is_managed(&current) => report.user_owned.push(skill.name.clone()),
                _ => {
                    write_atomic(&path, expected.as_bytes())?;
                    report.written.push(skill.name.clone());
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            report.error.get_or_insert_with(|| e.to_string());
        }
    }
    report
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    /// Only remove MCP entries whose `command` is this installation's anvil
    /// (used by `anvil uninstall`); skills are then kept for agents whose
    /// entry belongs to another installation.
    pub only_this_install: bool,
}

/// Remove `mcpServers.anvil` / `mcp_servers.anvil` and anvil's own skills.
pub fn remove_agent(ctx: &AgentContext, id: AgentId, opts: RemoveOptions) -> AgentReport {
    let spec = spec(&ctx.paths, id);
    let mut report = AgentReport::new(ctx, id);
    let exe = ctx.anvil_exe().ok();
    for target in &spec.mcp {
        report.mcp.push(remove_mcp(ctx, id, target, opts, exe.as_deref()));
    }
    let foreign = report.mcp.iter().any(|m| m.action == Action::Kept);
    for target in &spec.skills {
        report.skills.push(if foreign {
            SkillDirReport {
                dir: target.dir.clone(),
                layout: Some(target.layout),
                skipped: true,
                ..SkillDirReport::default()
            }
        } else {
            remove_skills(target)
        });
    }
    report.finish()
}

fn remove_mcp(
    ctx: &AgentContext,
    id: AgentId,
    target: &McpTarget,
    opts: RemoveOptions,
    exe: Option<&Path>,
) -> McpFileReport {
    let run = || -> Result<McpFileReport> {
        let Some(old) = read_optional(&target.path)? else {
            return Ok(mcp_report(target, Action::Absent));
        };
        if opts.only_this_install {
            match config::inspect(target.format, &target.path, &old)? {
                None => return Ok(mcp_report(target, Action::Absent)),
                Some(cmd) if !exe.is_some_and(|exe| same_path(&cmd, exe)) => {
                    return Ok(mcp_report(target, Action::Kept));
                }
                Some(_) => {}
            }
        }
        let Some(new) = config::remove(target.format, &target.path, &old)? else {
            return Ok(mcp_report(target, Action::Absent));
        };
        let backup = backup(&ctx.backups_dir, id.as_str(), &target.path)?;
        write_atomic(&target.path, new.as_bytes())?;
        Ok(McpFileReport {
            backup: Some(backup),
            ..mcp_report(target, Action::Removed)
        })
    };
    run().unwrap_or_else(|e| failed(target, e))
}

fn remove_skills(target: &SkillTarget) -> SkillDirReport {
    let mut report = SkillDirReport {
        dir: target.dir.clone(),
        layout: Some(target.layout),
        ..SkillDirReport::default()
    };
    for skill in embedded() {
        let path = skill_file(&target.dir, target.layout, &skill.name);
        let result = (|| -> Result<()> {
            match read_optional(&path)? {
                Some(current) if is_managed(&current) => {
                    fs::remove_file(&path).at(&path)?;
                    if target.layout == SkillLayout::Folder {
                        if let Some(folder) = path.parent() {
                            remove_dir_if_empty(folder);
                        }
                    }
                    report.removed.push(skill.name.clone());
                }
                Some(_) => report.user_owned.push(skill.name.clone()),
                None => {}
            }
            Ok(())
        })();
        if let Err(e) = result {
            report.error.get_or_insert_with(|| e.to_string());
        }
    }
    report
}

// ---------------------------------------------------------------------------
// Detection / status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PathState {
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpStatus {
    pub label: &'static str,
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub exists: bool,
    /// An `anvil` entry is present.
    pub configured: bool,
    pub command: Option<String>,
    /// The entry points to this installation's `bin/anvil`.
    pub points_to_this_install: bool,
    /// The configured command exists on disk.
    pub command_exists: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillsStatus {
    pub dir: PathBuf,
    pub layout: SkillLayout,
    pub applicable: bool,
    /// Managed skills whose content is up to date.
    pub synced: usize,
    pub total: usize,
    /// Managed skills present but out of date.
    pub outdated: Vec<String>,
    pub missing: Vec<String>,
    pub user_owned: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    pub id: AgentId,
    pub name: &'static str,
    pub detected: bool,
    pub detection: Vec<PathState>,
    /// Every applicable MCP config registers this installation.
    pub configured: bool,
    pub mcp: Vec<McpStatus>,
    pub skills: Vec<SkillsStatus>,
}

impl AgentStatus {
    pub fn skills_synced(&self) -> usize {
        self.skills.iter().filter(|s| s.applicable).map(|s| s.synced).sum()
    }

    /// Problems worth a warning (broken or foreign MCP entries, unreadable files).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for m in &self.mcp {
            if let Some(e) = &m.error {
                out.push(format!("{}: {e}", self.id));
            } else if m.configured && !m.command_exists {
                out.push(format!(
                    "{}: MCP entry in {} points to a missing executable '{}' (run `anvil agents setup`)",
                    self.id,
                    m.path.display(),
                    m.command.as_deref().unwrap_or("")
                ));
            } else if m.configured && !m.points_to_this_install {
                out.push(format!(
                    "{}: MCP entry in {} points to another anvil installation ('{}')",
                    self.id,
                    m.path.display(),
                    m.command.as_deref().unwrap_or("")
                ));
            }
        }
        out
    }
}

pub fn detect_agent(ctx: &AgentContext, id: AgentId) -> AgentStatus {
    let spec = spec(&ctx.paths, id);
    let exe = ctx.anvil_exe().ok();
    let mcp: Vec<McpStatus> = spec.mcp.iter().map(|t| mcp_status(t, exe.as_deref())).collect();
    let applicable: Vec<&McpStatus> = spec
        .mcp
        .iter()
        .zip(&mcp)
        .filter(|(t, s)| mcp_applies(t, false) || s.exists)
        .map(|(_, s)| s)
        .collect();
    let configured = !applicable.is_empty() && applicable.iter().all(|s| s.points_to_this_install);
    AgentStatus {
        id,
        name: id.display_name(),
        detected: ctx.is_detected(id),
        detection: spec
            .detect
            .iter()
            .map(|p| PathState {
                path: p.clone(),
                exists: p.exists(),
            })
            .collect(),
        configured,
        mcp,
        skills: spec.skills.iter().map(skills_status).collect(),
    }
}

pub fn detect_all(ctx: &AgentContext) -> Vec<AgentStatus> {
    AgentId::ALL.into_iter().map(|id| detect_agent(ctx, id)).collect()
}

fn mcp_status(target: &McpTarget, exe: Option<&Path>) -> McpStatus {
    let mut status = McpStatus {
        label: target.label,
        path: target.path.clone(),
        format: target.format,
        exists: target.path.is_file(),
        configured: false,
        command: None,
        points_to_this_install: false,
        command_exists: false,
        error: None,
    };
    let inspected = read_optional(&target.path).and_then(|text| match text {
        Some(text) => config::inspect(target.format, &target.path, &text),
        None => Ok(None),
    });
    match inspected {
        Ok(Some(cmd)) => {
            status.configured = true;
            status.command_exists = !cmd.is_empty() && Path::new(&cmd).is_file();
            status.points_to_this_install = exe.is_some_and(|exe| same_path(&cmd, exe));
            status.command = Some(cmd);
        }
        Ok(None) => {}
        Err(e) => status.error = Some(e.to_string()),
    }
    status
}

fn skills_status(target: &SkillTarget) -> SkillsStatus {
    let mut status = SkillsStatus {
        dir: target.dir.clone(),
        layout: target.layout,
        applicable: skills_apply(target, false),
        synced: 0,
        total: embedded().len(),
        outdated: Vec::new(),
        missing: Vec::new(),
        user_owned: Vec::new(),
    };
    for skill in embedded() {
        let path = skill_file(&target.dir, target.layout, &skill.name);
        match read_optional(&path).ok().flatten() {
            Some(c) if c == skill.render(target.layout) => status.synced += 1,
            Some(c) if is_managed(&c) => status.outdated.push(skill.name.clone()),
            Some(_) => status.user_owned.push(skill.name.clone()),
            None => status.missing.push(skill.name.clone()),
        }
    }
    status
}

/// Write the canonical skills to `ANVIL_HOME/skills/<name>.md`. Returns the written names.
pub fn install_canonical_skills(layout: &InstallLayout) -> Result<Vec<String>> {
    let dir = layout.skills_dir();
    let mut written = Vec::new();
    for skill in embedded() {
        let path = dir.join(format!("{}.md", skill.name));
        let text = skill.canonical();
        if read_optional(&path)?.as_deref() != Some(text.as_str()) {
            write_atomic(&path, text.as_bytes())?;
        }
        written.push(skill.name.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_parse() {
        assert_eq!(
            parse_agent_list(&["claude, codex", "claude", "", "SiliconFlower"]).unwrap(),
            vec![AgentId::Claude, AgentId::Codex, AgentId::SiliconFlower]
        );
        assert!(matches!(
            parse_agent_list(&["cursor"]),
            Err(AgentError::UnknownAgent(_))
        ));
        assert_eq!(
            AgentId::ALL.map(|a| a.to_string()),
            ["siliconflower", "claude", "codex", "antigravity"]
        );
    }

    #[test]
    fn selection_from_flags() {
        assert_eq!(Selection::from_flags(vec![], false), Selection::Detected);
        assert_eq!(Selection::from_flags(vec![], true), Selection::All);
        assert_eq!(
            Selection::from_flags(vec![AgentId::Codex], true),
            Selection::Only(vec![AgentId::Codex])
        );
    }

    #[test]
    fn path_comparison() {
        assert!(same_path(
            r"C:/Users/X/anvil/bin/anvil.exe",
            Path::new(r"c:\users\x\anvil\bin\anvil.exe")
        ));
        assert!(same_path("/a/b/", Path::new("/a/b")));
        assert!(!same_path("/a/b", Path::new("/a/c")));
    }
}
