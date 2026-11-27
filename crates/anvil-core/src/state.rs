//! `state.json`: what is installed in an `ANVIL_HOME`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, IoContext, Result};
use crate::SCHEMA_VERSION;

pub const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallState {
    pub schema_version: u32,
    pub anvil_version: String,
    pub install_dir: PathBuf,
    /// RFC 3339 timestamp (UTC) of the last install operation.
    pub installed_at: String,
    #[serde(default)]
    pub bundles: Vec<InstalledBundle>,
    #[serde(default)]
    pub shims: Vec<String>,
    #[serde(default)]
    pub env_configured: bool,
    #[serde(default)]
    pub configured_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledBundle {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub target_subdir: String,
}

/// Current UTC time formatted as RFC 3339 (`2026-01-02T03:04:05Z`).
pub fn now_rfc3339() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

impl InstallState {
    pub fn new(install_dir: impl Into<PathBuf>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            anvil_version: crate::ANVIL_VERSION.to_string(),
            install_dir: install_dir.into(),
            installed_at: now_rfc3339(),
            bundles: Vec::new(),
            shims: Vec::new(),
            env_configured: false,
            configured_agents: Vec::new(),
        }
    }

    /// Load a state file; `Ok(None)` if it does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).at(path),
        };
        let state: InstallState = serde_json::from_str(&text).map_err(|source| CoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        if state.schema_version != SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchema {
                what: "state",
                found: state.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(Some(state))
    }

    /// Atomically write the state file (write to a sibling temp file, fsync, rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).at(parent)?;
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).expect("state serialization cannot fail");
        {
            let mut f = std::fs::File::create(&tmp).at(&tmp)?;
            f.write_all(json.as_bytes()).at(&tmp)?;
            f.write_all(b"\n").at(&tmp)?;
            f.sync_all().at(&tmp)?;
        }
        std::fs::rename(&tmp, path).at(path)
    }

    /// Insert or replace a bundle record (matched by id) and refresh `installed_at`.
    pub fn upsert_bundle(&mut self, bundle: InstalledBundle) {
        match self.bundles.iter_mut().find(|b| b.id == bundle.id) {
            Some(existing) => *existing = bundle,
            None => self.bundles.push(bundle),
        }
        self.touch();
    }

    pub fn bundle(&self, id: &str) -> Option<&InstalledBundle> {
        self.bundles.iter().find(|b| b.id == id)
    }

    /// Refresh `installed_at` and `anvil_version`.
    pub fn touch(&mut self) {
        self.installed_at = now_rfc3339();
        self.anvil_version = crate::ANVIL_VERSION.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        assert!(InstallState::load(&path).unwrap().is_none());

        let mut state = InstallState::new(dir.path());
        state.upsert_bundle(InstalledBundle {
            id: "llvm".into(),
            version: "1".into(),
            sha256: "a".repeat(64),
            target_subdir: "toolchains/llvm".into(),
        });
        state.upsert_bundle(InstalledBundle {
            id: "llvm".into(),
            version: "2".into(),
            sha256: "b".repeat(64),
            target_subdir: "toolchains/llvm".into(),
        });
        state.shims = vec!["clang".into()];
        state.save(&path).unwrap();

        let loaded = InstallState::load(&path).unwrap().unwrap();
        assert_eq!(loaded, state);
        assert_eq!(loaded.bundles.len(), 1);
        assert_eq!(loaded.bundle("llvm").unwrap().version, "2");
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn timestamp_is_rfc3339() {
        let ts = now_rfc3339();
        assert!(humantime::parse_rfc3339(&ts).is_ok(), "{ts}");
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn rejects_future_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = InstallState::new(dir.path());
        state.schema_version = 99;
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
        assert!(matches!(
            InstallState::load(&path),
            Err(CoreError::UnsupportedSchema { found: 99, .. })
        ));
    }
}
