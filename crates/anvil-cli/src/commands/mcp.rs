use anvil_core::paths::{absolutize, resolve_install_dir};
use anvil_core::{EnvMode, InstallLayout};
use anyhow::{Context, Result};

use crate::cli::McpArgs;
use crate::output::Outcome;

pub fn run(args: &McpArgs) -> Result<Outcome> {
    let install_dir = match &args.dest {
        Some(dir) => absolutize(dir)?,
        None => resolve_install_dir(None).or_else(|_| {
            // Fallback relative to running binary (`ANVIL_HOME/bin/anvil` -> `ANVIL_HOME`).
            let exe = std::env::current_exe().context("cannot locate running executable")?;
            Ok::<_, anyhow::Error>(
                exe.parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(exe.parent().unwrap_or(&exe))
                    .to_path_buf(),
            )
        })?,
    };
    let workspace = match &args.workspace {
        Some(w) => absolutize(w)?,
        None => std::env::current_dir().context("cannot determine current working directory")?,
    };

    anvil_mcp::run_stdio_server(InstallLayout::new(install_dir), workspace, EnvMode::from_process_env())
        .context("MCP stdio server failed")?;

    Ok(Outcome {
        json: None,
        human: String::new(),
        exit_code: 0,
    })
}
