//! Multicall dispatch: when `anvil.exe` is invoked under a tool name
//! (`clang`, `cargo`, `cmake`, ...) it locates the real binary inside
//! `ANVIL_HOME\toolchains\*\bin` and runs it in a self-contained environment.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anvil_core::{InstallLayout, ANVIL_HOME_ENV};

use crate::error::{EnvError, Result};
use crate::tools::{
    find_real_tool, managed_tool, real_tool_candidates, same_file, tool_from_argv0, toolchain_bin_dirs, ManagedTool,
};

/// Entry point used by `main` **before** argument parsing.
///
/// Returns `None` when the process was invoked as `anvil.exe` (normal CLI), or
/// `Some(result)` with the exit code of the real tool otherwise.
pub fn dispatch_multicall() -> Option<Result<i32>> {
    let mut args = std::env::args_os();
    let argv0 = args.next()?;
    let tool = invoked_tool(&argv0)?;
    Some(run_tool(tool, args.collect()))
}

/// Determine which managed tool the process was started as.
///
/// On Windows shims are hard links/copies, so the executable's own file name
/// is authoritative (argv[0] is whatever the parent process chose to pass).
fn invoked_tool(argv0: &OsStr) -> Option<&'static ManagedTool> {
    if let Some(name) = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(OsStr::to_os_string))
    {
        return tool_from_argv0(&name);
    }
    tool_from_argv0(argv0)
}

/// `ANVIL_HOME` for a running shim: the `ANVIL_HOME` variable if set,
/// otherwise `current_exe()/../..` (`ANVIL_HOME/bin/<tool>` → `ANVIL_HOME`).
pub fn resolve_home_from_exe(env_home: Option<OsString>, current_exe: &Path) -> Result<PathBuf> {
    if let Some(home) = env_home.filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    current_exe
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| EnvError::Environment(format!("ANVIL_HOME from executable path '{}'", current_exe.display())))
}

fn run_tool(tool: &'static ManagedTool, args: Vec<OsString>) -> Result<i32> {
    let exe = std::env::current_exe().map_err(|e| EnvError::Environment(format!("current executable: {e}")))?;
    let home = resolve_home_from_exe(std::env::var_os(ANVIL_HOME_ENV), &exe)?;
    let layout = InstallLayout::new(home);
    let cmd = prepare_command(&layout, tool, &args, Some(&exe))?;
    exec(cmd)
}

/// Build the command for `tool` (real binary + args + isolated environment).
pub fn prepare_command(
    layout: &InstallLayout,
    tool: &ManagedTool,
    args: &[OsString],
    current_exe: Option<&Path>,
) -> Result<Command> {
    let real = find_real_tool(layout, tool).ok_or_else(|| EnvError::ToolNotFound {
        tool: tool.name.to_string(),
        home: layout.root().to_path_buf(),
        searched: real_tool_candidates(layout, tool),
    })?;
    if current_exe.is_some_and(|exe| same_file(exe, &real)) {
        return Err(EnvError::ShimRecursion { path: real });
    }
    let mut cmd = Command::new(&real);
    cmd.args(args);
    let mut env = ToolEnv::for_layout(layout);
    // `llvm-mingw` ships a Clang wrapper named `x86_64-w64-mingw32-gcc.exe`
    // that uses compiler-rt instead of libgcc. If `rustc` sees it on PATH while compiling
    // for `x86_64-pc-windows-gnu` (including host build scripts and proc-macros when
    // targeting `gnullvm`), it disables its own self-contained MinGW linker and fails
    // looking for `-lgcc_eh`. Removing `llvm/bin` from `rustc`'s PATH (unless targeting
    // `gnullvm`) lets `rustc` use its bundled self-contained linker automatically.
    if tool.name == "rustc"
        && has_windows_gnu_self_contained(layout)
        && !args.iter().any(|a| a.to_string_lossy().contains("gnullvm"))
    {
        env.strip_dir_from_path(&layout.toolchain_dir("llvm").join("bin"));
    }
    env.apply(&mut cmd);
    Ok(cmd)
}

