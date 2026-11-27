//! LSP client bridge (`rust-analyzer` and `clangd`) for Anvil.
//!
//! Translates stateful LSP JSON-RPC into simple, 1-based line/column queries
//! suitable for AI agents and smaller open-weights models.

pub mod client;
pub mod error;
pub mod pool;
pub mod protocol;
pub mod types;
pub mod uri;

pub use client::{LspClient, ServerCommand};
pub use error::{LspError, Result};
pub use pool::LspPool;
pub use types::{discover_workspace_root, DiagnosticItem, HoverInfo, LanguageKind, LocationItem, SymbolItem};
pub use uri::{normalize_uri, path_to_uri, uri_to_path};
