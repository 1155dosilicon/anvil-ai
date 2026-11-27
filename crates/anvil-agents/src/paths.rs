//! Where agent configuration lives, for the real user or for a sandbox.

use std::path::{Path, PathBuf};

use anvil_core::EnvMode;

use crate::error::{AgentError, Result};

/// User directories relevant to agent configuration.
///
/// In [`EnvMode::Sandbox`] every path is below the sandbox directory and
/// override variables (`SILICONFLOWER_DATA_DIR`, `CODEX_HOME`, `APPDATA`,
/// `XDG_CONFIG_HOME`) are ignored, so a sandbox can never leak into the real
/// user profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPaths {
    /// `$HOME` / `%USERPROFILE%`.
    pub home: PathBuf,
    /// `%APPDATA%` (Windows only).
    pub appdata: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` or `~/.config` (non-Windows).
    pub config_dir: PathBuf,
    /// `$SILICONFLOWER_DATA_DIR` override.
    pub siliconflower_data_dir: Option<PathBuf>,
    /// `$CODEX_HOME` override.
    pub codex_home: Option<PathBuf>,
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

impl AgentPaths {
    pub fn from_mode(mode: &EnvMode) -> Result<Self> {
        match mode {
            EnvMode::Sandbox(root) => Ok(Self::sandbox(root)),
            EnvMode::Real => Self::real(),
        }
    }

    /// Fake home at `<root>/home` (same layout as `anvil-env`'s sandbox).
    pub fn sandbox(root: &Path) -> Self {
        let home = root.join("home");
        Self {
            appdata: cfg!(windows).then(|| home.join("AppData").join("Roaming")),
            config_dir: home.join(".config"),
            siliconflower_data_dir: None,
            codex_home: None,
            home,
        }
    }

    pub fn real() -> Result<Self> {
        let home = if cfg!(windows) {
            env_dir("USERPROFILE").or_else(|| env_dir("HOME"))
        } else {
            env_dir("HOME")
        }
        .ok_or_else(|| AgentError::Environment("the user home directory ($HOME / %USERPROFILE%)".into()))?;
        let appdata = if cfg!(windows) {
            Some(env_dir("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")))
        } else {
            None
        };
        Ok(Self {
            appdata,
            config_dir: env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            siliconflower_data_dir: env_dir("SILICONFLOWER_DATA_DIR"),
            codex_home: env_dir("CODEX_HOME"),
            home,
        })
    }

    pub fn siliconflower_dir(&self) -> PathBuf {
        self.siliconflower_data_dir
            .clone()
            .unwrap_or_else(|| self.home.join(".siliconflower"))
    }

    pub fn claude_cli_config(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    pub fn claude_dir(&self) -> PathBuf {
        self.home.join(".claude")
    }

    /// `%APPDATA%\Claude` (Windows) or `~/.config/Claude` (Linux).
    pub fn claude_desktop_dir(&self) -> PathBuf {
        match &self.appdata {
            Some(appdata) if cfg!(windows) => appdata.join("Claude"),
            _ => self.config_dir.join("Claude"),
        }
    }

    pub fn codex_dir(&self) -> PathBuf {
        self.codex_home.clone().unwrap_or_else(|| self.home.join(".codex"))
    }

    /// `~/.agents` (shared skills directory read by Codex).
    pub fn shared_agents_dir(&self) -> PathBuf {
        self.home.join(".agents")
    }

    pub fn antigravity_dir(&self) -> PathBuf {
        self.home.join(".gemini").join("antigravity")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_paths_stay_inside_the_sandbox() {
        let root = Path::new("/sandbox");
        let p = AgentPaths::from_mode(&EnvMode::Sandbox(root.into())).unwrap();
        for path in [
            p.siliconflower_dir(),
            p.claude_cli_config(),
            p.claude_desktop_dir(),
            p.codex_dir(),
            p.antigravity_dir(),
            p.shared_agents_dir(),
        ] {
            assert!(path.starts_with(root), "{}", path.display());
        }
        assert_eq!(p.siliconflower_dir(), root.join("home").join(".siliconflower"));
    }

    #[test]
    fn overrides() {
        let mut p = AgentPaths::sandbox(Path::new("/s"));
        p.siliconflower_data_dir = Some("/data/sf".into());
        p.codex_home = Some("/data/codex".into());
        assert_eq!(p.siliconflower_dir(), PathBuf::from("/data/sf"));
        assert_eq!(p.codex_dir(), PathBuf::from("/data/codex"));
    }
}
