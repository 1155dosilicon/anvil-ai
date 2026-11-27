//! Install directory / source root resolution and the on-disk install layout.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{CoreError, IoContext, Result};
use crate::manifest::MANIFEST_FILE;
use crate::state::STATE_FILE;

/// Environment variable that overrides the install directory.
pub const ANVIL_HOME_ENV: &str = "ANVIL_HOME";

/// Default install directory:
/// `ANVIL_HOME` if set, otherwise `%LOCALAPPDATA%\anvil` (Windows) or
/// `$XDG_DATA_HOME/anvil`, falling back to `~/.local/share/anvil` (Linux).
pub fn default_install_dir() -> Result<PathBuf> {
    default_install_dir_with(|key| std::env::var_os(key))
}

/// Testable variant of [`default_install_dir`] with an injectable environment lookup.
pub fn default_install_dir_with(get: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    let non_empty = |key: &str| get(key).filter(|v| !v.is_empty());

    if let Some(home) = non_empty(ANVIL_HOME_ENV) {
        return Ok(PathBuf::from(home));
    }

    if cfg!(windows) {
        if let Some(local) = non_empty("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("anvil"));
        }
        if let Some(profile) = non_empty("USERPROFILE") {
            return Ok(PathBuf::from(profile).join("AppData").join("Local").join("anvil"));
        }
        Err(CoreError::Environment(
            "the install directory (%LOCALAPPDATA% is not set; use --dest or ANVIL_HOME)".into(),
        ))
    } else {
        if let Some(data) = non_empty("XDG_DATA_HOME").filter(|p| Path::new(p).is_absolute()) {
            return Ok(PathBuf::from(data).join("anvil"));
        }
        if let Some(home) = non_empty("HOME") {
            return Ok(PathBuf::from(home).join(".local").join("share").join("anvil"));
        }
        Err(CoreError::Environment(
            "the install directory ($HOME is not set; use --dest or ANVIL_HOME)".into(),
        ))
    }
}

/// `--dest` if given (made absolute), otherwise [`default_install_dir`].
pub fn resolve_install_dir(dest: Option<&Path>) -> Result<PathBuf> {
    match dest {
        Some(dest) => absolutize(dest),
        None => default_install_dir().and_then(|p| absolutize(&p)),
    }
}

/// Resolve the directory of an existing Anvil installation:
/// 1. `--dest` if explicitly provided;
/// 2. `ANVIL_HOME` if set and non-empty;
/// 3. `<current_exe>/../..` if it contains `state.json` (running from `<ANVIL_HOME>/bin/anvil`);
/// 4. the platform default (`%LOCALAPPDATA%\anvil` / `$XDG_DATA_HOME/anvil` / `~/.local/share/anvil`).
pub fn resolve_existing_install_dir(dest: Option<&Path>) -> Result<PathBuf> {
    let exe = std::env::current_exe().ok();
    resolve_existing_install_dir_with(dest, exe.as_deref(), |key| std::env::var_os(key))
}

/// Testable variant of [`resolve_existing_install_dir`].
pub fn resolve_existing_install_dir_with(
    dest: Option<&Path>,
    current_exe: Option<&Path>,
    get: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    if let Some(dest) = dest {
        return absolutize(dest);
    }
    if let Some(home) = get(ANVIL_HOME_ENV).filter(|v| !v.is_empty()) {
        return absolutize(&PathBuf::from(home));
    }
    if let Some(candidate) = current_exe.and_then(|exe| exe.parent()).and_then(Path::parent) {
        if candidate.join(STATE_FILE).is_file() {
            return absolutize(candidate);
        }
    }
    default_install_dir_with(get).and_then(|p| absolutize(&p))
}

/// Directory holding `manifest.json` and `bundles/`:
/// `--from` if given, otherwise the directory of the running executable
/// (or its parent, if the executable lives in a `bin/`-style subfolder of the USB drive).
pub fn detect_source_root(from: Option<&Path>) -> Result<PathBuf> {
    if let Some(from) = from {
        return absolutize(from);
    }
    let exe = std::env::current_exe().at("current executable")?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| CoreError::Environment("the directory of the running executable".into()))?;
    if !exe_dir.join(MANIFEST_FILE).is_file() {
        if let Some(parent) = exe_dir.parent() {
            if parent.join(MANIFEST_FILE).is_file() {
                return Ok(parent.to_path_buf());
            }
        }
    }
    Ok(exe_dir.to_path_buf())
}

