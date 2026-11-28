//! End-to-end: install → state.json → shims → shim execution → doctor --json
//! → env → uninstall, all against a fake USB drive and a sandboxed user environment.

mod common;

use std::fs;
use std::path::Path;

use anvil_core::{InstallLayout, InstallState};
use anvil_env::{user_env_configured, EnvMode};
use common::*;

fn install_cmd(sandbox: &Path, usb: &Path, home: &Path) -> std::process::Command {
    let mut cmd = anvil(sandbox);
    cmd.arg("install")
        .arg("--from")
        .arg(usb)
        .arg("--dest")
        .arg(home)
        .args(["--yes", "--json"]);
    cmd
}

#[test]
fn full_install_doctor_uninstall_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = fake_usb(tmp.path());
    let home = tmp.path().join("anvil home");
    let sandbox = tmp.path().join("sandbox");
    let layout = InstallLayout::new(&home);
    let env_mode = EnvMode::Sandbox(sandbox.clone());

    // --- install ----------------------------------------------------------
    let out = run(&mut install_cmd(&sandbox, &usb.root, &home));
    assert!(out.status.success(), "stderr:\n{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["command"], "install");
    let bundles = doc["bundles"].as_array().unwrap();
    assert_eq!(bundles.len(), 1, "only the bundle for this platform: {doc:#}");
    assert_eq!(bundles[0]["id"], "build-tools");
    assert_eq!(bundles[0]["status"], "installed");
    assert_eq!(bundles[0]["sha256"], usb.manifest.bundles[0].sha256.as_str());
    assert_eq!(doc["shims"]["created"], serde_json::json!(["cmake", "ninja"]));
    assert!(
        doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("'ctest'")),
        "ctest is listed but not shipped: {doc:#}"
    );
    assert_eq!(doc["env_configured"], true);
    assert_eq!(doc["env"]["sandbox"], true);

    // state.json
    let state = InstallState::load(&layout.state_file())
        .unwrap()
        .expect("state.json written");
    assert_eq!(state.schema_version, 1);
    assert!(same_path(&state.install_dir, &home));
    assert!(
        humantime::parse_rfc3339(&state.installed_at).is_ok(),
        "{}",
        state.installed_at
    );
    assert_eq!(state.bundles.len(), 1);
    assert_eq!(state.bundles[0].id, "build-tools");
    assert_eq!(state.bundles[0].version, "3.31.0");
    assert_eq!(state.bundles[0].target_subdir, "toolchains/build-tools");
    assert_eq!(state.shims, vec!["cmake", "ninja"]);
    assert!(state.env_configured);
    assert!(state.configured_agents.is_empty());

    // files and shims
    assert_eq!(
        fs::read_to_string(home.join("toolchains/build-tools/share/doc/README.txt")).unwrap(),
        "build tools"
    );
    assert!(layout.anvil_exe().is_file());
    assert!(layout.shim_path("cmake").is_file());
    assert!(layout.shim_path("ninja").is_file());
    assert!(!layout.shim_path("clang").exists());
    assert!(layout.cargo_home().is_dir());
    let leftovers: Vec<_> = fs::read_dir(layout.toolchains_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec!["build-tools"]);

    // sandboxed user environment
    assert!(user_env_configured(&env_mode, &layout).unwrap());
    let reg = fs::read_to_string(sandbox.join("hkcu-environment.json")).unwrap();
    assert!(reg.contains("ANVIL_HOME"), "{reg}");

    // --- idempotent re-install -------------------------------------------
    let out = run(&mut install_cmd(&sandbox, &usb.root, &home));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["bundles"][0]["status"], "unchanged");
    assert_eq!(doc["env"]["modified"], serde_json::json!([]));

    // --- shim runs the real tool -----------------------------------------
    let out = run(std::process::Command::new(layout.shim_path("cmake"))
        .env_remove("ANVIL_HOME")
        .args(["--exit=7", "-S", "src dir"]));
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ARG=src dir"), "{text}");
    assert!(text.contains("CMAKE_GENERATOR=Ninja"), "{text}");

    // --- doctor --json ----------------------------------------------------
    let out = run(anvil(&sandbox).arg("doctor").arg("--dest").arg(&home).arg("--json"));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    assert!(out.status.success());
    assert_eq!(doc["state"]["status"], "ok");
    assert_eq!(doc["path"]["in_user_env"], true);
    let tools = doc["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    for t in tools {
        assert_eq!(t["status"], "ok", "{t:#}");
        assert_eq!(t["version"], "dummy-tool version 9.9.9");
    }
    assert!(doc["smoke_tests"].as_array().unwrap().is_empty());

    // --- doctor --smoke-test: compilers are not installed → skipped, not failed
    let out = run(anvil(&sandbox)
        .arg("doctor")
        .arg("--dest")
        .arg(&home)
        .args(["--smoke-test", "--json"]));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    let smoke = doc["smoke_tests"].as_array().unwrap();
    let langs: Vec<&str> = smoke.iter().map(|s| s["language"].as_str().unwrap()).collect();
    assert_eq!(langs, vec!["c", "c++", "rust", "rust-gnullvm"]);
    assert!(smoke.iter().all(|s| s["status"] == "skipped"), "{doc:#}");

    // --- env --shell json ---------------------------------------------------
    let out = run(anvil(&sandbox)
        .arg("env")
        .arg("--dest")
        .arg(&home)
        .args(["--shell", "json"]));
    assert!(out.status.success());
    let doc = json(&out);
    assert!(same_path(Path::new(doc["vars"]["ANVIL_HOME"].as_str().unwrap()), &home));
    assert_eq!(doc["vars"]["CMAKE_GENERATOR"], "Ninja");
    assert_eq!(doc["installed"], true);
    assert!(same_path(
        Path::new(doc["path_prepend"][0].as_str().unwrap()),
        &layout.bin_dir()
    ));

    // --- uninstall ------------------------------------------------------------
    let out = run(anvil(&sandbox)
        .arg("uninstall")
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc:#}");
    assert_eq!(doc["removed"], true);
    assert_eq!(doc["leftovers"], serde_json::json!([]));
    assert!(!home.exists(), "install dir must be gone");
    assert!(!user_env_configured(&env_mode, &layout).unwrap(), "PATH entry removed");
    let reg = fs::read_to_string(sandbox.join("hkcu-environment.json")).unwrap();
    assert!(!reg.contains("ANVIL_HOME"), "{reg}");

    // A second uninstall refuses to touch anything and reports JSON.
    let out = run(anvil(&sandbox)
        .arg("uninstall")
        .arg("--dest")
        .arg(&home)
        .args(["--yes", "--json"]));
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert!(doc["error"].as_str().unwrap().contains("no anvil installation"));
}

