mod agents;
mod bundle;
mod doctor;
mod env;
mod install;
mod mcp;
mod skills;
mod uninstall;

use anyhow::Result;

use crate::cli::Command;
use crate::output::Outcome;

pub fn run(command: Command) -> Result<Outcome> {
    match command {
        Command::Install(args) => install::run(&args),
        Command::Doctor(args) => doctor::run(&args),
        Command::Env(args) => env::run(&args),
        Command::Uninstall(args) => uninstall::run(&args),
        Command::Agents(args) => agents::run(&args),
        Command::Skills(args) => skills::run(&args),
        Command::Mcp(args) => mcp::run(&args),
        Command::Bundle(args) => bundle::run(&args),
    }
}