/// Make a path absolute without touching the filesystem (no symlink resolution).
pub fn absolutize(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).at(path)
}

/// `true` if `path` is a non-empty relative path made only of normal components
/// (no root, drive prefix, `..`). Both `/` and `\` are treated as separators.
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with(['/', '\\']) {
        return false;
    }
    let mut normal = 0usize;
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return false,
            p if p.contains(':') => return false,
            _ => normal += 1,
        }
    }
    normal > 0
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// `true` if `name` can safely be used as a single path component (bundle ids, tool names).
pub fn is_safe_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

/// Layout of an Anvil installation (`ANVIL_HOME`).
///
/// ```text
/// ANVIL_HOME/
/// ├── bin/            anvil(.exe) + multicall shims
/// ├── toolchains/     extracted bundles (llvm, rust, build-tools, ...)
/// ├── cargo-home/     CARGO_HOME used by the shims
/// ├── skills/         canonical agent skills (`<name>.md`)
/// ├── backups/        copies of agent config files taken before every change
/// └── state.json
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    root: PathBuf,
}

impl InstallLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn toolchains_dir(&self) -> PathBuf {
        self.root.join("toolchains")
    }

    /// `toolchains/<name>`.
    pub fn toolchain_dir(&self, name: &str) -> PathBuf {
        self.toolchains_dir().join(name)
    }

    /// Directory a bundle with the given `target_subdir` is extracted to.
    pub fn bundle_dir(&self, target_subdir: &str) -> PathBuf {
        let mut dir = self.root.clone();
        for part in target_subdir.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".") {
            dir.push(part);
        }
        dir
    }

    pub fn cargo_home(&self) -> PathBuf {
        self.root.join("cargo-home")
    }

    /// `skills/` – canonical copies of the embedded agent skills.
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// `backups/` – agent configuration files saved before anvil modified them.
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn state_file(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }

    /// `bin/anvil(.exe)` – the multicall binary all shims point to.
    pub fn anvil_exe(&self) -> PathBuf {
        self.shim_path("anvil")
    }

    /// `bin/<tool>(.exe)`.
    pub fn shim_path(&self, tool: &str) -> PathBuf {
        self.bin_dir().join(format!("{tool}{}", std::env::consts::EXE_SUFFIX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn anvil_home_overrides_everything() {
        let dir = default_install_dir_with(env(&[
            ("ANVIL_HOME", "/opt/anvil-custom"),
            ("LOCALAPPDATA", "/la"),
            ("HOME", "/home/u"),
        ]))
        .unwrap();
        assert_eq!(dir, PathBuf::from("/opt/anvil-custom"));
    }

    #[test]
    fn empty_anvil_home_is_ignored() {
        let dir =
            default_install_dir_with(env(&[("ANVIL_HOME", ""), ("LOCALAPPDATA", "/la"), ("HOME", "/home/u")])).unwrap();
        if cfg!(windows) {
            assert_eq!(dir, PathBuf::from("/la").join("anvil"));
        } else {
            assert_eq!(dir, PathBuf::from("/home/u/.local/share/anvil"));
        }
    }

    #[test]
    fn platform_default_dir() {
        if cfg!(windows) {
            let dir = default_install_dir_with(env(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")])).unwrap();
            assert_eq!(dir, PathBuf::from(r"C:\Users\u\AppData\Local\anvil"));
            assert!(default_install_dir_with(env(&[])).is_err());
        } else {
            let dir = default_install_dir_with(env(&[("HOME", "/home/u")])).unwrap();
            assert_eq!(dir, PathBuf::from("/home/u/.local/share/anvil"));
            let dir = default_install_dir_with(env(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")])).unwrap();
            assert_eq!(dir, PathBuf::from("/data/anvil"));
            // Relative XDG_DATA_HOME must be ignored per the XDG spec.
            let dir = default_install_dir_with(env(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "rel")])).unwrap();
            assert_eq!(dir, PathBuf::from("/home/u/.local/share/anvil"));
        }
    }

    #[test]
    fn safe_relative_paths() {
        assert!(is_safe_relative_path("bundles/windows-x86_64/llvm-mingw.tar.zst"));
        assert!(is_safe_relative_path("toolchains/llvm"));
        assert!(is_safe_relative_path(r"toolchains\llvm"));
        assert!(is_safe_relative_path("./toolchains/llvm"));
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("."));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path(r"\\server\share"));
        assert!(!is_safe_relative_path("C:/Windows"));
        assert!(!is_safe_relative_path("toolchains/../../etc"));
        assert!(!is_safe_relative_path(r"toolchains\..\x"));
    }

    #[test]
    fn safe_components() {
        assert!(is_safe_component("llvm"));
        assert!(is_safe_component("build-tools"));
        assert!(!is_safe_component(""));
        assert!(!is_safe_component(".."));
        assert!(!is_safe_component("a/b"));
        assert!(!is_safe_component(r"a\b"));
    }

    #[test]
    fn layout_paths() {
        let l = InstallLayout::new(PathBuf::from("root"));
        assert_eq!(l.bin_dir(), Path::new("root").join("bin"));
        assert_eq!(l.state_file(), Path::new("root").join("state.json"));
        assert_eq!(
            l.bundle_dir("toolchains/llvm"),
            Path::new("root").join("toolchains").join("llvm")
        );
        assert_eq!(l.toolchain_dir("rust"), l.bundle_dir("toolchains/rust"));
        assert_eq!(
            l.shim_path("cmake"),
            Path::new("root")
                .join("bin")
                .join(format!("cmake{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn existing_install_dir_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = tmp.path().join("custom-anvil");
        let exe = installed
            .join("bin")
            .join(format!("anvil{}", std::env::consts::EXE_SUFFIX));
        let explicit = tmp.path().join("explicit-dest");
        let env_home = tmp.path().join("env-home");
        let local_app = tmp.path().join("local-app");
        let unix_home = tmp.path().join("unix-home");

        let default_env = || {
            env(&[
                ("LOCALAPPDATA", local_app.to_str().unwrap()),
                ("HOME", unix_home.to_str().unwrap()),
            ])
        };

        // Without state.json next to exe/../.., fall back to platform default.
        let dir = resolve_existing_install_dir_with(None, Some(&exe), default_env()).unwrap();
        if cfg!(windows) {
            assert_eq!(dir, absolutize(&local_app.join("anvil")).unwrap());
        } else {
            assert_eq!(dir, absolutize(&unix_home.join(".local/share/anvil")).unwrap());
        }

        // Once state.json exists in exe/../.., exe location wins over platform default.
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(installed.join(STATE_FILE), "{}").unwrap();
        let dir = resolve_existing_install_dir_with(None, Some(&exe), default_env()).unwrap();
        assert_eq!(dir, absolutize(&installed).unwrap());

        // Empty ANVIL_HOME is ignored so exe/../.. still wins.
        let dir = resolve_existing_install_dir_with(
            None,
            Some(&exe),
            env(&[
                ("ANVIL_HOME", ""),
                ("LOCALAPPDATA", local_app.to_str().unwrap()),
                ("HOME", unix_home.to_str().unwrap()),
            ]),
        )
        .unwrap();
        assert_eq!(dir, absolutize(&installed).unwrap());

        // Non-empty ANVIL_HOME overrides exe/../..
        let dir = resolve_existing_install_dir_with(
            None,
            Some(&exe),
            env(&[
                ("ANVIL_HOME", env_home.to_str().unwrap()),
                ("LOCALAPPDATA", local_app.to_str().unwrap()),
                ("HOME", unix_home.to_str().unwrap()),
            ]),
        )
        .unwrap();
        assert_eq!(dir, absolutize(&env_home).unwrap());

        // Explicit --dest overrides both ANVIL_HOME and exe/../..
        let dir = resolve_existing_install_dir_with(
            Some(&explicit),
            Some(&exe),
            env(&[
                ("ANVIL_HOME", env_home.to_str().unwrap()),
                ("LOCALAPPDATA", local_app.to_str().unwrap()),
                ("HOME", unix_home.to_str().unwrap()),
            ]),
        )
        .unwrap();
        assert_eq!(dir, absolutize(&explicit).unwrap());
    }
}
