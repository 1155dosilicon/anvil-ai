//! Multicall shims in `ANVIL_HOME\bin`.
//!
//! `bin\anvil.exe` is a copy of the running binary; every tool shim is an NTFS
//! hard link `bin\<tool>.exe` ⇔ `bin\anvil.exe` (no extra space), falling back
//! to a plain copy on filesystems without hard links (FAT/exFAT).

use std::fs;
use std::path::{Path, PathBuf};

use anvil_core::error::IoContext;
use anvil_core::InstallLayout;
use serde::Serialize;

use crate::error::Result;
use crate::tools::{find_real_tool, managed_tool, same_file, MANAGED_TOOLS, RETIRED_TOOLS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShimKind {
    HardLink,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreatedShim {
    pub tool: String,
    pub path: PathBuf,
    pub kind: ShimKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedShim {
    pub tool: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ShimReport {
    pub anvil_exe: PathBuf,
    pub created: Vec<CreatedShim>,
    pub skipped: Vec<SkippedShim>,
    /// Stale shims removed because their tool is no longer installed.
    pub removed: Vec<String>,
}

impl ShimReport {
    pub fn tool_names(&self) -> Vec<String> {
        self.created.iter().map(|s| s.tool.clone()).collect()
    }
}

/// Copy `source_exe` to `bin/anvil(.exe)` and create a shim for every tool in
/// `tools` whose real binary exists under `toolchains/`. Managed shims that are
/// not (or no longer) backed by a real binary are removed.
pub fn install_shims(layout: &InstallLayout, source_exe: &Path, tools: &[&str]) -> Result<ShimReport> {
    let bin = layout.bin_dir();
    fs::create_dir_all(&bin).at(&bin)?;
    let anvil = layout.anvil_exe();
    install_binary(source_exe, &anvil)?;

    let mut report = ShimReport {
        anvil_exe: anvil.clone(),
        ..ShimReport::default()
    };
    for &name in tools {
        if report.created.iter().any(|c| c.tool == name) || report.skipped.iter().any(|s| s.tool == name) {
            continue;
        }
        let Some(tool) = managed_tool(name) else {
            report.skipped.push(SkippedShim {
                tool: name.to_string(),
                reason: "not a tool managed by anvil shims".into(),
            });
            continue;
        };
        if find_real_tool(layout, tool).is_none() {
            report.skipped.push(SkippedShim {
                tool: name.to_string(),
                reason: format!(
                    "no binary in toolchains/*/bin (expected in toolchains/{}/bin)",
                    tool.toolchain
                ),
            });
            continue;
        }
        let path = layout.shim_path(tool.name);
        let kind = link_shim(&anvil, &path)?;
        report.created.push(CreatedShim {
            tool: tool.name.to_string(),
            path,
            kind,
        });
    }

    for name in MANAGED_TOOLS
        .iter()
        .map(|t| t.name)
        .chain(RETIRED_TOOLS.iter().copied())
    {
        if report.created.iter().any(|c| c.tool == name) {
            continue;
        }
        let path = layout.shim_path(name);
        if fs::symlink_metadata(&path).is_ok() {
            remove_or_set_aside(&path)?;
            report.removed.push(name.to_string());
        }
    }
    Ok(report)
}

/// Remove every managed shim and `bin/anvil(.exe)`. Returns the removed names.
pub fn remove_shims(layout: &InstallLayout) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for name in MANAGED_TOOLS
        .iter()
        .map(|t| t.name)
        .chain(RETIRED_TOOLS.iter().copied())
        .chain(["anvil"])
    {
        let path = layout.shim_path(name);
        if fs::symlink_metadata(&path).is_ok() {
            fs::remove_file(&path).at(&path)?;
            removed.push(name.to_string());
        }
    }
    Ok(removed)
}

/// Copy the anvil binary into place without ever leaving a half-written file.
fn install_binary(source: &Path, dest: &Path) -> Result<()> {
    if same_file(source, dest) {
        return Ok(()); // re-running the installed copy
    }
    let staging = dest.with_extension("new");
    fs::copy(source, &staging).at(&staging)?;
    if fs::symlink_metadata(dest).is_ok() {
        // Windows cannot overwrite a running executable but can rename it.
        let mut old = dest.with_extension("old");
        if fs::remove_file(&old).is_err() && old.exists() {
            // A previous copy is still running: pick a unique name.
            old = dest.with_extension(format!("old-{}", std::process::id()));
        }
        fs::rename(dest, &old).at(dest)?;
        fs::rename(&staging, dest).at(dest)?;
        let _ = fs::remove_file(&old);
    } else {
        fs::rename(&staging, dest).at(dest)?;
    }
    Ok(())
}

/// `<file name>.old` (or `.old-<pid>`) next to `path`.
fn aside_path(path: &Path, unique: bool) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    if unique {
        name.push(format!(".old-{}", std::process::id()));
    } else {
        name.push(".old");
    }
    path.with_file_name(name)
}

/// Delete `path`; if Windows refuses because the executable is running
/// (`PermissionDenied`), rename it to `<name>.old` / `<name>.old-<pid>` instead
/// (a running image can be renamed, not deleted). Returns the set-aside path.
fn remove_or_set_aside(path: &Path) -> Result<Option<PathBuf>> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut aside = aside_path(path, false);
            if fs::symlink_metadata(&aside).is_ok() && fs::remove_file(&aside).is_err() {
                // An older set-aside copy is itself still running.
                aside = aside_path(path, true);
            }
            fs::rename(path, &aside).at(path)?;
            Ok(Some(aside))
        }
        Err(e) => Err(e).at(path).map_err(Into::into),
    }
}

