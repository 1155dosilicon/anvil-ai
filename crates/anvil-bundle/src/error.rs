//! Error types for USB bundle building and verification.

use std::path::PathBuf;

use thiserror::Error;

pub type Result<T, E = BundleError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error(transparent)]
    Core(#[from] anvil_core::CoreError),

    #[error("I/O error at '{}': {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },

    #[error("invalid recipe: {0}")]
    InvalidRecipe(String),

    #[error("external command '{command}' failed: {detail}")]
    ExternalCommand { command: String, detail: String },

    #[error("required tool '{0}' not found on PATH")]
    MissingHostTool(String),
}
