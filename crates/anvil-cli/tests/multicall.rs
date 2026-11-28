//! Real multicall dispatch: the `anvil` binary, invoked through a shim named
//! after a tool, must run `ANVIL_HOME/toolchains/*/bin/<tool>` with the
//! arguments, an isolated environment and the exit code passed through.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use anvil_core::InstallLayout;
use common::*;

/// `home/bin/{anvil,cmake}` via the real shim engine + dummy real `cmake`.
fn setup(home: &Path) -> InstallLayout {
    let layout = InstallLayout::new(home);
    install_dummy(
        &layout
            .toolchain_dir("build-tools")
            .join("bin")
            .join(format!("cmake{EXE}")),
    );
    let report = anvil_env::install_shims(&layout, &anvil_bin(), &["cmake"]).unwrap();
    assert_eq!(report.tool_names(), vec!["cmake"]);
    layout
}

fn value<'a>(stdout: &'a str, key: &str) -> Option<&'a str> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn shim(layout: &InstallLayout, tool: &str) -> Command {
    let mut cmd = Command::new(layout.shim_path(tool));
    cmd.env_remove("ANVIL_HOME");
    cmd
}

#[test]
fn shim_forwards_args_env_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("anvil home"); // space on purpose
    let layout = setup(&home);

    let out = run(shim(&layout, "cmake")
        .args(["--exit=42", "hello world", "--flag", "-DX=1;2", ""])
        .env("CMAKE_GENERATOR", "Unix Makefiles")); // overridden by anvil
    let text = stdout(&out);
    assert_eq!(
        out.status.code(),
        Some(42),
        "stdout:\n{text}\nstderr:\n{}",
        stderr(&out)
    );

    let args: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("ARG=")).collect();
    assert_eq!(args, vec!["--exit=42", "hello world", "--flag", "-DX=1;2", ""]);
    assert_eq!(value(&text, "ARGC"), Some("5"));

    // ANVIL_HOME derived from current_exe()/../.. since the variable was removed.
    let anvil_home = PathBuf::from(value(&text, "ANVIL_HOME").unwrap());
    assert!(
        same_path(&anvil_home, &home),
        "{} != {}",
        anvil_home.display(),
        home.display()
    );
    assert_eq!(value(&text, "CMAKE_GENERATOR"), Some("Ninja"));
    let cargo_home = PathBuf::from(value(&text, "CARGO_HOME").unwrap());
    assert_eq!(cargo_home.file_name().unwrap(), "cargo-home");
    assert!(same_path(cargo_home.parent().unwrap(), &home));

    // Toolchain bin directory is first on PATH.
    let path = value(&text, "PATH").unwrap();
    let first = std::env::split_paths(path).next().unwrap();
    assert!(
        same_path(&first, &layout.toolchain_dir("build-tools").join("bin")),
        "{path}"
    );
}

#[test]
fn shim_propagates_success_and_other_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = setup(&tmp.path().join("home"));
    for code in [0, 1, 3, 255] {
        let out = run(shim(&layout, "cmake").arg(format!("--exit={code}")));
        assert_eq!(out.status.code(), Some(code), "{}", stderr(&out));
    }
    // `--version` is answered by the real tool, not by anvil's clap parser.
    let out = run(shim(&layout, "cmake").arg("--version"));
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "dummy-tool version 9.9.9");
}

#[test]
fn anvil_home_variable_overrides_exe_location() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = setup(&tmp.path().join("home"));
    let other = tmp.path().join("other-home");
    install_dummy(
        &InstallLayout::new(&other)
            .toolchain_dir("build-tools")
            .join("bin")
            .join(format!("cmake{EXE}")),
    );

    let out = run(shim(&layout, "cmake").env("ANVIL_HOME", &other));
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        same_path(Path::new(value(&text, "ANVIL_HOME").unwrap()), &other),
        "{text}"
    );
}

#[test]
fn missing_real_tool_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = setup(&tmp.path().join("home"));
    // A shim for `ninja` whose real binary is absent.
    let ninja = layout.shim_path("ninja");
    std::fs::copy(layout.anvil_exe(), &ninja).unwrap();

    let out = run(&mut shim(&layout, "ninja"));
    assert_eq!(out.status.code(), Some(127));
    let err = stderr(&out);
    assert!(err.contains("'ninja' is not installed"), "{err}");
    assert!(stdout(&out).is_empty());
}

#[test]
fn anvil_name_runs_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = setup(&tmp.path().join("home"));
    let out = run(Command::new(layout.anvil_exe()).arg("--version"));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("anvil "), "{}", stdout(&out));
}
