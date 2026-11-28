//! Core building blocks for Anvil.
//!
//! * [`platform`] – supported host platforms.
//! * [`paths`] – install directory / source root resolution and the on-disk layout.
//! * [`manifest`] – the `manifest.json` shipped next to the bundles on the USB drive.
//! * [`state`] – the `state.json` written into the install directory.
//! * [`envmode`] – real vs. sandboxed user environment (registry, `%USERPROFILE%`, agent configs).
//! * [`archive`] – single-pass streaming `.tar.zst` extraction with SHA-256 verification.
//! * [`hash`] / [`progress`] – helpers used by the extractor.

#[cfg(not(all(windows, target_arch = "x86_64")))]
compile_error!("Anvil supports only Windows x86_64 (build with --target x86_64-pc-windows-gnu or -msvc)");

pub mod archive;
pub mod envmode;
pub mod error;
pub mod hash;
pub mod manifest;
pub mod paths;
pub mod platform;
pub mod progress;
pub mod state;

pub use archive::{extract_bundle_streaming, pack_dir_tar_zst, ArchiveInfo, ExtractReport, ExtractRequest};
pub use envmode::{EnvMode, ENV_SANDBOX_VAR};
pub use error::{CoreError, Result};
pub use manifest::{BundleSpec, Manifest, MANIFEST_FILE};
pub use paths::{InstallLayout, ANVIL_HOME_ENV};
pub use platform::Platform;
pub use progress::{Progress, SilentProgress, StderrProgress};
pub use state::{InstallState, InstalledBundle, STATE_FILE};

/// Version of the Anvil binary (shared by every crate of the workspace).
pub const ANVIL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current schema version of both `manifest.json` and `state.json`.
pub const SCHEMA_VERSION: u32 = 1;