#[test]
fn tampered_bundle_aborts_and_leaves_no_partial_install() {
    let tmp = tempfile::tempdir().unwrap();
    let mut usb = fake_usb(tmp.path());
    usb.manifest.bundles[0].sha256 = "f".repeat(64);
    write_manifest(&usb.root, &usb.manifest);
    let home = tmp.path().join("home");
    let sandbox = tmp.path().join("sandbox");

    let out = run(&mut install_cmd(&sandbox, &usb.root, &home));
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    let causes = doc["causes"].to_string();
    assert!(causes.contains("SHA-256 mismatch"), "{doc:#}");
    let toolchains = InstallLayout::new(&home).toolchains_dir();
    assert_eq!(
        fs::read_dir(&toolchains).unwrap().count(),
        0,
        "no .tmp-* or partial bundle"
    );
    assert!(!InstallLayout::new(&home).state_file().exists());
    assert!(!sandbox.exists(), "user environment untouched");
}

#[test]
fn skip_env_and_only_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = fake_usb(tmp.path());
    let home = tmp.path().join("home");
    let sandbox = tmp.path().join("sandbox");

    let out = run(install_cmd(&sandbox, &usb.root, &home).args(["--only", "build-tools", "--skip-env"]));
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert!(doc["env"].is_null());
    assert_eq!(doc["env_configured"], false);
    assert!(!sandbox.exists());

    // `llvm` exists only for the other platform.
    let out = run(install_cmd(&sandbox, &usb.root, &home).args(["--only", "llvm"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(json(&out)["error"]
        .as_str()
        .unwrap()
        .contains("'llvm' is not available"));
}

#[test]
fn non_interactive_install_requires_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = fake_usb(tmp.path());
    let home = tmp.path().join("home");
    let out = run(anvil(&tmp.path().join("sandbox"))
        .arg("install")
        .arg("--from")
        .arg(&usb.root)
        .arg("--dest")
        .arg(&home)
        .arg("--json"));
    assert_eq!(out.status.code(), Some(1));
    assert!(json(&out)["error"].as_str().unwrap().contains("--yes"));
    assert!(!home.join("toolchains").exists());
}

#[test]
fn json_mode_never_pollutes_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = tmp.path().join("sandbox");
    let empty = tmp.path().join("nothing-here");

    // Doctor on a missing installation: valid JSON, ok=false, exit code 1.
    let out = run(anvil(&sandbox)
        .arg("doctor")
        .arg("--dest")
        .arg(&empty)
        .args(["--smoke-test", "--json"]));
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["state"]["status"], "missing");

    // Missing manifest → JSON error.
    let out = run(anvil(&sandbox)
        .arg("install")
        .arg("--from")
        .arg(&empty)
        .args(["--yes", "--json"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["ok"], false);

    // Usage errors are JSON too when --json is present.
    let out = run(anvil(&sandbox).args(["install", "--bogus", "--json"]));
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["kind"], "usage");

    // env --shell json on a missing install: warning on stderr, JSON on stdout.
    let out = run(anvil(&sandbox)
        .arg("env")
        .arg("--dest")
        .arg(&empty)
        .args(["--shell", "json"]));
    assert!(out.status.success());
    assert_eq!(json(&out)["installed"], false);
    assert!(stderr(&out).contains("warning"));
}

#[test]
fn help_and_shell_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(anvil(tmp.path()).arg("--help"));
    assert!(out.status.success());
    let help = stdout(&out);
    for cmd in ["install", "doctor", "env", "uninstall"] {
        assert!(help.contains(cmd), "{help}");
    }
    for (shell, needle) in [("powershell", "$env:ANVIL_HOME"), ("json", "\"ANVIL_HOME\"")] {
        let out = run(anvil(tmp.path())
            .arg("env")
            .arg("--dest")
            .arg(tmp.path())
            .args(["--shell", shell]));
        assert!(out.status.success());
        assert!(stdout(&out).contains(needle), "{shell}: {}", stdout(&out));
    }
}
