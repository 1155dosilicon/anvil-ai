//! Catalogue of tools exposed through multicall shims.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anvil_core::InstallLayout;

/// Toolchain directories under `ANVIL_HOME/toolchains`, in lookup order.
pub const TOOLCHAINS: [&str; 3] = ["llvm", "rust", "build-tools"];

/// A tool that can be invoked through an `ANVIL_HOME/bin/<name>` shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ManagedTool {
    pub name: &'static str,
    /// Toolchain directory (`toolchains/<toolchain>/bin`) expected to provide it.
    pub toolchain: &'static str,
}

const fn tool(name: &'static str, toolchain: &'static str) -> ManagedTool {
    ManagedTool { name, toolchain }
}

pub const MANAGED_TOOLS: &[ManagedTool] = &[
    // LLVM / Clang
    tool("clang", "llvm"),
    tool("clang++", "llvm"),
    tool("clangd", "llvm"),
    tool("clang-tidy", "llvm"),
    tool("clang-format", "llvm"),
    tool("lld", "llvm"),
    tool("ld.lld", "llvm"),
    tool("lldb", "llvm"),
    // Rust
    tool("rustc", "rust"),
    tool("cargo", "rust"),
    tool("rustfmt", "rust"),
    tool("clippy-driver", "rust"),
    tool("cargo-clippy", "rust"),
    tool("rust-analyzer", "rust"),
    // Build tools
    tool("cmake", "build-tools"),
    tool("ninja", "build-tools"),
    tool("pkg-config", "build-tools"),
    tool("pkgconf", "build-tools"),
    tool("gdb", "build-tools"),
];

/// Look up a managed tool by exact name (case-insensitive on Windows).
pub fn managed_tool(name: &str) -> Option<&'static ManagedTool> {
    MANAGED_TOOLS.iter().find(|t| {
        if cfg!(windows) {
            t.name.eq_ignore_ascii_case(name)
        } else {
            t.name == name
        }
    })
}

/// Strip a trailing `.exe` (any case) from a file name.
pub fn strip_exe_suffix(name: &str) -> &str {
    let len = name.len();
    if len > 4 && name.is_char_boundary(len - 4) && name[len - 4..].eq_ignore_ascii_case(".exe") {
        &name[..len - 4]
    } else {
        name
    }
}

/// Map `argv[0]` (any path form, with or without `.exe`) to a managed tool.
///
/// Only `.exe` is stripped, so dotted tool names such as `ld.lld` keep working.
/// Returns `None` for `anvil` itself and for unknown names.
pub fn tool_from_argv0(argv0: &OsStr) -> Option<&'static ManagedTool> {
    let text = argv0.to_str()?;
    // Accept both separators regardless of host so Windows-style argv[0] parses everywhere.
    let base = text.rsplit(['/', '\\']).next().unwrap_or(text);
    let name = strip_exe_suffix(base);
    if name.eq_ignore_ascii_case("anvil") {
        return None;
    }
    managed_tool(name)
}

/// `toolchains/<toolchain>/bin` directories that exist, in [`TOOLCHAINS`] order.
pub fn toolchain_bin_dirs(layout: &InstallLayout) -> Vec<PathBuf> {
    TOOLCHAINS
        .iter()
        .map(|tc| layout.toolchain_dir(tc).join("bin"))
        .filter(|p| p.is_dir())
        .collect()
}

/// Candidate locations of the real binary, preferred toolchain first.
pub fn real_tool_candidates(layout: &InstallLayout, tool: &ManagedTool) -> Vec<PathBuf> {
    let file = format!("{}{}", tool.name, std::env::consts::EXE_SUFFIX);
    std::iter::once(tool.toolchain)
        .chain(TOOLCHAINS.iter().copied().filter(|tc| *tc != tool.toolchain))
        .map(|tc| layout.toolchain_dir(tc).join("bin").join(&file))
        .collect()
}

/// Real binary of `tool` inside the installation, if present.
pub fn find_real_tool(layout: &InstallLayout, tool: &ManagedTool) -> Option<PathBuf> {
    real_tool_candidates(layout, tool).into_iter().find(|p| p.is_file())
}

/// `true` if both paths name the same file (after canonicalisation).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn name(s: &str) -> Option<&'static str> {
        tool_from_argv0(&OsString::from(s)).map(|t| t.name)
    }

    #[test]
    fn argv0_resolution() {
        assert_eq!(name("cmake"), Some("cmake"));
        assert_eq!(name("cmake.exe"), Some("cmake"));
        assert_eq!(name("CMAKE.EXE").is_some(), cfg!(windows));
        assert_eq!(name("/home/u/.local/share/anvil/bin/clang++"), Some("clang++"));
        assert_eq!(name(r"C:\Users\u\AppData\Local\anvil\bin\ld.lld.exe"), Some("ld.lld"));
        assert_eq!(name("ld.lld"), Some("ld.lld"));
        assert_eq!(name("cargo-clippy"), Some("cargo-clippy"));
        assert_eq!(name("anvil"), None);
        assert_eq!(name("anvil.exe"), None);
        assert_eq!(name("/usr/bin/anvil"), None);
        assert_eq!(name("gcc"), None);
        assert_eq!(name(""), None);
    }

    #[test]
    fn catalogue_is_consistent() {
        for t in MANAGED_TOOLS {
            assert!(TOOLCHAINS.contains(&t.toolchain), "{t:?}");
            assert!(anvil_core::paths::is_safe_component(t.name));
        }
        let mut names: Vec<_> = MANAGED_TOOLS.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), MANAGED_TOOLS.len(), "duplicate tool names");
    }

    #[test]
    fn real_tool_lookup_prefers_own_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let layout = InstallLayout::new(dir.path());
        let cmake = managed_tool("cmake").unwrap();
        assert!(find_real_tool(&layout, cmake).is_none());

        let exe = format!("cmake{}", std::env::consts::EXE_SUFFIX);
        let in_llvm = layout.toolchain_dir("llvm").join("bin").join(&exe);
        std::fs::create_dir_all(in_llvm.parent().unwrap()).unwrap();
        std::fs::write(&in_llvm, b"x").unwrap();
        assert_eq!(find_real_tool(&layout, cmake), Some(in_llvm));

        let in_build = layout.toolchain_dir("build-tools").join("bin").join(&exe);
        std::fs::create_dir_all(in_build.parent().unwrap()).unwrap();
        std::fs::write(&in_build, b"x").unwrap();
        assert_eq!(find_real_tool(&layout, cmake), Some(in_build));
        assert_eq!(toolchain_bin_dirs(&layout).len(), 2);
    }
}
