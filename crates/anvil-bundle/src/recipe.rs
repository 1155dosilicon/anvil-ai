//! Declarative upstream toolchain recipes for `anvil bundle sync`.
//!
//! Recipes can be overridden at runtime with `--recipe <file.json>` so
//! upstream versions can be updated without recompiling `anvil`.

use std::path::Path;

use anvil_core::Platform;
use serde::{Deserialize, Serialize};

use crate::error::{BundleError, Result};

/// How an upstream archive should be merged into the staging bundle root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpstreamLayout {
    /// Strip single top-level directory (`llvm-mingw-*/bin/...` -> `bin/...`).
    StripTopDir,
    /// Extract directly into `bin/` (e.g., `ninja.zip` containing just `ninja(.exe)`).
    IntoBin,
    /// Official Rust standalone tarball (`<pkg>-<ver>-<triple>/<component>/{bin,lib,...}` -> root).
    RustComponent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamArtifact {
    pub url: String,
    pub filename: String,
    pub layout: UpstreamLayout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRecipe {
    pub id: String,
    pub platform: Platform,
    pub version: String,
    pub target_subdir: String,
    pub archive_name: String,
    pub artifacts: Vec<UpstreamArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeBook {
    pub schema_version: u32,
    pub bundles: Vec<BundleRecipe>,
}

impl RecipeBook {
    /// Load a custom JSON recipe file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| BundleError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let book: Self =
            serde_json::from_str(&text).map_err(|e| BundleError::InvalidRecipe(format!("{}: {e}", path.display())))?;
        if book.schema_version != 1 {
            return Err(BundleError::InvalidRecipe(format!(
                "unsupported recipe schema_version {}",
                book.schema_version
            )));
        }
        Ok(book)
    }

    /// Built-in default upstream recipe for Windows x86_64 and Linux x86_64.
    pub fn builtin() -> Self {
        let rust_ver = "1.85.1";
        let llvm_mingw_tag = "20250305";
        let llvm_ver = "20.1.0";
        let cmake_ver = "3.31.6";
        let ninja_ver = "1.12.1";

        let rust_dist = "https://static.rust-lang.org/dist";
        let rust_win_triple = "x86_64-pc-windows-gnullvm";
        let rust_linux_triple = "x86_64-unknown-linux-gnu";

        let rust_artifacts = |triple: &str| -> Vec<UpstreamArtifact> {
            let comps = [
                format!("rustc-{rust_ver}-{triple}.tar.xz"),
                format!("cargo-{rust_ver}-{triple}.tar.xz"),
                format!("rust-std-{rust_ver}-{triple}.tar.xz"),
                format!("clippy-{rust_ver}-{triple}.tar.xz"),
                format!("rustfmt-{rust_ver}-{triple}.tar.xz"),
                format!("rust-analyzer-{rust_ver}-{triple}.tar.xz"),
                format!("rust-src-{rust_ver}.tar.xz"),
            ];
            comps
                .into_iter()
                .map(|filename| UpstreamArtifact {
                    url: format!("{rust_dist}/{filename}"),
                    filename,
                    layout: UpstreamLayout::RustComponent,
                    sha256: None,
                })
                .collect()
        };

        Self {
            schema_version: 1,
            bundles: vec![
                // ---- Windows x86_64 ----
                BundleRecipe {
                    id: "llvm".into(),
                    platform: Platform::WindowsX86_64,
                    version: format!("llvm-mingw-{llvm_mingw_tag}"),
                    target_subdir: "toolchains/llvm".into(),
                    archive_name: "llvm-mingw.tar.zst".into(),
                    artifacts: vec![UpstreamArtifact {
                        url: format!(
                            "https://github.com/mstorsjo/llvm-mingw/releases/download/{llvm_mingw_tag}/llvm-mingw-{llvm_mingw_tag}-ucrt-x86_64.zip"
                        ),
                        filename: format!("llvm-mingw-{llvm_mingw_tag}-ucrt-x86_64.zip"),
                        layout: UpstreamLayout::StripTopDir,
                        sha256: None,
                    }],
                },
                BundleRecipe {
                    id: "rust".into(),
                    platform: Platform::WindowsX86_64,
                    version: rust_ver.into(),
                    target_subdir: "toolchains/rust".into(),
                    archive_name: "rust.tar.zst".into(),
                    artifacts: rust_artifacts(rust_win_triple),
                },
                BundleRecipe {
                    id: "build-tools".into(),
                    platform: Platform::WindowsX86_64,
                    version: format!("cmake-{cmake_ver}-ninja-{ninja_ver}"),
                    target_subdir: "toolchains/build-tools".into(),
                    archive_name: "build-tools.tar.zst".into(),
                    artifacts: vec![
                        UpstreamArtifact {
                            url: format!(
                                "https://github.com/Kitware/CMake/releases/download/v{cmake_ver}/cmake-{cmake_ver}-windows-x86_64.zip"
                            ),
                            filename: format!("cmake-{cmake_ver}-windows-x86_64.zip"),
                            layout: UpstreamLayout::StripTopDir,
                            sha256: None,
                        },
                        UpstreamArtifact {
                            url: format!(
                                "https://github.com/ninja-build/ninja/releases/download/v{ninja_ver}/ninja-win.zip"
                            ),
                            filename: format!("ninja-{ninja_ver}-win.zip"),
                            layout: UpstreamLayout::IntoBin,
                            sha256: None,
                        },
                    ],
                },
                // ---- Linux x86_64 ----
                BundleRecipe {
                    id: "llvm".into(),
                    platform: Platform::LinuxX86_64,
                    version: llvm_ver.into(),
                    target_subdir: "toolchains/llvm".into(),
                    archive_name: "llvm-clang.tar.zst".into(),
                    artifacts: vec![UpstreamArtifact {
                        url: format!(
                            "https://github.com/llvm/llvm-project/releases/download/llvmorg-{llvm_ver}/LLVM-{llvm_ver}-Linux-X64.tar.xz"
                        ),
                        filename: format!("LLVM-{llvm_ver}-Linux-X64.tar.xz"),
                        layout: UpstreamLayout::StripTopDir,
                        sha256: None,
                    }],
                },
                BundleRecipe {
                    id: "rust".into(),
                    platform: Platform::LinuxX86_64,
                    version: rust_ver.into(),
                    target_subdir: "toolchains/rust".into(),
                    archive_name: "rust.tar.zst".into(),
                    artifacts: rust_artifacts(rust_linux_triple),
                },
                BundleRecipe {
                    id: "build-tools".into(),
                    platform: Platform::LinuxX86_64,
                    version: format!("cmake-{cmake_ver}-ninja-{ninja_ver}"),
                    target_subdir: "toolchains/build-tools".into(),
                    archive_name: "build-tools.tar.zst".into(),
                    artifacts: vec![
                        UpstreamArtifact {
                            url: format!(
                                "https://github.com/Kitware/CMake/releases/download/v{cmake_ver}/cmake-{cmake_ver}-linux-x86_64.tar.gz"
                            ),
                            filename: format!("cmake-{cmake_ver}-linux-x86_64.tar.gz"),
                            layout: UpstreamLayout::StripTopDir,
                            sha256: None,
                        },
                        UpstreamArtifact {
                            url: format!(
                                "https://github.com/ninja-build/ninja/releases/download/v{ninja_ver}/ninja-linux.zip"
                            ),
                            filename: format!("ninja-{ninja_ver}-linux.zip"),
                            layout: UpstreamLayout::IntoBin,
                            sha256: None,
                        },
                    ],
                },
            ],
        }
    }

    pub fn select(&self, targets: &[Platform], only: &[String]) -> Vec<&BundleRecipe> {
        self.bundles
            .iter()
            .filter(|b| targets.is_empty() || targets.contains(&b.platform))
            .filter(|b| only.is_empty() || only.iter().any(|id| id == &b.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_recipe_covers_both_platforms_and_all_bundles() {
        let book = RecipeBook::builtin();
        assert_eq!(book.schema_version, 1);
        assert_eq!(book.select(&[Platform::WindowsX86_64], &[]).len(), 3);
        assert_eq!(book.select(&[Platform::LinuxX86_64], &[]).len(), 3);
        assert_eq!(book.select(&[], &["rust".into()]).len(), 2);
        // Round-trip JSON serialization.
        let json = serde_json::to_string_pretty(&book).unwrap();
        let parsed: RecipeBook = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, book);
    }
}
