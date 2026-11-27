//! `anvil` – single multicall binary.
//!
//! Invoked as `anvil`, it is a regular CLI. Invoked through a shim
//! (`clang`, `cargo`, `cmake`, ...) it transparently runs the real tool.

mod cli;
mod commands;
mod output;

use std::io::Write;
use std::process::exit;

use clap::error::ErrorKind;
use clap::Parser;
use serde_json::json;

use crate::cli::Cli;

fn main() {
    // Multicall dispatch must happen before clap sees the arguments: they belong to the tool.
    if let Some(result) = anvil_env::dispatch_multicall() {
        match result {
            Ok(code) => exit(code),
            Err(err) => {
                eprintln!("anvil: {err}");
                exit(127);
            }
        }
    }

    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => {
            let wants_json = args.iter().any(|a| a == "--json")
                || args.windows(2).any(|w| w[0] == "--shell" && w[1] == "json")
                || args.iter().any(|a| a == "--shell=json");
            if wants_json && !matches!(err.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                output::print_json(&json!({
                    "ok": false,
                    "error": err.render().to_string().trim(),
                    "kind": "usage",
                }));
                exit(2);
            }
            err.exit();
        }
    };

    let json_mode = cli.command.json();
    let name = cli.command.name();
    let code = match commands::run(cli.command) {
        Ok(outcome) => {
            match (json_mode, outcome.json) {
                (true, Some(doc)) => output::print_json(&doc),
                _ => {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(outcome.human.as_bytes());
                    let _ = out.flush();
                }
            }
            outcome.exit_code
        }
        Err(err) => {
            if json_mode {
                output::print_json(&output::error_json(name, &err));
            } else {
                eprintln!("error: {err}");
                for cause in output::causes(&err) {
                    eprintln!("  caused by: {cause}");
                }
            }
            1
        }
    };
    exit(code);
}
