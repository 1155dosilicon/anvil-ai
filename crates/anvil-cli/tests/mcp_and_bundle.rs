//! End-to-end CLI test covering the full USB lifecycle:
//! `bundle init-usb` -> `bundle pack` -> `bundle verify` -> `install` -> `mcp` over real stdio pipes!

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn anvil_bin() -> &'static str {
    env!("CARGO_BIN_EXE_anvil")
}

#[test]
fn full_usb_build_install_and_live_mcp_stdio_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = tmp.path().join("usb");
    let ssd = tmp.path().join("ssd-anvil");
    let sandbox = tmp.path().join("sandbox");
    let home = sandbox.join("home");
    std::fs::create_dir_all(home.join(".siliconflower")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    // 1. Initialize USB drive root with templates, skills, and the real anvil binary.
    let out = Command::new(anvil_bin())
        .args(["bundle", "init-usb", "--usb"])
        .arg(&usb)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert!(usb.join("AGENTS.md").is_file());
    assert!(usb.join("skills").join("anvil-toolkit.md").is_file());

    // 2. Pack a toolchain directory with a dummy tool binary for the host platform.
    let tc_src = tmp.path().join("fake-build-tools");
    std::fs::create_dir_all(tc_src.join("bin")).unwrap();
    let ninja_name = format!("ninja{}", std::env::consts::EXE_SUFFIX);
    std::fs::write(tc_src.join("bin").join(&ninja_name), b"placeholder-ninja").unwrap();

    let platform = anvil_core::Platform::current().unwrap().as_str();
    let out = Command::new(anvil_bin())
        .args([
            "bundle",
            "pack",
            "--usb",
            &usb.display().to_string(),
            "--platform",
            platform,
            "--id",
            "build-tools",
            "--version",
            "1.12.1",
            "--source",
            &tc_src.display().to_string(),
            "--level",
            "3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["report"]["tools"], json!(["ninja"]));

    // 3. Verify the USB drive checksums & files.
    let out = Command::new(anvil_bin())
        .args(["bundle", "verify", "--usb"])
        .arg(&usb)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["report"]["bundles"][0]["ok"], true);

    // 4. Dry-run upstream sync plan.
    let out = Command::new(anvil_bin())
        .args([
            "bundle",
            "sync",
            "--usb",
            &usb.display().to_string(),
            "--targets",
            "windows-x86_64",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["report"]["items"].as_array().unwrap().len(), 3);

    // 5. Install from the newly built USB into the SSD (sandboxed env & agent configs).
    let out = Command::new(anvil_bin())
        .env("ANVIL_ENV_SANDBOX", &sandbox)
        .args(["install", "--from"])
        .arg(&usb)
        .arg("--dest")
        .arg(&ssd)
        .args(["--yes", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert!(home.join(".siliconflower").join("config.json").is_file());
    assert!(home
        .join(".siliconflower")
        .join("skills")
        .join("rust-systems.md")
        .is_file());
    assert!(home.join(".codex").join("config.toml").is_file());
    assert!(home
        .join(".codex")
        .join("skills")
        .join("rust-systems")
        .join("SKILL.md")
        .is_file());

    // 6. Spawn the INSTALLED `<ssd>/bin/anvil(.exe) mcp` over real stdio pipes!
    let installed_anvil = ssd.join("bin").join(format!("anvil{}", std::env::consts::EXE_SUFFIX));
    assert!(installed_anvil.is_file());

    let mut child = Command::new(&installed_anvil)
        .env("ANVIL_ENV_SANDBOX", &sandbox)
        .args(["mcp", "--dest"])
        .arg(&ssd)
        .arg("--workspace")
        .arg(tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        let reqs = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"skill_list","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"anvil_doctor","arguments":{}}}),
        ];
        for r in reqs {
            writeln!(stdin, "{}", serde_json::to_string(&r).unwrap()).unwrap();
        }
    }
    // Dropping stdin sends EOF; server should exit cleanly with 0.
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "anvil mcp stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each MCP stdout line must be valid JSON"))
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "anvil");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 11);

    let skill_text = lines[2]["result"]["content"][0]["text"].as_str().unwrap();
    let skill_json: Value = serde_json::from_str(skill_text).unwrap();
    assert_eq!(skill_json["count"], 5);

    let doc_text = lines[3]["result"]["content"][0]["text"].as_str().unwrap();
    let doc_json: Value = serde_json::from_str(doc_text).unwrap();
    assert_eq!(doc_json["state"]["status"], "ok");
}

