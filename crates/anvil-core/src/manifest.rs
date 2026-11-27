//! `manifest.json`: describes the bundles available on the source (USB) drive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, IoContext, Result};
use crate::hash::is_sha256_hex;
use crate::paths::{is_safe_component, is_safe_relative_path};
use crate::platform::Platform;
use crate::SCHEMA_VERSION;

pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub anvil_version: String,
    #[serde(default)]
    pub bundles: Vec<BundleSpec>,
}

/// One compressed toolchain bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSpec {
    /// Logical id, e.g. `llvm`, `rust`, `build-tools`.
    pub id: String,
    pub platform: Platform,
    /// Archive path relative to the source root, e.g. `bundles/windows-x86_64/llvm-mingw.tar.zst`.
    pub archive: String,
    /// Lower-case hex SHA-256 of the archive file.
    pub sha256: String,
    /// Size of the archive file in bytes.
    pub size_bytes: u64,
    /// Destination relative to the install dir, e.g. `toolchains/llvm`.
    pub target_subdir: String,
    pub version: String,
    /// Tools provided by the bundle (used to create shims).
    #[serde(default)]
    pub tools: Vec<String>,
}

impl BundleSpec {
    /// Absolute path of the archive on the source drive.
    pub fn archive_path(&self, source_root: &Path) -> PathBuf {
        let mut path = source_root.to_path_buf();
        for part in self.archive.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".") {
            path.push(part);
        }
        path
    }
}

impl Manifest {
    pub fn new(bundles: Vec<BundleSpec>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            anvil_version: crate::ANVIL_VERSION.to_string(),
            bundles,
        }
    }

    /// Read, parse and validate a manifest file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).at(path)?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(|source| CoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest serialization cannot fail")
    }

    /// Structural validation: schema, unique ids per platform, hashes and safe paths.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchema {
                what: "manifest",
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        let bad = |msg: String| Err(CoreError::InvalidManifest(msg));
        let mut seen = HashSet::new();
        for b in &self.bundles {
            if !is_safe_component(&b.id) {
                return bad(format!("invalid bundle id '{}'", b.id));
            }
            if !seen.insert((b.id.as_str(), b.platform)) {
                return bad(format!("duplicate bundle '{}' for {}", b.id, b.platform));
            }
            if !is_sha256_hex(&b.sha256) {
                return bad(format!("bundle '{}': sha256 must be 64 hex characters", b.id));
            }
            if !is_safe_relative_path(&b.archive) {
                return bad(format!("bundle '{}': unsafe archive path '{}'", b.id, b.archive));
            }
            if !is_safe_relative_path(&b.target_subdir) {
                return bad(format!("bundle '{}': unsafe target_subdir '{}'", b.id, b.target_subdir));
            }
            if let Some(tool) = b.tools.iter().find(|t| !is_safe_component(t)) {
                return bad(format!("bundle '{}': invalid tool name '{tool}'", b.id));
            }
        }
        Ok(())
    }

    /// All bundles for `platform`.
    pub fn bundles_for(&self, platform: Platform) -> impl Iterator<Item = &BundleSpec> {
        self.bundles.iter().filter(move |b| b.platform == platform)
    }

    /// Bundles for `platform`, restricted to `only` (in manifest order) when non-empty.
    /// Unknown ids in `only` are an error.
    pub fn select(&self, platform: Platform, only: &[String]) -> Result<Vec<&BundleSpec>> {
        if let Some(missing) = only.iter().find(|id| !self.bundles_for(platform).any(|b| &b.id == *id)) {
            return Err(CoreError::UnknownBundle {
                id: missing.clone(),
                platform: platform.to_string(),
            });
        }
        Ok(self
            .bundles_for(platform)
            .filter(|b| only.is_empty() || only.contains(&b.id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(id: &str, platform: Platform) -> BundleSpec {
        BundleSpec {
            id: id.into(),
            platform,
            archive: format!("bundles/{platform}/{id}.tar.zst"),
            sha256: "a".repeat(64),
            size_bytes: 42,
            target_subdir: format!("toolchains/{id}"),
            version: "1.0.0".into(),
            tools: vec!["cmake".into()],
        }
    }

    #[test]
    fn parses_documented_format() {
        let json = r#"{
            "schema_version": 1,
            "anvil_version": "0.1.0",
            "bundles": [{
                "id": "llvm",
                "platform": "windows-x86_64",
                "archive": "bundles/windows-x86_64/llvm-mingw.tar.zst",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size_bytes": 123456,
                "target_subdir": "toolchains/llvm",
                "version": "20.1.0",
                "tools": ["clang", "clang++"]
            }]
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
        assert_eq!(m.bundles[0].platform, Platform::WindowsX86_64);
        assert_eq!(m.bundles[0].tools, vec!["clang", "clang++"]);
        let back: Manifest = serde_json::from_str(&m.to_json_pretty()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn rejects_invalid_manifests() {
        let mut m = Manifest::new(vec![bundle("llvm", Platform::LinuxX86_64)]);
        m.validate().unwrap();

        m.schema_version = 2;
        assert!(matches!(m.validate(), Err(CoreError::UnsupportedSchema { .. })));
        m.schema_version = 1;

        let mut dup = m.clone();
        dup.bundles.push(bundle("llvm", Platform::LinuxX86_64));
        assert!(dup.validate().is_err());

        let mut traversal = m.clone();
        traversal.bundles[0].target_subdir = "../outside".into();
        assert!(traversal.validate().is_err());

        let mut abs = m.clone();
        abs.bundles[0].archive = "/etc/shadow".into();
        assert!(abs.validate().is_err());

        let mut hash = m.clone();
        hash.bundles[0].sha256 = "xyz".into();
        assert!(hash.validate().is_err());
    }

    #[test]
    fn select_filters_by_platform_and_ids() {
        let m = Manifest::new(vec![
            bundle("llvm", Platform::WindowsX86_64),
            bundle("rust", Platform::WindowsX86_64),
            bundle("llvm", Platform::LinuxX86_64),
        ]);
        assert_eq!(m.select(Platform::WindowsX86_64, &[]).unwrap().len(), 2);
        let only = m.select(Platform::WindowsX86_64, &["rust".into()]).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].id, "rust");
        assert!(matches!(
            m.select(Platform::LinuxX86_64, &["rust".into()]),
            Err(CoreError::UnknownBundle { .. })
        ));
    }

    #[test]
    fn archive_path_is_joined_per_component() {
        let b = bundle("llvm", Platform::LinuxX86_64);
        let p = b.archive_path(Path::new("usb"));
        assert_eq!(
            p,
            Path::new("usb")
                .join("bundles")
                .join("linux-x86_64")
                .join("llvm.tar.zst")
        );
    }
}
