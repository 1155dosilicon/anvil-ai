//! Adapter tests. Everything runs inside a `TempDir` through
//! `EnvMode::Sandbox`, so the real user configuration is never touched.

use std::fs;
use std::path::{Path, PathBuf};

use anvil_agents::skills::{is_managed, managed_names};
use anvil_agents::{
    detect_agent, embedded_skill, remove_agent, setup_agent, Action, AgentContext, AgentId, RemoveOptions, SetupOptions,
};
use anvil_core::{EnvMode, InstallLayout};
use serde_json::{json, Value};

struct Env {
    _tmp: tempfile::TempDir,
    ctx: AgentContext,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(tmp.path().join("anvil home"));
        let ctx = AgentContext::new(&EnvMode::Sandbox(tmp.path().join("sandbox")), layout).unwrap();
        assert!(ctx.sandbox);
        Self { _tmp: tmp, ctx }
    }

    fn home(&self) -> &Path {
        &self.ctx.paths.home
    }

    fn exe(&self) -> String {
        self.ctx.anvil_exe().unwrap().display().to_string()
    }

    fn install_dir(&self) -> String {
        self.ctx.install_dir().unwrap().display().to_string()
    }

    fn backups(&self) -> Vec<PathBuf> {
        match fs::read_dir(self.ctx.layout.backups_dir()) {
            Ok(entries) => {
                let mut v: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
                v.sort();
                v
            }
            Err(_) => Vec::new(),
        }
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn actions(report: &anvil_agents::AgentReport) -> Vec<Action> {
    report.mcp.iter().map(|m| m.action).collect()
}

const DEFAULT: SetupOptions = SetupOptions {
    force_all_targets: false,
    skip_skills: false,
    skip_mcp: false,
};

#[test]
fn siliconflower_merge_backup_flat_skills_and_clean_removal() {
    let env = Env::new();
    let dir = env.home().join(".siliconflower");
    let config = dir.join("config.json");
    write(
        &config,
        r#"{
  "apiKey": "sk-live-123",
  "baseURL": "https://api.example.invalid/v1",
  "model": "qwen",
  "hooks": { "onStart": ["notify"] },
  "mcpServers": { "fs": { "command": "fs-mcp", "args": ["--root", "/"] } }
}"#,
    );
    let user_skill = dir.join("skills/my-notes.md");
    write(&user_skill, "# My notes\n\nmine\n");
    // A user-authored skill that happens to share a managed name: never overwritten.
    let clash = dir.join("skills/rust-systems.md");
    write(&clash, "# My Rust rules\n");

    assert!(env.ctx.is_detected(AgentId::SiliconFlower));
    let report = setup_agent(&env.ctx, AgentId::SiliconFlower, DEFAULT);
    assert!(report.ok, "{:?}", report.errors);
    assert_eq!(actions(&report), vec![Action::Updated]);
    let backup = report.mcp[0].backup.clone().expect("backup taken");
    assert!(backup.starts_with(env.ctx.layout.backups_dir()));
    assert!(backup
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("siliconflower-config.json-"));
    assert!(fs::read_to_string(&backup).unwrap().contains("sk-live-123"));

    let v = read_json(&config);
    assert_eq!(v["apiKey"], "sk-live-123");
    assert_eq!(v["baseURL"], "https://api.example.invalid/v1");
    assert_eq!(v["model"], "qwen");
    assert_eq!(v["hooks"], json!({"onStart": ["notify"]}));
    assert_eq!(v["mcpServers"]["fs"]["args"], json!(["--root", "/"]));
    assert_eq!(
        v["mcpServers"]["anvil"],
        json!({"command": env.exe(), "args": ["mcp"], "env": {"ANVIL_HOME": env.install_dir()}})
    );
    assert!(Path::new(&env.exe()).is_absolute());

    // Flat skills: `<name>.md`, no frontmatter, starting with `# <Title>`.
    let skills = &report.skills[0];
    assert_eq!(skills.user_owned, vec!["rust-systems"]);
    assert_eq!(skills.written.len(), managed_names().len() - 1);
    let toolkit = fs::read_to_string(dir.join("skills/anvil-toolkit.md")).unwrap();
    let title = &embedded_skill("anvil-toolkit").unwrap().title;
    assert!(toolkit.starts_with(&format!("# {title}\n")), "{toolkit}");
    assert!(!toolkit.contains("\n---\n") && !toolkit.starts_with("---"));
    assert!(!dir.join("skills/anvil-toolkit").exists(), "no folder format here");
    assert_eq!(fs::read_to_string(&clash).unwrap(), "# My Rust rules\n");

    // Idempotent: nothing written, no new backup.
    let before = fs::read_to_string(&config).unwrap();
    let again = setup_agent(&env.ctx, AgentId::SiliconFlower, DEFAULT);
    assert_eq!(actions(&again), vec![Action::Unchanged]);
    assert!(!again.changed());
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
    assert_eq!(env.backups().len(), 1);

    let status = detect_agent(&env.ctx, AgentId::SiliconFlower);
    assert!(status.detected && status.configured);
    assert!(status.mcp[0].points_to_this_install);
    assert!(
        !status.mcp[0].command_exists,
        "anvil is not actually installed in this test"
    );
    assert_eq!(status.skills_synced(), managed_names().len() - 1);
    assert_eq!(status.skills[0].user_owned, vec!["rust-systems"]);

    // Removal: only the anvil key and anvil's own skills.
    let removed = remove_agent(&env.ctx, AgentId::SiliconFlower, RemoveOptions::default());
    assert!(removed.ok, "{:?}", removed.errors);
    assert_eq!(actions(&removed), vec![Action::Removed]);
    let v = read_json(&config);
    assert!(v["mcpServers"].get("anvil").is_none());
    assert_eq!(v["mcpServers"]["fs"]["command"], "fs-mcp");
    assert_eq!(v["apiKey"], "sk-live-123");
    assert_eq!(v["hooks"], json!({"onStart": ["notify"]}));
    assert!(!dir.join("skills/anvil-toolkit.md").exists());
    assert_eq!(fs::read_to_string(&user_skill).unwrap(), "# My notes\n\nmine\n");
    assert_eq!(fs::read_to_string(&clash).unwrap(), "# My Rust rules\n");
    assert_eq!(env.backups().len(), 2, "removal backs up too");

    let twice = remove_agent(&env.ctx, AgentId::SiliconFlower, RemoveOptions::default());
    assert_eq!(actions(&twice), vec![Action::Absent]);
    assert!(!twice.changed());
}

