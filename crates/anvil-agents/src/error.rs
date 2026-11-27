use std::path::PathBuf;

use anvil_core::CoreError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Core(#[from] CoreError),

    /// An existing configuration file could not be parsed. It is never modified.
    #[error("cannot parse {format} in '{}': {message} (the file was left untouched; fix or move it and retry)", path.display())]
    InvalidConfig {
        path: PathBuf,
        format: &'static str,
        message: String,
    },

    /// The file parses but has an unexpected shape (e.g. `mcpServers` is not an object).
    #[error("unexpected structure in '{}': {message} (the file was left untouched)", path.display())]
    UnexpectedStructure { path: PathBuf, message: String },

    #[error("unknown agent '{0}' (known agents: siliconflower, claude, codex, antigravity)")]
    UnknownAgent(String),

    #[error("unknown skill '{0}' (run `anvil skills list`)")]
    UnknownSkill(String),

    #[error("invalid skill file '{name}': {message}")]
    InvalidSkill { name: String, message: String },

    #[error("cannot determine {0}")]
    Environment(String),
}

pub type Result<T, E = AgentError> = std::result::Result<T, E>;