/// Best-effort cleanup of `<shim>.old` left behind by a previous live upgrade.
fn cleanup_set_aside(path: &Path) {
    let _ = fs::remove_file(aside_path(path, false));
}

fn link_shim(anvil: &Path, shim: &Path) -> Result<ShimKind> {
    cleanup_set_aside(shim);
    remove_or_set_aside(shim)?;

    if fs::hard_link(anvil, shim).is_ok() {
        return Ok(ShimKind::HardLink);
    }
    fs::copy(anvil, shim).at(shim)?;
    Ok(ShimKind::Copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"bin").unwrap();
    }

    #[test]
    fn creates_shims_only_for_installed_tools_and_prunes_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path().join("home"));
        let exe = std::env::consts::EXE_SUFFIX;
        let source = dir.path().join(format!("anvil-src{exe}"));
        fs::write(&source, b"fake anvil binary").unwrap();
        touch(
            &layout
                .toolchain_dir("build-tools")
                .join("bin")
                .join(format!("cmake{exe}")),
        );
        touch(
            &layout
                .toolchain_dir("build-tools")
                .join("bin")
                .join(format!("ninja{exe}")),
        );

        // A stale shim from a previous install whose toolchain is gone, and
        // shims of tools retired since Anvil 0.1.
        touch(&layout.shim_path("clang"));
        touch(&layout.shim_path("lld"));
        touch(&layout.shim_path("gdb"));

        let report = install_shims(&layout, &source, &["cmake", "ninja", "rustc", "not-a-tool", "cmake"]).unwrap();
        assert_eq!(report.tool_names(), vec!["cmake", "ninja"]);
        assert_eq!(
            report.skipped.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>(),
            vec!["rustc", "not-a-tool"]
        );
        assert_eq!(report.removed, vec!["clang", "lld", "gdb"]);
        assert!(!layout.shim_path("clang").exists());
        assert!(!layout.shim_path("lld").exists() && !layout.shim_path("gdb").exists());
        assert_eq!(fs::read(layout.anvil_exe()).unwrap(), b"fake anvil binary");
        for shim in &report.created {
            assert_eq!(fs::read(&shim.path).unwrap(), b"fake anvil binary", "{shim:?}");
            assert_eq!(shim.kind, ShimKind::HardLink);
        }

        // Idempotent re-run with an updated binary.
        fs::write(&source, b"new anvil binary").unwrap();
        let again = install_shims(&layout, &source, &["cmake", "ninja"]).unwrap();
        assert_eq!(again.tool_names(), vec!["cmake", "ninja"]);
        assert_eq!(fs::read(layout.shim_path("cmake")).unwrap(), b"new anvil binary");
        assert!(!layout.anvil_exe().with_extension("new").exists());

        touch(&layout.shim_path("pkg-config"));
        let removed = remove_shims(&layout).unwrap();
        assert_eq!(removed, vec!["cmake", "ninja", "pkg-config", "anvil"]);
        assert_eq!(fs::read_dir(layout.bin_dir()).unwrap().count(), 0);
    }

    #[test]
    fn aside_names() {
        let p = Path::new("bin").join("ld.lld.exe");
        assert_eq!(aside_path(&p, false), Path::new("bin").join("ld.lld.exe.old"));
        assert_eq!(
            aside_path(&p, true),
            Path::new("bin").join(format!("ld.lld.exe.old-{}", std::process::id()))
        );
    }

    /// Live upgrade: the shim being replaced is a running executable.
    #[test]
    fn running_shim_is_set_aside_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let anvil = bin.join("anvil.exe");
        fs::write(&anvil, b"new anvil").unwrap();

        // A copy of cmd.exe stands in for the old shim and keeps running.
        let shim = bin.join("cmake.exe");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        fs::copy(Path::new(&system).join(r"System32\cmd.exe"), &shim).unwrap();
        let mut child = std::process::Command::new(&shim)
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            fs::remove_file(&shim).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "a running image cannot be deleted"
        );

        let kind = link_shim(&anvil, &shim).unwrap();
        assert_eq!(kind, ShimKind::HardLink);
        assert_eq!(fs::read(&shim).unwrap(), b"new anvil");
        assert!(aside_path(&shim, false).is_file(), "old shim renamed to .old");

        // Second upgrade while the first `.old` is still running → `.old-<pid>`.
        fs::remove_file(&shim).unwrap();
        fs::copy(aside_path(&shim, false), &shim).unwrap();
        let mut child2 = std::process::Command::new(&shim)
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        link_shim(&anvil, &shim).unwrap();
        assert!(aside_path(&shim, true).is_file());

        let _ = crate::process::kill_tree(&mut child);
        let _ = crate::process::kill_tree(&mut child2);
        let _ = child.wait();
        let _ = child2.wait();
        // Once nothing runs, the next install cleans up `.old`.
        link_shim(&anvil, &shim).unwrap();
        assert!(!aside_path(&shim, false).exists());
    }
}