#[test]
fn siliconflower_data_dir_override() {
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = anvil_agents::AgentPaths::sandbox(&tmp.path().join("sandbox"));
    paths.siliconflower_data_dir = Some(tmp.path().join("sf-data"));
    let ctx = AgentContext::with_paths(paths, InstallLayout::new(tmp.path().join("home")));
    fs::create_dir_all(tmp.path().join("sf-data")).unwrap();
    assert!(ctx.is_detected(AgentId::SiliconFlower));
    let report = setup_agent(&ctx, AgentId::SiliconFlower, DEFAULT);
    assert_eq!(actions(&report), vec![Action::Created]);
    assert!(tmp.path().join("sf-data/config.json").is_file());
    assert!(tmp.path().join("sf-data/skills/c-modern.md").is_file());
}

#[test]
fn claude_cli_desktop_and_folder_skills() {
    let env = Env::new();
    let cli = env.home().join(".claude.json");
    write(
        &cli,
        r#"{"numStartups": 42, "projects": {"/w": {"allowedTools": []}}, "mcpServers": {"gh": {"type": "stdio", "command": "gh-mcp"}}}"#,
    );
    let desktop_dir = env.ctx.paths.claude_desktop_dir();
    let desktop = desktop_dir.join("claude_desktop_config.json");
    write(&desktop, r#"{"globalShortcut": "Ctrl+Space"}"#);
    let user_skill = env.home().join(".claude/skills/mine/SKILL.md");
    write(&user_skill, "---\nname: mine\ndescription: x\n---\n");

    let report = setup_agent(&env.ctx, AgentId::Claude, DEFAULT);
    assert!(report.ok, "{:?}", report.errors);
    assert_eq!(actions(&report), vec![Action::Updated, Action::Updated]);

    let v = read_json(&cli);
    assert_eq!(v["numStartups"], 42);
    assert!(v["projects"]["/w"].is_object());
    assert_eq!(v["mcpServers"]["gh"]["command"], "gh-mcp");
    assert_eq!(
        v["mcpServers"]["anvil"],
        json!({"type": "stdio", "command": env.exe(), "args": ["mcp"], "env": {"ANVIL_HOME": env.install_dir()}})
    );
    let d = read_json(&desktop);
    assert_eq!(d["globalShortcut"], "Ctrl+Space");
    assert_eq!(
        d["mcpServers"]["anvil"],
        json!({"command": env.exe(), "args": ["mcp"], "env": {"ANVIL_HOME": env.install_dir()}})
    );

    for name in managed_names() {
        let text = fs::read_to_string(env.home().join(format!(".claude/skills/{name}/SKILL.md"))).unwrap();
        assert!(text.starts_with(&format!("---\nname: {name}\ndescription: ")), "{text}");
        assert!(is_managed(&text));
    }
    assert_eq!(env.backups().len(), 2);

    let again = setup_agent(&env.ctx, AgentId::Claude, DEFAULT);
    assert_eq!(actions(&again), vec![Action::Unchanged, Action::Unchanged]);
    assert!(!again.changed());
    assert_eq!(env.backups().len(), 2);
    assert!(detect_agent(&env.ctx, AgentId::Claude).configured);

    let removed = remove_agent(&env.ctx, AgentId::Claude, RemoveOptions::default());
    assert_eq!(actions(&removed), vec![Action::Removed, Action::Removed]);
    assert!(read_json(&cli)["mcpServers"].get("anvil").is_none());
    assert_eq!(read_json(&cli)["mcpServers"]["gh"]["command"], "gh-mcp");
    assert_eq!(read_json(&desktop)["globalShortcut"], "Ctrl+Space");
    assert!(
        !env.home().join(".claude/skills/rust-systems").exists(),
        "empty folder removed"
    );
    assert!(user_skill.is_file(), "user skill untouched");
}

#[test]
fn claude_desktop_only_when_installed_or_forced() {
    let env = Env::new();
    write(&env.home().join(".claude.json"), "{}");
    let desktop = env.ctx.paths.claude_desktop_dir().join("claude_desktop_config.json");

    let report = setup_agent(&env.ctx, AgentId::Claude, DEFAULT);
    assert_eq!(actions(&report), vec![Action::Updated, Action::Skipped]);
    assert!(!desktop.exists());
    assert!(
        detect_agent(&env.ctx, AgentId::Claude).configured,
        "skipped target is not required"
    );

    let forced = setup_agent(
        &env.ctx,
        AgentId::Claude,
        SetupOptions {
            force_all_targets: true,
            ..DEFAULT
        },
    );
    assert_eq!(actions(&forced), vec![Action::Unchanged, Action::Created]);
    assert!(desktop.is_file());
}

const CODEX_TOML: &str = r#"# My Codex settings — keep this comment
model = "o4-mini"
approval_policy = "on-request" # trailing comment

[mcp_servers.search]
command = "search-mcp"
args = ["--fast"]

[profiles.deep]
model = "o3"
"#;

#[test]
fn codex_toml_edit_preserves_comments_and_shared_agents_skills() {
    let env = Env::new();
    let config = env.home().join(".codex/config.toml");
    write(&config, CODEX_TOML);
    fs::create_dir_all(env.home().join(".agents")).unwrap();

    let report = setup_agent(&env.ctx, AgentId::Codex, DEFAULT);
    assert!(report.ok, "{:?}", report.errors);
    assert_eq!(actions(&report), vec![Action::Updated]);
    let text = fs::read_to_string(&config).unwrap();
    for line in CODEX_TOML.lines() {
        assert!(text.lines().any(|l| l == line), "lost {line:?}:\n{text}");
    }
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc["mcp_servers"]["anvil"]["command"].as_str(),
        Some(env.exe().as_str())
    );
    assert_eq!(doc["mcp_servers"]["anvil"]["args"][0].as_str(), Some("mcp"));
    assert_eq!(
        doc["mcp_servers"]["anvil"]["env"]["ANVIL_HOME"].as_str(),
        Some(env.install_dir().as_str())
    );
    assert_eq!(doc["mcp_servers"]["search"]["command"].as_str(), Some("search-mcp"));

    for root in [".codex/skills", ".agents/skills"] {
        let skill = env.home().join(root).join("cpp-modern/SKILL.md");
        assert!(skill.is_file(), "{}", skill.display());
    }

    let again = setup_agent(&env.ctx, AgentId::Codex, DEFAULT);
    assert_eq!(actions(&again), vec![Action::Unchanged]);
    assert!(!again.changed());
    assert_eq!(fs::read_to_string(&config).unwrap(), text);

    let removed = remove_agent(&env.ctx, AgentId::Codex, RemoveOptions::default());
    assert_eq!(actions(&removed), vec![Action::Removed]);
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        CODEX_TOML,
        "byte-for-byte restore"
    );
    assert!(!env.home().join(".agents/skills/cpp-modern").exists());
    assert!(!env.home().join(".codex/skills/cpp-modern").exists());
}

