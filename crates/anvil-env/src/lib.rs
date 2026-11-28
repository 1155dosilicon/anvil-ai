//! Environment integration for Anvil on Windows x86_64.
//!
//! * [`tools`] – catalogue of managed tools and real-binary lookup.
//! * [`shim`] – creation of multicall shims in `ANVIL_HOME\bin`.
//! * [`dispatch`] – runtime resolution when `anvil.exe` is invoked as a tool name.
//! * [`userenv`] – persistent user PATH (`HKCU\Environment`).
//! * [`winpath`] – pure helper functions behind `userenv`.
//! * [`shell`] – `anvil env` script rendering (PowerShell / JSON).
//! * [`doctor`] – health checks and compile smoke tests.

pub mod dispatch;
pub mod doctor;
pub mod error;
pub mod process;
pub mod shell;
pub mod shim;
pub mod tools;
pub mod userenv;
pub mod winpath;

pub use dispatch::{dispatch_multicall, ToolEnv};
pub use doctor::{run_doctor, DoctorOptions, DoctorReport, Status};
pub use error::{EnvError, Result};
pub use shim::{install_shims, remove_shims, ShimReport};
pub use tools::{ManagedTool, MANAGED_TOOLS, RETIRED_TOOLS, TOOLCHAINS};
pub use userenv::{apply_user_env, remove_user_env, user_env_configured, EnvChange, EnvMode, ENV_SANDBOX_VAR};