fn exec(mut cmd: Command) -> Result<i32> {
    use windows_sys::Win32::Foundation::BOOL;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    // The child shares our console and receives Ctrl+C/Ctrl+Break itself; we
    // only have to survive long enough to forward its exit code. A handler
    // routine (unlike the NULL handler) is not inherited by the child.
    unsafe extern "system" fn ignore_ctrl(_event: u32) -> BOOL {
        1
    }
    // SAFETY: registering a valid `extern "system"` handler with no captured state.
    unsafe {
        SetConsoleCtrlHandler(Some(ignore_ctrl), 1);
    }

    let program = PathBuf::from(cmd.get_program());
    let status = cmd.status().map_err(|source| EnvError::Spawn { program, source })?;
    Ok(status.code().unwrap_or(1))
}

/// Environment injected into every tool launched through a shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEnv {
    pub vars: Vec<(String, OsString)>,
}

impl ToolEnv {
    /// Environment for the real tools of `layout`:
    ///
    /// * `PATH` = `toolchains\*\bin` + inherited `PATH`, `ANVIL_HOME`, `CARGO_HOME`;
    /// * `CC` / `CXX` when Clang is installed, `CMAKE_GENERATOR=Ninja`;
    /// * `RUSTC` = `bin\rustc.exe` shim when installed alongside self-contained MinGW
    ///   (so host `build.rs` / proc-macro builds under `--target x86_64-pc-windows-gnullvm`
    ///   strip `llvm\bin` automatically), falling back to `toolchains\rust\bin\rustc.exe`;
    /// * `RUST_SRC_PATH` = `toolchains\rust\lib\rustlib\src\rust\library` (lets
    ///   `rust-analyzer` resolve `std`/`core` offline without `rustup`);
    /// * `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS=-C link-self-contained=yes`
    ///   and `CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=toolchains\llvm\bin\clang.exe`;
    /// * [`GNULLVM_RUSTFLAGS_VAR`] = [`GNULLVM_RUSTFLAGS`] when the `gnullvm` rust-std
    ///   is installed and the user has not set it (standalone `.exe`, no `libunwind.dll`).
    pub fn for_layout(layout: &InstallLayout) -> Self {
        Self::with_base_path(layout, std::env::var_os("PATH"))
    }

    pub fn with_base_path(layout: &InstallLayout, base_path: Option<OsString>) -> Self {
        let mut vars = Vec::new();
        vars.push(("PATH".to_string(), prepend_path(&toolchain_bin_dirs(layout), base_path)));
        vars.push((ANVIL_HOME_ENV.to_string(), layout.root().as_os_str().to_os_string()));
        vars.push(("CARGO_HOME".to_string(), layout.cargo_home().into_os_string()));
        for (var, name) in [("CC", "clang"), ("CXX", "clang++")] {
            if let Some(path) = managed_tool(name).and_then(|t| find_real_tool(layout, t)) {
                vars.push((var.to_string(), path.into_os_string()));
            }
        }
        vars.push(("CMAKE_GENERATOR".to_string(), OsString::from("Ninja")));

        let rust = layout.toolchain_dir("rust");
        let rustc = rust.join("bin").join(exe_name("rustc"));
        let self_contained_gnu = has_windows_gnu_self_contained(layout);
        if rustc.is_file() {
            let rustc_shim = layout.shim_path("rustc");
            let selected = if self_contained_gnu && rustc_shim.is_file() {
                rustc_shim
            } else {
                rustc
            };
            vars.push(("RUSTC".to_string(), selected.into_os_string()));
        }
        let rust_src = rust
            .join("lib")
            .join("rustlib")
            .join("src")
            .join("rust")
            .join("library");
        if rust_src.is_dir() {
            vars.push(("RUST_SRC_PATH".to_string(), rust_src.into_os_string()));
        }

        // A value set by the user wins (`anvil` never overrides it).
        if has_gnullvm_std(layout) && std::env::var_os(GNULLVM_RUSTFLAGS_VAR).is_none() {
            vars.push((GNULLVM_RUSTFLAGS_VAR.to_string(), OsString::from(GNULLVM_RUSTFLAGS)));
        }

        let clang = layout.toolchain_dir("llvm").join("bin").join(exe_name("clang"));
        if self_contained_gnu {
            vars.push((
                "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS".to_string(),
                OsString::from("-C link-self-contained=yes"),
            ));
            if clang.is_file() {
                vars.push((
                    "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER".to_string(),
                    clang.into_os_string(),
                ));
            }
        } else if clang.is_file() {
            for var in WINDOWS_LINKER_VARS {
                vars.push((var.to_string(), clang.clone().into_os_string()));
            }
        }
        Self { vars }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_os_str())
    }

    pub fn apply(&self, cmd: &mut Command) {
        for (k, v) in &self.vars {
            cmd.env(k, v);
        }
    }

    /// Remove `dir` from the `PATH` entry in this environment (if present).
    pub fn strip_dir_from_path(&mut self, dir: &Path) {
        if let Some((_, val)) = self.vars.iter_mut().find(|(k, _)| k == "PATH") {
            let target = dir.to_string_lossy();
            let kept: Vec<PathBuf> = std::env::split_paths(val.as_os_str())
                .filter(|p| !crate::winpath::entries_equal(&p.to_string_lossy(), &target))
                .collect();
            if let Ok(joined) = std::env::join_paths(kept) {
                *val = joined;
            }
        }
    }
}