#[test]
fn codex_without_shared_agents_dir_does_not_create_it() {
    let env = Env::new();
    fs::create_dir_all(env.home().join(".codex")).unwrap();
    let report = setup_agent(&env.ctx, AgentId::Codex, DEFAULT);
    assert_eq!(actions(&report), vec![Action::Created]);
    assert!(report.skills[1].skipped);
    assert!(!env.home().join(".agents").exists());
    let text = fs::read_to_string(env.home().join(".codex/config.toml")).unwrap();
    assert!(text.starts_with("[mcp_servers.anvil]\n"), "{text}");
}

#[test]
fn antigravity_merge_and_removal() {
    let env = Env::new();
    let global = env.home().join(".gemini/config/mcp_config.json");
    let legacy = env.home().join(".gemini/antigravity/mcp_config.json");
    write(
        &legacy,
        r#"{"mcpServers": {"browser": {"command": "b", "env": {"K": "V"}}}}"#,
    );

    let report = setup_agent(&env.ctx, AgentId::Antigravity, DEFAULT);
    assert!(report.ok, "{:?}", report.errors);
    assert_eq!(actions(&report), vec![Action::Created, Action::Updated]);
    let vg = read_json(&global);
    assert_eq!(vg["mcpServers"]["anvil"]["command"], env.exe());
    assert_eq!(vg["mcpServers"]["anvil"]["env"]["ANVIL_HOME"], env.install_dir());
    let vl = read_json(&legacy);
    assert_eq!(vl["mcpServers"]["browser"]["env"]["K"], "V");
    assert_eq!(vl["mcpServers"]["anvil"]["command"], env.exe());
    assert_eq!(vl["mcpServers"]["anvil"]["env"]["ANVIL_HOME"], env.install_dir());
    assert!(env
        .home()
        .join(".gemini/config/skills/cmake-ninja-debug/SKILL.md")
        .is_file());
    assert!(env
        .home()
        .join(".gemini/antigravity/skills/cmake-ninja-debug/SKILL.md")
        .is_file());

    assert!(!setup_agent(&env.ctx, AgentId::Antigravity, DEFAULT).changed());

    // A stale entry (anvil moved) is updated in place and detected beforehand.
    let mut stale = read_json(&legacy);
    stale["mcpServers"]["anvil"]["command"] = json!("/old/place/anvil");
    write(&legacy, &serde_json::to_string(&stale).unwrap());
    let status = detect_agent(&env.ctx, AgentId::Antigravity);
    assert!(!status.configured);
    assert!(
        status.problems().iter().any(|p| p.contains("missing executable")),
        "{:?}",
        status.problems()
    );
    assert_eq!(
        actions(&setup_agent(&env.ctx, AgentId::Antigravity, DEFAULT)),
        vec![Action::Unchanged, Action::Updated]
    );
    assert!(detect_agent(&env.ctx, AgentId::Antigravity).configured);

    let removed = remove_agent(&env.ctx, AgentId::Antigravity, RemoveOptions::default());
    assert_eq!(actions(&removed), vec![Action::Removed, Action::Removed]);
    let vl = read_json(&legacy);
    assert_eq!(
        vl,
        json!({"mcpServers": {"browser": {"command": "b", "env": {"K": "V"}}}})
    );
    assert!(read_json(&global)["mcpServers"].get("anvil").is_none());
    assert!(!env.home().join(".gemini/config/skills/cmake-ninja-debug").exists());
    assert!(!env.home().join(".gemini/antigravity/skills/cmake-ninja-debug").exists());
}

