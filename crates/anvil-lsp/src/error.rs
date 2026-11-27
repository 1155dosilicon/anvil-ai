//! Error types for the LSP bridge.

use std::path::PathBuf;

use thiserror::Error;

pub type Result<T, E = LspError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum LspError {
    #[error("unsupported file type for LSP: {}", path.display())]
    UnsupportedFile { path: PathBuf },

    #[error("LSP server '{server}' not found in Anvil toolchains or PATH")]
    ServerNotFound { server: String },

    #[error("failed to start LSP server '{server}': {source}")]
    Spawn { server: String, source: std::io::Error },

    #[error("I/O error communicating with LSP server: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid JSON from LSP server: {0}")]
    Json(#[from] serde_json::Error),

    #[error("LSP request '{method}' timed out after {timeout_ms}ms")]
    Timeout { method: String, timeout_ms: u64 },

    #[error("LSP server returned error {code} for '{method}': {message}")]
    Rpc { method: String, code: i64, message: String },

    #[error("LSP server exited unexpectedly")]
    Disconnected,

    #[error("invalid file path or URI: {0}")]
    InvalidPath(String),
}
