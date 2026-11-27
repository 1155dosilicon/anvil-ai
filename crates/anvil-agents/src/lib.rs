//! AI agent integration for Anvil.
//!
//! * [`skills`] – canonical skills embedded in the binary and their
//!   folder (`<name>/SKILL.md`) / flat (`<name>.md`) renderings.
//! * [`config`] – surgical JSON / TOML edits of MCP configuration files.
//! * [`agents`] – adapters for SiliconFlower, Claude, Codex and Antigravity:
//!   detection, idempotent setup with backups and atomic writes, removal.
//! * [`paths`] – where agent files live (real user or [`anvil_core::EnvMode::Sandbox`]).
//! * [`templates`] – files for the root of the USB drive.

pub mod agents;
pub mod config;
pub mod error;
pub mod fsutil;
pub mod paths;
pub mod skills;
pub mod templates;

pub use agents::{
    detect_agent, detect_all, install_canonical_skills, parse_agent_list, remove_agent, setup_agent, Action,
    AgentContext, AgentId, AgentReport, AgentStatus, RemoveOptions, Selection, SetupOptions,
};
pub use config::{generic_snippet, ConfigFormat, McpEntry};
pub use error::{AgentError, Result};
pub use paths::AgentPaths;
pub use skills::{embedded, embedded_skill, Skill, SkillLayout};