#[test]
fn malformed_configs_are_reported_and_never_modified() {
    let env = Env::new();
    let bad_json = "{ \"apiKey\": \"keep-me\", \"mcpServers\": { ";
    let bad_toml = "model = \"x\"\n[mcp_servers.anvil\ncommand = 1\n";
    let sf = env.home().join(".siliconflower/config.json");
    let codex = env.home().join(".codex/config.toml");
    let claude = env.home().join(".claude.json");
    write(&sf, bad_json);
    write(&codex, bad_toml);
    write(&claude, r#"{"mcpServers": "not an object"}"#);

    for (id, path, original, needle) in [
        (AgentId::SiliconFlower, &sf, bad_json, "JSON"),
        (AgentId::Codex, &codex, bad_toml, "TOML"),
        (
            AgentId::Claude,
            &claude,
            r#"{"mcpServers": "not an object"}"#,
            "mcpServers",
        ),
    ] {
        let report = setup_agent(&env.ctx, id, DEFAULT);
        assert!(!report.ok, "{id}");
        assert_eq!(report.mcp[0].action, Action::Failed);
        let err = report.mcp[0].error.as_deref().unwrap();
        assert!(err.contains(needle) && err.contains("untouched"), "{err}");
        assert_eq!(report.mcp[0].path, path.components().collect::<PathBuf>(), "{err}");
        assert!(err.contains(&*path.file_name().unwrap().to_string_lossy()), "{err}");
        // Skills do not depend on the broken config file and are still written.
        assert!(report
            .skills
            .iter()
            .all(|s| s.error.is_none() && (s.skipped || !s.written.is_empty())));
        assert_eq!(fs::read_to_string(path).unwrap(), original, "{id}: file modified");

        let removal = remove_agent(&env.ctx, id, RemoveOptions::default());
        assert!(!removal.ok);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            original,
            "{id}: file modified by remove"
        );

        let status = detect_agent(&env.ctx, id);
        assert!(status.mcp[0].error.is_some());
        assert!(!status.problems().is_empty());
    }
    assert!(env.backups().is_empty(), "no backup when nothing was written");
}