/// Whether the isolated Rust toolchain ships `x86_64-pc-windows-gnu` self-contained MinGW libs.
pub fn has_windows_gnu_self_contained(layout: &InstallLayout) -> bool {
    layout
        .toolchain_dir("rust")
        .join("lib")
        .join("rustlib")
        .join("x86_64-pc-windows-gnu")
        .join("lib")
        .join("self-contained")
        .is_dir()
}

/// Optional Rust target linked through llvm-mingw.
pub const GNULLVM_TRIPLE: &str = "x86_64-pc-windows-gnullvm";

/// Cargo flags for [`GNULLVM_TRIPLE`]: link `libunwind` and the C runtime
/// statically so binaries run without `libunwind.dll` on `PATH`.
pub const GNULLVM_RUSTFLAGS_VAR: &str = "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS";
pub const GNULLVM_RUSTFLAGS: &str = "-C target-feature=+crt-static";

/// Whether the isolated Rust toolchain ships `rust-std` for [`GNULLVM_TRIPLE`].
pub fn has_gnullvm_std(layout: &InstallLayout) -> bool {
    layout
        .toolchain_dir("rust")
        .join("lib")
        .join("rustlib")
        .join(GNULLVM_TRIPLE)
        .join("lib")
        .is_dir()
}

/// Cargo linker overrides pointing at llvm-mingw's `clang.exe`.
pub const WINDOWS_LINKER_VARS: [&str; 2] = [
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER",
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
];

