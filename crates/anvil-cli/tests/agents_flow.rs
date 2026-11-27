//! End-to-end agent integration through the real `anvil` binary:
//! install (with pre-existing configs of all four agents) → MCP + skills →
//! agents detect → doctor → skills list/read → re-install (idempotent) →
//! uninstall (surgical cleanup). Everything lives in a sandbox (`ANVIL_ENV_SANDBOX`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use anvil_core::InstallLayout;
use common::*;
use serde_json::{json, Value};

const CODEX_TOML: &str = "# personal codex config (keep me)\nmodel = \"o3\"\n\n[mcp_servers.search]\ncommand = \"search-mcp\" # comment\nargs = [\"--fast\"]\n";

struct Agents {
    sf_config: PathBuf,
    sf_user_skill: PathBuf,
    claude_cli: PathBuf,
    claude_desktop: PathBuf,
    claude_user_skill: PathBuf,
    codex_config: PathBuf,
    antigravity_config: PathBuf,
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Pre-existing user configuration of the four agents inside the sandbox.
fn seed_agents(sandbox: &Path) -> Agents {
    let home = sandbox.join("home");
    let desktop_dir = if cfg!(windows) {
        home.join("AppData").join("Roaming").join("Claude")
    } else {
        home.join(".config").join("Claude")
    };
    let a = Agents {
        sf_config: home.join(".siliconflower").join("config.json"),
        sf_user_skill: home.join(".siliconflower").join("skills").join("team-rules.md"),
        claude_cli: home.join(".claude.json"),
        claude_desktop: desktop_dir.join("claude_desktop_config.json"),
        claude_user_skill: home.join(".claude").join("skills").join("mine").join("SKILL.md"),
        codex_config: home.join(".codex").join("config.toml"),
        antigravity_config: home.join(".gemini").join("antigravity").join("mcp_config.json"),
    };
    write(
        &a.sf_config,
        r#"{"apiKey":"sk-123","baseURL":"https://x.invalid","hooks":{"a":1},"mcpServers":{"fs":{"command":"fs"}}}"#,
    );
    write(&a.sf_user_skill, "# Team rules\n");
    write(
        &a.claude_cli,
        r#"{"userID":"u1","mcpServers":{"gh":{"type":"stdio","command":"gh"}}}"#,
    );
    write(&a.claude_desktop, r#"{"theme":"dark"}"#);
    write(&a.claude_user_skill, "---\nname: mine\ndescription: mine\n---\n");
    write(&a.codex_config, CODEX_TOML);
    write(&a.antigravity_config, r#"{"mcpServers":{}}"#);
    a
}

fn backups(dir: &Path) -> usize {
    fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn install_configures_agents_and_uninstall_cleans_up_surgically() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = fake_usb(tmp.path());
    let home = tmp.path().join("anvil home");
    let sandbox = tmp.path().join("sandbox");
    let layout = InstallLayout::new(&home);
    let agents = seed_agents(&sandbox);

    // --- install --------------------------------------------------------------
    let out = run(anvil(&sandbox)
        .arg("install")
        .arg("--from")
        .arg(&usb.root)
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    let reports = doc["agents"]["agents"].as_array().unwrap();
    assert_eq!(doc["agents"]["selection"], "detected");
    assert_eq!(reports.len(), 4, "{doc:#}");
    for r in reports {
        assert_eq!(r["ok"], true, "{r:#}");
    }
    assert_eq!(
        doc["configured_agents"],
        json!(["antigravity", "claude", "codex", "siliconflower"])
    );
    assert_eq!(doc["skills"]["installed"].as_array().unwrap().len(), 5);
    for name in [
        "anvil-toolkit",
        "rust-systems",
        "c-modern",
        "cpp-modern",
        "cmake-ninja-debug",
    ] {
        let canonical = fs::read_to_string(layout.skills_dir().join(format!("{name}.md"))).unwrap();
        assert!(
            canonical.starts_with(&format!("---\nname: {name}\ntitle: ")),
            "{canonical}"
        );
    }

    // Absolute path of the installed multicall binary, which must exist.
    let exe = layout.anvil_exe();
    assert!(exe.is_absolute() && exe.is_file());
    let exe = exe.display().to_string();

    let sf = read_json(&agents.sf_config);
    assert_eq!(sf["apiKey"], "sk-123");
    assert_eq!(sf["baseURL"], "https://x.invalid");
    assert_eq!(sf["hooks"], json!({"a": 1}));
    assert_eq!(sf["mcpServers"]["fs"]["command"], "fs");
    assert_eq!(sf["mcpServers"]["anvil"], json!({"command": exe, "args": ["mcp"]}));
    let cli = read_json(&agents.claude_cli);
    assert_eq!(cli["userID"], "u1");
    assert_eq!(
        cli["mcpServers"]["anvil"],
        json!({"type": "stdio", "command": exe, "args": ["mcp"]})
    );
    let desktop = read_json(&agents.claude_desktop);
    assert_eq!(desktop["theme"], "dark");
    assert_eq!(desktop["mcpServers"]["anvil"]["command"], exe.as_str());
    let codex = fs::read_to_string(&agents.codex_config).unwrap();
    assert!(codex.contains("# personal codex config (keep me)") && codex.contains("# comment"));
    let doc_toml: toml_edit::DocumentMut = codex.parse().unwrap();
    assert_eq!(doc_toml["mcp_servers"]["anvil"]["command"].as_str(), Some(exe.as_str()));
    assert_eq!(
        read_json(&agents.antigravity_config)["mcpServers"]["anvil"]["command"],
        exe.as_str()
    );

    let shome = sandbox.join("home");
    assert!(shome.join(".siliconflower/skills/rust-systems.md").is_file());
    assert!(!shome.join(".siliconflower/skills/rust-systems").exists());
    assert!(shome.join(".claude/skills/rust-systems/SKILL.md").is_file());
    assert!(shome.join(".codex/skills/rust-systems/SKILL.md").is_file());
    assert!(!shome.join(".agents").exists(), "~/.agents is never created");
    assert!(shome.join(".gemini/antigravity/skills/rust-systems/SKILL.md").is_file());
    assert_eq!(backups(&layout.backups_dir()), 5, "one backup per modified file");

    // --- agents detect --------------------------------------------------------
    let out = run(anvil(&sandbox)
        .args(["agents", "detect", "--json", "--dest"])
        .arg(&home));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(
        doc["detected"],
        json!(["siliconflower", "claude", "codex", "antigravity"])
    );
    assert_eq!(doc["sandbox"], true);
    for status in doc["agents"].as_array().unwrap() {
        assert_eq!(status["configured"], true, "{status:#}");
        let synced: u64 = status["skills"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["applicable"] == true)
            .map(|s| s["synced"].as_u64().unwrap())
            .sum();
        assert_eq!(synced, 5, "{status:#}");
        for m in status["mcp"].as_array().unwrap() {
            assert_eq!(m["points_to_this_install"], true, "{m:#}");
            assert_eq!(m["command_exists"], true, "{m:#}");
        }
    }
    assert_eq!(doc["warnings"], json!([]));

    // --- doctor ---------------------------------------------------------------
    let out = run(anvil(&sandbox).args(["doctor", "--json", "--dest"]).arg(&home));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    assert_eq!(
        doc["agents"]["configured"],
        json!(["siliconflower", "claude", "codex", "antigravity"])
    );
    assert_eq!(doc["agents"]["recorded"].as_array().unwrap().len(), 4);
    let agent_warnings: Vec<&Value> = doc["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w.as_str().unwrap().contains("MCP"))
        .collect();
    assert!(agent_warnings.is_empty(), "{agent_warnings:?}");

    // --- skills list / read ---------------------------------------------------
    let out = run(anvil(&sandbox).args(["skills", "list", "--json"]));
    let doc = json(&out);
    let names: Vec<&str> = doc["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "anvil-toolkit",
            "rust-systems",
            "c-modern",
            "cpp-modern",
            "cmake-ninja-debug"
        ]
    );
    let out = run(anvil(&sandbox).args(["skills", "read", "cpp-modern", "--json"]));
    let doc = json(&out);
    assert_eq!(doc["source"], "embedded");
    assert!(doc["content"].as_str().unwrap().contains("std::span"));
    let out = run(anvil(&sandbox).args(["skills", "read", "anvil-toolkit"]));
    assert!(out.status.success());
    assert!(
        stdout(&out).starts_with("---\nname: anvil-toolkit\n"),
        "{}",
        stdout(&out)
    );
    let out = run(anvil(&sandbox).args(["skills", "read", "nope", "--json"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(json(&out)["error"].as_str().unwrap().contains("unknown skill"));
    let out = run(anvil(&sandbox).args(["skills", "read", "../../etc/passwd", "--json"]));
    assert_eq!(out.status.code(), Some(1));

    // A non-embedded skill dropped into ANVIL_HOME/skills is listed and readable.
    write(
        &layout.skills_dir().join("team-extra.md"),
        "---\nname: team-extra\ntitle: Team Extra\ndescription: extra\n---\n\nbody\n",
    );
    let doc = json(&run(anvil(&sandbox)
        .args(["skills", "list", "--json", "--dest"])
        .arg(&home)));
    assert_eq!(doc["skills"].as_array().unwrap().len(), 6);
    let doc = json(&run(anvil(&sandbox)
        .args(["skills", "read", "team-extra", "--json", "--dest"])
        .arg(&home)));
    assert_eq!(doc["source"], "anvil-home");

    // --- print-config -----------------------------------------------------------
    let out = run(anvil(&sandbox)
        .args(["agents", "print-config", "--format", "toml", "--dest"])
        .arg(&home));
    let snippet = stdout(&out);
    let parsed: toml_edit::DocumentMut = snippet.parse().unwrap();
    assert_eq!(parsed["mcp_servers"]["anvil"]["command"].as_str(), Some(exe.as_str()));
    let out = run(anvil(&sandbox).args(["agents", "print-config", "--dest"]).arg(&home));
    let snippet: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(snippet["mcpServers"]["anvil"]["args"], json!(["mcp"]));

    // --- idempotent re-install ---------------------------------------------------
    let before = fs::read_to_string(&agents.codex_config).unwrap();
    let out = run(anvil(&sandbox)
        .arg("install")
        .arg("--from")
        .arg(&usb.root)
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json"]));
    let doc = json(&out);
    for r in doc["agents"]["agents"].as_array().unwrap() {
        for m in r["mcp"].as_array().unwrap() {
            assert_eq!(m["action"], "unchanged", "{r:#}");
        }
        for s in r["skills"].as_array().unwrap() {
            assert_eq!(s["written"], json!([]), "{r:#}");
        }
    }
    assert_eq!(fs::read_to_string(&agents.codex_config).unwrap(), before);
    assert_eq!(backups(&layout.backups_dir()), 5, "no backup without a change");

    // --- uninstall ----------------------------------------------------------------
    let out = run(anvil(&sandbox)
        .arg("uninstall")
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    assert!(!home.exists());
    for r in doc["agents"]["agents"].as_array().unwrap() {
        assert_eq!(r["ok"], true, "{r:#}");
    }
    let backups_dir = PathBuf::from(doc["agents"]["backups_dir"].as_str().unwrap());
    assert!(backups_dir.starts_with(&sandbox), "sandbox backups stay in the sandbox");
    assert_eq!(backups(&backups_dir), 5);

    let sf = read_json(&agents.sf_config);
    assert_eq!(
        sf,
        json!({"apiKey":"sk-123","baseURL":"https://x.invalid","hooks":{"a":1},"mcpServers":{"fs":{"command":"fs"}}})
    );
    assert_eq!(
        read_json(&agents.claude_cli),
        json!({"userID":"u1","mcpServers":{"gh":{"type":"stdio","command":"gh"}}})
    );
    assert_eq!(
        read_json(&agents.claude_desktop),
        json!({"theme":"dark","mcpServers":{}})
    );
    assert_eq!(fs::read_to_string(&agents.codex_config).unwrap(), CODEX_TOML);
    assert_eq!(read_json(&agents.antigravity_config), json!({"mcpServers":{}}));
    assert_eq!(fs::read_to_string(&agents.sf_user_skill).unwrap(), "# Team rules\n");
    assert!(agents.claude_user_skill.is_file());
    for dir in [
        ".siliconflower/skills/rust-systems.md",
        ".claude/skills/rust-systems",
        ".codex/skills/rust-systems",
        ".gemini/antigravity/skills/rust-systems",
    ] {
        assert!(!shome.join(dir).exists(), "{dir} should be removed");
    }
}

#[test]
fn agents_setup_without_detected_agents_and_explicit_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = tmp.path().join("sandbox");
    let home = tmp.path().join("home");

    // Nothing detected: useful warning, nothing created.
    let out = run(anvil(&sandbox)
        .args(["agents", "setup", "--yes", "--json", "--dest"])
        .arg(&home));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["agents"], json!([]));
    assert!(doc["warnings"].to_string().contains("--all"), "{doc:#}");
    assert!(doc["configured_agents"].is_null(), "no state.json to update");
    assert!(!sandbox.exists());

    // JSON mode without --yes refuses to modify anything.
    let out = run(anvil(&sandbox)
        .args(["agents", "setup", "--only", "codex", "--json", "--dest"])
        .arg(&home));
    assert_eq!(out.status.code(), Some(1));
    assert!(json(&out)["error"].as_str().unwrap().contains("--yes"));
    assert!(!sandbox.exists());

    // Explicit selection configures an agent that is not installed yet.
    let out = run(anvil(&sandbox)
        .args([
            "agents",
            "setup",
            "--only",
            "codex",
            "--skip-skills",
            "--yes",
            "--json",
            "--dest",
        ])
        .arg(&home));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["agents"][0]["id"], "codex");
    assert_eq!(doc["agents"][0]["mcp"][0]["action"], "created");
    assert_eq!(doc["agents"][0]["skills"], json!([]));
    assert!(doc["warnings"].to_string().contains("does not exist yet"), "{doc:#}");
    assert!(sandbox.join("home/.codex/config.toml").is_file());
    assert!(!sandbox.join("home/.codex/skills").exists());

    // skills sync writes skills only.
    let out = run(anvil(&sandbox)
        .args(["skills", "sync", "--only", "codex", "--json", "--dest"])
        .arg(&home));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    assert_eq!(doc["agents"][0]["mcp"], json!([]));
    assert!(sandbox.join("home/.codex/skills/c-modern/SKILL.md").is_file());

    // Unknown agent names are rejected.
    let out = run(anvil(&sandbox).args(["agents", "remove", "--only", "cursor", "--yes", "--json"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(json(&out)["error"].as_str().unwrap().contains("unknown agent"));

    // Remove everything again.
    let out = run(anvil(&sandbox)
        .args(["agents", "remove", "--yes", "--json", "--dest"])
        .arg(&home));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    let toml = fs::read_to_string(sandbox.join("home/.codex/config.toml")).unwrap();
    assert!(!toml.contains("anvil"), "{toml}");
    assert!(!sandbox.join("home/.codex/skills/c-modern").exists());
}

#[test]
fn corrupt_agent_config_does_not_break_install() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = fake_usb(tmp.path());
    let home = tmp.path().join("home");
    let sandbox = tmp.path().join("sandbox");
    let config = sandbox.join("home/.gemini/antigravity/mcp_config.json");
    write(&config, "{ this is not json");

    let out = run(anvil(&sandbox)
        .arg("install")
        .arg("--from")
        .arg(&usb.root)
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json", "--skip-env"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["agents"]["agents"][0]["ok"], false);
    assert!(doc["warnings"].to_string().contains("left untouched"), "{doc:#}");
    assert_eq!(doc["configured_agents"], json!([]));
    assert_eq!(fs::read_to_string(&config).unwrap(), "{ this is not json");

    // --skip-agents leaves agents alone entirely.
    let out = run(anvil(&sandbox)
        .arg("install")
        .arg("--from")
        .arg(&usb.root)
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json", "--skip-env", "--skip-agents"]));
    let doc = json(&out);
    assert!(doc["agents"].is_null());

    // Doctor reports the broken file as a warning, not an error.
    let doc = json(&run(anvil(&sandbox).args(["doctor", "--json", "--dest"]).arg(&home)));
    assert!(doc["warnings"].to_string().contains("antigravity"), "{doc:#}");
    assert!(!doc["errors"].to_string().contains("antigravity"), "{doc:#}");

    // Uninstall with --keep-agents does not touch agent files.
    let out = run(anvil(&sandbox)
        .args(["uninstall", "--yes", "--json", "--keep-agents", "--keep-env", "--dest"])
        .arg(&home));
    let doc = json(&out);
    assert!(doc["agents"].is_null());
    assert_eq!(fs::read_to_string(&config).unwrap(), "{ this is not json");
}