#[test]
fn installed_binary_finds_custom_dest_without_anvil_home() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = tmp.path().join("usb");
    let custom_home = tmp.path().join("custom-anvil-home");
    let sandbox = tmp.path().join("sandbox");
    let home = sandbox.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    // Build a minimal USB bundle and install into `custom_home` via `--dest`.
    let out = Command::new(anvil_bin())
        .args(["bundle", "init-usb", "--usb"])
        .arg(&usb)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let tc_src = tmp.path().join("fake-build-tools");
    std::fs::create_dir_all(tc_src.join("bin")).unwrap();
    std::fs::write(
        tc_src
            .join("bin")
            .join(format!("ninja{}", std::env::consts::EXE_SUFFIX)),
        b"placeholder-ninja",
    )
    .unwrap();

    let platform = anvil_core::Platform::current().unwrap().as_str();
    let out = Command::new(anvil_bin())
        .args([
            "bundle",
            "pack",
            "--usb",
            &usb.display().to_string(),
            "--platform",
            platform,
            "--id",
            "build-tools",
            "--version",
            "1.12.1",
            "--source",
            &tc_src.display().to_string(),
            "--level",
            "3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = Command::new(anvil_bin())
        .env("ANVIL_ENV_SANDBOX", &sandbox)
        .env_remove("ANVIL_HOME")
        .args(["install", "--from"])
        .arg(&usb)
        .arg("--dest")
        .arg(&custom_home)
        .args(["--yes", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Verify agent configs recorded ANVIL_HOME pointing to custom_home.
    let claude_cfg: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        claude_cfg["mcpServers"]["anvil"]["env"]["ANVIL_HOME"],
        custom_home.display().to_string()
    );
    let codex_cfg: toml_edit::DocumentMut = std::fs::read_to_string(home.join(".codex").join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        codex_cfg["mcp_servers"]["anvil"]["env"]["ANVIL_HOME"].as_str(),
        Some(custom_home.display().to_string().as_str())
    );

    let installed_anvil = custom_home
        .join("bin")
        .join(format!("anvil{}", std::env::consts::EXE_SUFFIX));
    assert!(installed_anvil.is_file());

    // 1. `anvil doctor --json` WITHOUT `--dest` and WITHOUT `ANVIL_HOME` must detect `custom_home`.
    let out = Command::new(&installed_anvil)
        .env("ANVIL_ENV_SANDBOX", &sandbox)
        .env_remove("ANVIL_HOME")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid doctor json ({e}): stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(doc["state"]["status"], "ok", "{doc:#}");
    assert_eq!(
        std::fs::canonicalize(doc["install_dir"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&custom_home).unwrap()
    );

    // 2. `anvil mcp` WITHOUT `--dest` and WITHOUT `ANVIL_HOME` must also detect `custom_home`.
    let mut child = Command::new(&installed_anvil)
        .env("ANVIL_ENV_SANDBOX", &sandbox)
        .env_remove("ANVIL_HOME")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"anvil_doctor","arguments":{}}}),
        ] {
            writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "anvil mcp stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let doc_text = lines[1]["result"]["content"][0]["text"].as_str().unwrap();
    let doc_json: Value = serde_json::from_str(doc_text).unwrap();
    assert_eq!(doc_json["state"]["status"], "ok", "{doc_json:#}");
    assert_eq!(
        std::fs::canonicalize(doc_json["install_dir"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&custom_home).unwrap()
    );
}
