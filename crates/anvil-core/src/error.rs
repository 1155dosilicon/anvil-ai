use std::io;
use std::path::{Path, PathBuf};

/// Errors produced by `anvil-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error at '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid JSON in '{}': {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("unsupported {what} schema version {found} (this anvil understands {expected})")]
    UnsupportedSchema {
        what: &'static str,
        found: u32,
        expected: u32,
    },

    #[error("bundle '{id}' is not available for platform {platform}")]
    UnknownBundle { id: String, platform: String },

    #[error("SHA-256 mismatch for bundle '{id}': expected {expected}, got {actual}")]
    ChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },

    #[error("size mismatch for bundle '{id}': manifest says {expected} bytes, archive has {actual} bytes")]
    SizeMismatch { id: String, expected: u64, actual: u64 },

    #[error("unsafe entry '{entry}' in bundle '{id}': {reason}")]
    UnsafeEntry { id: String, entry: String, reason: String },

    #[error("corrupt or unreadable archive for bundle '{id}': {source}")]
    Archive {
        id: String,
        #[source]
        source: io::Error,
    },

    #[error("cannot determine {0}")]
    Environment(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

/// Attach a path to an [`io::Error`].
pub trait IoContext<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T> {
        self.map_err(|source| CoreError::Io {
            path: path.as_ref().to_path_buf(),
            source,
        })
    }
}
