//! Multicall dispatch: when the `anvil` binary is invoked under a tool name
//! (`clang`, `cargo`, `cmake`, ...) it locates the real binary inside
//! `ANVIL_HOME/toolchains/*/bin` and runs it in a self-contained environment
//! that does not depend on `.bashrc` or any user configuration.

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
/// Returns `None` when the process was invoked as `anvil` (normal CLI), or
/// `Some(result)` with the exit code of the real tool otherwise. On Unix a
/// successful dispatch never returns (the process image is replaced).
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
/// On Unix shims are symlinks and `current_exe()` resolves them, so argv[0]
/// is the only source of truth.
fn invoked_tool(argv0: &OsStr) -> Option<&'static ManagedTool> {
    #[cfg(windows)]
    {
        if let Some(name) = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(OsStr::to_os_string))
        {
            return tool_from_argv0(&name);
        }
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
    ToolEnv::for_layout(layout).apply(&mut cmd);
    Ok(cmd)
}

#[cfg(unix)]
fn exec(mut cmd: Command) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let program = PathBuf::from(cmd.get_program());
    let source = cmd.exec(); // only returns on failure
    Err(EnvError::Spawn { program, source })
}

#[cfg(windows)]
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
    /// * `PATH` = `toolchains/*/bin` + inherited `PATH`, `ANVIL_HOME`, `CARGO_HOME`;
    /// * `CC` / `CXX` when Clang is installed, `CMAKE_GENERATOR=Ninja`;
    /// * `RUSTC` = `toolchains/rust/bin/rustc` (a system `rustup` proxy must never
    ///   hijack the isolated `cargo`) and `RUST_SRC_PATH` =
    ///   `toolchains/rust/lib/rustlib/src/rust/library` (lets `rust-analyzer`
    ///   resolve `std`/`core` offline, without `rustup`);
    /// * Windows only: `CARGO_TARGET_X86_64_PC_WINDOWS_{GNULLVM,GNU}_LINKER` =
    ///   `toolchains/llvm/bin/clang.exe`, so Cargo links with llvm-mingw/lld
    ///   instead of looking for Visual Studio's `link.exe`.
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
        if rustc.is_file() {
            vars.push(("RUSTC".to_string(), rustc.into_os_string()));
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

        if cfg!(windows) {
            let clang = layout.toolchain_dir("llvm").join("bin").join(exe_name("clang"));
            if clang.is_file() {
                for var in WINDOWS_LINKER_VARS {
                    vars.push((var.to_string(), clang.clone().into_os_string()));
                }
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
}

/// Cargo linker overrides pointing at llvm-mingw's `clang.exe` (Windows only).
pub const WINDOWS_LINKER_VARS: [&str; 2] = [
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER",
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
];

fn exe_name(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

/// `dirs` joined with the platform separator, followed by `base` (verbatim).
pub fn prepend_path(dirs: &[PathBuf], base: Option<OsString>) -> OsString {
    let sep = if cfg!(windows) { ";" } else { ":" };
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
        let sep = if cfg!(windows) { ";" } else { ":" };
        assert_eq!(
            prepend_path(&dirs, Some("rest".into())),
            OsString::from(format!("a{sep}b{sep}rest"))
        );
        assert_eq!(prepend_path(&dirs, None), OsString::from(format!("a{sep}b")));
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
            let expected = cfg!(windows).then_some(clang.as_os_str());
            assert_eq!(env.get(var), expected, "{var}");
        }
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
