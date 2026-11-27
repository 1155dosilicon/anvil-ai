//! Shared helpers for the CLI integration tests.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use anvil_core::{pack_dir_tar_zst, BundleSpec, Manifest, Platform};

pub const EXE: &str = std::env::consts::EXE_SUFFIX;

/// A tiny program standing in for a real tool (cmake, ninja, ...): it echoes
/// its arguments and selected environment variables, answers `--version`,
/// and exits with the code given by `--exit=N`.
const DUMMY_SRC: &str = r#"
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("dummy-tool version 9.9.9");
        return;
    }
    println!("ARGC={}", args.len());
    for a in &args {
        println!("ARG={a}");
    }
    for k in ["ANVIL_HOME", "CMAKE_GENERATOR", "CARGO_HOME", "PATH"] {
        println!("{k}={}", std::env::var(k).unwrap_or_default());
    }
    let code = args
        .iter()
        .find_map(|a| a.strip_prefix("--exit="))
        .map(|c| c.parse::<i32>().unwrap())
        .unwrap_or(0);
    std::process::exit(code);
}
"#;

pub fn anvil_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_anvil"))
}

fn rustc() -> PathBuf {
    if let Some(rustc) = std::env::var_os("RUSTC") {
        return rustc.into();
    }
    // `CARGO` points at the toolchain's cargo; rustc sits right next to it.
    if let Some(cargo) = option_env!("CARGO") {
        let candidate = Path::new(cargo).with_file_name(format!("rustc{EXE}"));
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("rustc")
}

/// Path of the compiled dummy tool (compiled once per test process).
pub fn dummy_tool() -> &'static Path {
    static DUMMY: OnceLock<PathBuf> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("anvil-dummy-tool");
        fs::create_dir_all(&dir).unwrap();
        let hash = &anvil_core::hash::sha256_bytes(DUMMY_SRC.as_bytes())[..12];
        let out = dir.join(format!("dummy-{hash}{EXE}"));
        if out.is_file() {
            return out;
        }
        let src = dir.join(format!("dummy-{hash}-{}.rs", std::process::id()));
        fs::write(&src, DUMMY_SRC).unwrap();
        let staging = dir.join(format!("dummy-{hash}-{}{EXE}", std::process::id()));
        let status = Command::new(rustc())
            .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
            .arg(&staging)
            .arg(&src)
            .status()
            .expect("failed to run rustc to build the dummy tool");
        assert!(status.success(), "rustc failed to build the dummy tool");
        // Rename is atomic, so concurrently running test binaries never see a partial file.
        let _ = fs::rename(&staging, &out);
        let _ = fs::remove_file(&src);
        out
    })
}

/// Copy the dummy tool to `path` (creating parent directories).
pub fn install_dummy(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::copy(dummy_tool(), path).unwrap();
}

/// `anvil` command with an isolated user environment (never the real registry / $HOME).
pub fn anvil(sandbox: &Path) -> Command {
    let mut cmd = Command::new(anvil_bin());
    cmd.env("ANVIL_ENV_SANDBOX", sandbox)
        .env_remove("ANVIL_HOME")
        .stdin(Stdio::null());
    cmd
}

pub fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("failed to spawn process")
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Parse stdout as exactly one JSON document (fails on any stray output).
pub fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a single valid JSON document ({e}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout(out),
            stderr(out)
        )
    })
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Fake USB drive: `<usb>/manifest.json` + `<usb>/bundles/<platform>/build-tools.tar.zst`
/// whose `bin/` contains dummy `cmake` and `ninja`.
pub struct FakeUsb {
    pub root: PathBuf,
    pub manifest: Manifest,
}

pub fn fake_usb(tmp: &Path) -> FakeUsb {
    let platform = Platform::current().expect("tests run on a supported platform");
    let staging = tmp.join("staging/build-tools");
    install_dummy(&staging.join(format!("bin/cmake{EXE}")));
    install_dummy(&staging.join(format!("bin/ninja{EXE}")));
    fs::create_dir_all(staging.join("share/doc")).unwrap();
    fs::write(staging.join("share/doc/README.txt"), "build tools").unwrap();

    let root = tmp.join("usb");
    let rel = format!("bundles/{platform}/build-tools.tar.zst");
    let info = pack_dir_tar_zst(&staging, &root.join(&rel), 3).unwrap();

    let other = match platform {
        Platform::WindowsX86_64 => Platform::LinuxX86_64,
        Platform::LinuxX86_64 => Platform::WindowsX86_64,
    };
    let manifest = Manifest::new(vec![
        BundleSpec {
            id: "build-tools".into(),
            platform,
            archive: rel,
            sha256: info.sha256,
            size_bytes: info.size_bytes,
            target_subdir: "toolchains/build-tools".into(),
            version: "3.31.0".into(),
            tools: vec!["cmake".into(), "ninja".into(), "gdb".into()],
        },
        // Present in the manifest but for the other OS: must be ignored.
        BundleSpec {
            id: "llvm".into(),
            platform: other,
            archive: format!("bundles/{other}/llvm.tar.zst"),
            sha256: "0".repeat(64),
            size_bytes: 1,
            target_subdir: "toolchains/llvm".into(),
            version: "20.1.0".into(),
            tools: vec!["clang".into()],
        },
    ]);
    write_manifest(&root, &manifest);
    FakeUsb { root, manifest }
}

pub fn write_manifest(root: &Path, manifest: &Manifest) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("manifest.json"), manifest.to_json_pretty()).unwrap();
}
