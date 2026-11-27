//! Real vs. sandboxed user environment.
//!
//! Every component that touches files or settings *outside* `ANVIL_HOME`
//! (user PATH, shell profiles, AI agent configuration files) takes an
//! [`EnvMode`]. Tests always use [`EnvMode::Sandbox`], so they can never
//! modify the real registry, `$HOME` or agent configs.

use std::path::PathBuf;

/// Setting this variable to a directory makes the CLI use [`EnvMode::Sandbox`].
pub const ENV_SANDBOX_VAR: &str = "ANVIL_ENV_SANDBOX";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvMode {
    /// The real user environment (registry / `$HOME`).
    Real,
    /// Isolated: everything happens below this directory.
    Sandbox(PathBuf),
}

impl EnvMode {
    /// [`EnvMode::Sandbox`] if `ANVIL_ENV_SANDBOX` is set, else [`EnvMode::Real`].
    pub fn from_process_env() -> Self {
        match std::env::var_os(ENV_SANDBOX_VAR).filter(|v| !v.is_empty()) {
            Some(dir) => EnvMode::Sandbox(PathBuf::from(dir)),
            None => EnvMode::Real,
        }
    }

    pub fn is_sandbox(&self) -> bool {
        matches!(self, EnvMode::Sandbox(_))
    }
}
