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
    /// Bundles skipped by [`Manifest::load`] because their platform is not
    /// supported by this build (e.g. `llvm (linux-x86_64)` on drives written
    /// by Anvil 0.1). Never serialized: saving the manifest drops them.
    #[serde(skip)]
    pub unsupported: Vec<String>,
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
            unsupported: Vec::new(),
        }
    }

    /// Read, parse and validate a manifest file.
    ///
    /// Bundles for platforms this build does not support are skipped and
    /// listed in [`Manifest::unsupported`] instead of failing the whole file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).at(path)?;
        Self::from_json_str(&text).map_err(|e| match e {
            CoreError::Json { source, .. } => CoreError::Json {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    /// Parse and validate manifest JSON (see [`Manifest::load`]).
    pub fn from_json_str(text: &str) -> Result<Self> {
        let json_err = |source| CoreError::Json {
            path: PathBuf::from(MANIFEST_FILE),
            source,
        };
        let mut value: serde_json::Value = serde_json::from_str(text).map_err(json_err)?;
        let unsupported = take_unsupported_bundles(&mut value);
        let mut manifest: Manifest = serde_json::from_value(value).map_err(json_err)?;
        manifest.unsupported = unsupported;
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

/// Remove bundles whose `platform` is a string this build does not know,
/// returning `"<id> (<platform>)"` for each one.
fn take_unsupported_bundles(value: &mut serde_json::Value) -> Vec<String> {
    let mut skipped = Vec::new();
    if let Some(bundles) = value.get_mut("bundles").and_then(serde_json::Value::as_array_mut) {
        bundles.retain(|b| match b.get("platform").and_then(serde_json::Value::as_str) {
            Some(p) if p.parse::<Platform>().is_err() => {
                let id = b.get("id").and_then(serde_json::Value::as_str).unwrap_or("?");
                skipped.push(format!("{id} ({p})"));
                false
            }
            _ => true,
        });
    }
    skipped
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
    fn skips_bundles_for_unsupported_platforms() {
        // Drives written by Anvil 0.1 also list Linux bundles.
        let mut legacy = Manifest::new(vec![bundle("llvm", Platform::WindowsX86_64)]);
        let mut value = serde_json::to_value(&legacy).unwrap();
        let mut linux = value["bundles"][0].clone();
        linux["platform"] = "linux-x86_64".into();
        linux["archive"] = "bundles/linux-x86_64/llvm.tar.zst".into();
        value["bundles"].as_array_mut().unwrap().push(linux);

        let m = Manifest::from_json_str(&value.to_string()).unwrap();
        assert_eq!(m.bundles.len(), 1);
        assert_eq!(m.bundles[0].platform, Platform::WindowsX86_64);
        assert_eq!(m.unsupported, vec!["llvm (linux-x86_64)"]);
        // Saving drops the skipped entries.
        assert!(!m.to_json_pretty().contains("linux"));
        legacy.unsupported = m.unsupported.clone();
        assert_eq!(m, legacy);

        // A non-string platform is still a hard error.
        let mut bad = serde_json::to_value(Manifest::new(vec![bundle("llvm", Platform::WindowsX86_64)])).unwrap();
        bad["bundles"][0]["platform"] = 7.into();
        assert!(Manifest::from_json_str(&bad.to_string()).is_err());
    }

    #[test]
    fn rejects_invalid_manifests() {
        let mut m = Manifest::new(vec![bundle("llvm", Platform::WindowsX86_64)]);
        m.validate().unwrap();

        m.schema_version = 2;
        assert!(matches!(m.validate(), Err(CoreError::UnsupportedSchema { .. })));
        m.schema_version = 1;

        let mut dup = m.clone();
        dup.bundles.push(bundle("llvm", Platform::WindowsX86_64));
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
        ]);
        assert_eq!(m.select(Platform::WindowsX86_64, &[]).unwrap().len(), 2);
        let only = m.select(Platform::WindowsX86_64, &["rust".into()]).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].id, "rust");
        assert!(matches!(
            m.select(Platform::WindowsX86_64, &["build-tools".into()]),
            Err(CoreError::UnknownBundle { .. })
        ));
    }

    #[test]
    fn archive_path_is_joined_per_component() {
        let b = bundle("llvm", Platform::WindowsX86_64);
        let p = b.archive_path(Path::new("usb"));
        assert_eq!(
            p,
            Path::new("usb")
                .join("bundles")
                .join("windows-x86_64")
                .join("llvm.tar.zst")
        );
    }
}
