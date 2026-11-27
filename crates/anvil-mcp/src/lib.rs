//! Unified stdio MCP server for Anvil.

pub mod protocol;
pub mod toolchain;
pub mod tools;

use std::io::{self, BufReader};
use std::path::PathBuf;

use anvil_core::{EnvMode, InstallLayout};

pub use protocol::{handle_line, serve_stdio};
pub use tools::{ToolCallOutcome, ToolRegistry};

/// Run the MCP server on process stdin/stdout until EOF, then cleanly shut down all LSP servers.
pub fn run_stdio_server(layout: InstallLayout, default_workspace: PathBuf, env_mode: EnvMode) -> io::Result<()> {
    let registry = ToolRegistry::new(layout, default_workspace, env_mode);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let result = serve_stdio(&mut reader, &mut writer, &registry);
    registry.shutdown();
    result
}