#[test]
fn uninstall_mode_keeps_entries_of_other_installations() {
    let env = Env::new();
    let config = env.home().join(".gemini/antigravity/mcp_config.json");
    write(
        &config,
        r#"{"mcpServers": {"anvil": {"command": "/somewhere/else/bin/anvil", "args": ["mcp"]}}}"#,
    );
    let user_owned_skills = env.home().join(".gemini/antigravity/skills/c-modern/SKILL.md");
    let managed = embedded_skill("c-modern").unwrap().render_folder();
    write(&user_owned_skills, &managed);

    let opts = RemoveOptions {
        only_this_install: true,
    };
    let report = remove_agent(&env.ctx, AgentId::Antigravity, opts);
    assert_eq!(actions(&report), vec![Action::Absent, Action::Kept]);
    assert!(report.skills[0].skipped, "skills belong to the other installation");
    assert!(user_owned_skills.is_file());
    assert!(read_json(&config)["mcpServers"]["anvil"].is_object());

    // Our own entry is removed in the same mode.
    setup_agent(&env.ctx, AgentId::Antigravity, DEFAULT);
    let report = remove_agent(&env.ctx, AgentId::Antigravity, opts);
    assert_eq!(actions(&report), vec![Action::Removed, Action::Removed]);
    assert!(!user_owned_skills.exists());
}

#[test]
fn nothing_is_detected_in_an_empty_sandbox_and_detection_creates_nothing() {
    let env = Env::new();
    assert!(env.ctx.detected().is_empty());
    for id in AgentId::ALL {
        let s = detect_agent(&env.ctx, id);
        assert!(!s.detected && !s.configured);
        assert_eq!(s.skills_synced(), 0);
    }
    assert!(!env.home().exists(), "detection must be read-only");
}