fn exe_name(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

/// `dirs` joined with `;`, followed by `base` (verbatim).
pub fn prepend_path(dirs: &[PathBuf], base: Option<OsString>) -> OsString {
    let sep = ";";
    let mut out = OsString::new();
    for dir in dirs {
        if !out.is_empty() {
            out.push(sep);
        }
        out.push(dir.as_os_str());
    }
    if let Some(base) = base.filter(|b| !b.is_empty()) {
        if !out.is_empty() {
            out.push(sep);
        }
        out.push(base);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_resolution() {
        let exe = Path::new("/opt/anvil/bin/cmake");
        assert_eq!(resolve_home_from_exe(None, exe).unwrap(), PathBuf::from("/opt/anvil"));
        assert_eq!(
            resolve_home_from_exe(Some(OsString::new()), exe).unwrap(),
            PathBuf::from("/opt/anvil")
        );
        assert_eq!(
            resolve_home_from_exe(Some("/custom".into()), exe).unwrap(),
            PathBuf::from("/custom")
        );
    }

    #[test]
    fn path_prepending() {
        let dirs = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(prepend_path(&dirs, Some("rest".into())), OsString::from("a;b;rest"));
        assert_eq!(prepend_path(&dirs, None), OsString::from("a;b"));
        assert_eq!(prepend_path(&[], Some("rest".into())), OsString::from("rest"));
    }

    #[test]
    fn tool_env_contents() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let llvm_bin = layout.toolchain_dir("llvm").join("bin");
        std::fs::create_dir_all(&llvm_bin).unwrap();
        let clang = llvm_bin.join(format!("clang{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&clang, b"x").unwrap();

        let env = ToolEnv::with_base_path(&layout, Some("orig".into()));
        let path = env.get("PATH").unwrap().to_string_lossy().into_owned();
        assert!(path.starts_with(&*llvm_bin.to_string_lossy()), "{path}");
        assert!(path.ends_with("orig"));
        assert_eq!(env.get("ANVIL_HOME"), Some(dir.path().as_os_str()));
        assert_eq!(env.get("CARGO_HOME"), Some(layout.cargo_home().as_os_str()));
        assert_eq!(env.get("CC"), Some(clang.as_os_str()));
        assert_eq!(env.get("CXX"), None, "clang++ is not installed");
        assert_eq!(env.get("CMAKE_GENERATOR"), Some(OsStr::new("Ninja")));
        assert_eq!(env.get("RUSTC"), None, "rust toolchain is not installed");
        assert_eq!(env.get("RUST_SRC_PATH"), None);
        for var in WINDOWS_LINKER_VARS {
            assert_eq!(env.get(var), Some(clang.as_os_str()), "{var}");
        }
    }

    #[test]
    fn tool_env_prefers_rustc_shim_when_self_contained_gnu_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let rust = layout.toolchain_dir("rust");
        let rustc = rust.join("bin").join(exe_name("rustc"));
        std::fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        std::fs::write(&rustc, b"x").unwrap();
        std::fs::create_dir_all(rust.join("lib/rustlib/x86_64-pc-windows-gnu/lib/self-contained")).unwrap();

        // Without shim, falls back to real rustc binary.
        let env = ToolEnv::with_base_path(&layout, None);
        assert_eq!(env.get("RUSTC"), Some(rustc.as_os_str()));

        // With bin/rustc.exe shim present, RUSTC routes through the shim so host build scripts
        // strip llvm/bin from PATH even under `--target x86_64-pc-windows-gnullvm`.
        let shim = layout.shim_path("rustc");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, b"shim").unwrap();
        let env = ToolEnv::with_base_path(&layout, None);
        assert_eq!(env.get("RUSTC"), Some(shim.as_os_str()));
    }

    #[test]
    fn tool_env_rust_variables() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let rust = layout.toolchain_dir("rust");
        let rustc = rust.join("bin").join(exe_name("rustc"));
        std::fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        std::fs::write(&rustc, b"x").unwrap();

        // rustc present, rust-src component absent.
        let env = ToolEnv::with_base_path(&layout, None);
        assert_eq!(env.get("RUSTC"), Some(rustc.as_os_str()));
        assert_eq!(env.get("RUST_SRC_PATH"), None);
        for var in WINDOWS_LINKER_VARS {
            assert_eq!(env.get(var), None, "no clang → no linker override for {var}");
        }

        assert_eq!(env.get(GNULLVM_RUSTFLAGS_VAR), None, "no gnullvm rust-std");
        std::fs::create_dir_all(rust.join("lib/rustlib").join(GNULLVM_TRIPLE).join("lib")).unwrap();
        if std::env::var_os(GNULLVM_RUSTFLAGS_VAR).is_none() {
            let env = ToolEnv::with_base_path(&layout, None);
            assert_eq!(env.get(GNULLVM_RUSTFLAGS_VAR), Some(OsStr::new(GNULLVM_RUSTFLAGS)));
        }

        let src = rust.join("lib/rustlib/src/rust/library");
        std::fs::create_dir_all(&src).unwrap();
        let env = ToolEnv::with_base_path(&layout, None);
        assert_eq!(
            env.get("RUST_SRC_PATH").map(Path::new),
            Some(
                rust.join("lib")
                    .join("rustlib")
                    .join("src")
                    .join("rust")
                    .join("library")
                    .as_path()
            )
        );

        // A `rustc` living in another toolchain is not the isolated toolchain's rustc.
        std::fs::remove_file(&rustc).unwrap();
        let elsewhere = layout.toolchain_dir("llvm").join("bin").join(exe_name("rustc"));
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, b"x").unwrap();
        assert_eq!(ToolEnv::with_base_path(&layout, None).get("RUSTC"), None);
    }

    #[test]
    fn tool_env_is_applied_to_commands() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let rustc = layout.toolchain_dir("rust").join("bin").join(exe_name("rustc"));
        std::fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        std::fs::write(&rustc, b"x").unwrap();
        let mut cmd = Command::new("x");
        ToolEnv::with_base_path(&layout, None).apply(&mut cmd);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| *k == "RUSTC" && *v == Some(rustc.as_os_str())));
    }

    #[test]
    fn missing_tool_reports_searched_paths() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let err = prepare_command(&layout, managed_tool("ninja").unwrap(), &[], None).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, EnvError::ToolNotFound { .. }));
        assert!(msg.contains("build-tools"), "{msg}");
    }
}
