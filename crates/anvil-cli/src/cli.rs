use std::path::PathBuf;

use anvil_env::shell::Shell;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Portable offline toolchains (LLVM/Clang, Rust, CMake, Ninja, LSPs) for AI agents.
///
/// Installs pre-packaged `.tar.zst` bundles from a USB drive into the user's
/// profile without administrator rights, and exposes every tool through
/// multicall shims in `ANVIL_HOME/bin`.
#[derive(Debug, Parser)]
#[command(
    name = "anvil",
    bin_name = "anvil",
    version,
    arg_required_else_help = true,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install bundles from the source drive (streaming, SHA-256 verified).
    Install(InstallArgs),
    /// Check the installation; optionally compile and run hello-world programs.
    Doctor(DoctorArgs),
    /// Print shell commands (or JSON) that put anvil on PATH for this session.
    Env(EnvArgs),
    /// Remove the installation and undo the user environment changes.
    Uninstall(UninstallArgs),
    /// Configure AI coding agents (MCP server `anvil mcp` + skills).
    Agents(AgentsArgs),
    /// Built-in skills (coding guides) for AI agents.
    Skills(SkillsArgs),
    /// Run the unified stdio MCP server (toolchains, LSP bridge and skills).
    Mcp(McpArgs),
    /// Build, sync and verify an offline Anvil USB drive.
    Bundle(BundleArgs),
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Install(_) => "install",
            Command::Doctor(_) => "doctor",
            Command::Env(_) => "env",
            Command::Uninstall(_) => "uninstall",
            Command::Agents(a) => match a.command {
                AgentsCommand::Detect(_) => "agents detect",
                AgentsCommand::Setup(_) => "agents setup",
                AgentsCommand::Remove(_) => "agents remove",
                AgentsCommand::PrintConfig(_) => "agents print-config",
            },
            Command::Skills(a) => match a.command {
                SkillsCommand::List(_) => "skills list",
                SkillsCommand::Read(_) => "skills read",
                SkillsCommand::Sync(_) => "skills sync",
            },
            Command::Mcp(_) => "mcp",
            Command::Bundle(a) => match a.command {
                BundleCommand::InitUsb(_) => "bundle init-usb",
                BundleCommand::Pack(_) => "bundle pack",
                BundleCommand::Sync(_) => "bundle sync",
                BundleCommand::Verify(_) => "bundle verify",
                BundleCommand::PrintRecipe => "bundle print-recipe",
            },
        }
    }

    /// Whether stdout must contain a single JSON document.
    pub fn json(&self) -> bool {
        match self {
            Command::Install(a) => a.json,
            Command::Doctor(a) => a.json,
            Command::Env(a) => a.shell == Some(Shell::Json),
            Command::Uninstall(a) => a.json,
            Command::Agents(a) => match &a.command {
                AgentsCommand::Detect(a) => a.json,
                AgentsCommand::Setup(a) => a.json,
                AgentsCommand::Remove(a) => a.json,
                AgentsCommand::PrintConfig(_) => false,
            },
            Command::Skills(a) => match &a.command {
                SkillsCommand::List(a) => a.json,
                SkillsCommand::Read(a) => a.json,
                SkillsCommand::Sync(a) => a.json,
            },
            Command::Mcp(_) => false,
            Command::Bundle(a) => match &a.command {
                BundleCommand::InitUsb(a) => a.json,
                BundleCommand::Pack(a) => a.json,
                BundleCommand::Sync(a) => a.json,
                BundleCommand::Verify(a) => a.json,
                BundleCommand::PrintRecipe => true,
            },
        }
    }
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Directory containing manifest.json and bundles/ (default: directory of this executable).
    #[arg(long, value_name = "DIR")]
    pub from: Option<PathBuf>,
    /// Install directory (default: $ANVIL_HOME or %LOCALAPPDATA%\anvil).
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Comma-separated bundle ids to install (default: all bundles for this platform).
    #[arg(long, value_name = "ID,...", value_delimiter = ',')]
    pub only: Vec<String>,
    /// Do not modify the persistent user environment (PATH).
    #[arg(long)]
    pub skip_env: bool,
    /// Re-extract bundles even if the same version is already installed.
    #[arg(long)]
    pub force: bool,
    /// Do not configure AI agents (MCP server + skills).
    #[arg(long, conflicts_with = "all_agents")]
    pub skip_agents: bool,
    /// Configure every supported AI agent, not only the detected ones.
    #[arg(long)]
    pub all_agents: bool,
    /// Do not ask for confirmation (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Install directory to check.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Also compile hello-world programs in C, C++ (-static), Rust and Rust gnullvm and run them with a clean system PATH.
    #[arg(long)]
    pub smoke_test: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EnvArgs {
    /// Install directory.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Output format (default: powershell).
    #[arg(
        long,
        value_name = "SHELL",
        value_parser = PossibleValuesParser::new(Shell::NAMES).map(|s| s.parse::<Shell>().expect("validated by clap"))
    )]
    pub shell: Option<Shell>,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Install directory to remove.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Leave the persistent user environment (PATH) untouched.
    #[arg(long)]
    pub keep_env: bool,
    /// Leave the `anvil` entries and skills in AI agent configurations.
    #[arg(long)]
    pub keep_agents: bool,
    /// Do not ask for confirmation (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub command: AgentsCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentsCommand {
    /// Report which agents are installed and whether anvil is configured in them.
    Detect(AgentsDetectArgs),
    /// Register the anvil MCP server and install skills (detected agents by default).
    Setup(AgentsSetupArgs),
    /// Remove anvil's MCP entry and anvil's skills (nothing else) from agents.
    Remove(AgentsRemoveArgs),
    /// Print a generic MCP configuration snippet for any other client.
    PrintConfig(PrintConfigArgs),
}

#[derive(Debug, Args)]
pub struct AgentsDetectArgs {
    /// Install directory whose `bin/anvil` the agents should use.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AgentsSetupArgs {
    /// Install directory whose `bin/anvil` the agents should use.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Comma-separated agents: siliconflower, claude, codex, antigravity.
    #[arg(long, value_name = "AGENT,...", value_delimiter = ',')]
    pub only: Vec<String>,
    /// Every supported agent (and optional targets such as Claude Desktop), even if not detected.
    #[arg(long)]
    pub all: bool,
    /// Only register the MCP server; do not write skills.
    #[arg(long)]
    pub skip_skills: bool,
    /// Do not ask for confirmation (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AgentsRemoveArgs {
    /// Install directory (used for backups and state.json).
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Comma-separated agents (default: every supported agent).
    #[arg(long, value_name = "AGENT,...", value_delimiter = ',')]
    pub only: Vec<String>,
    /// Do not ask for confirmation (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigFormatArg {
    Json,
    Toml,
}

#[derive(Debug, Args)]
pub struct PrintConfigArgs {
    /// Install directory whose `bin/anvil` the snippet should use.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// `json` (`mcpServers`, most clients) or `toml` (`mcp_servers`, Codex).
    #[arg(long, value_enum, default_value_t = ConfigFormatArg::Json)]
    pub format: ConfigFormatArg,
}

#[derive(Debug, Args)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// List the available skills.
    List(SkillsListArgs),
    /// Print a skill (works straight from the USB drive).
    Read(SkillsReadArgs),
    /// Write anvil's skills into agent skill directories (no MCP changes).
    Sync(SkillsSyncArgs),
}

#[derive(Debug, Args)]
pub struct SkillsListArgs {
    /// Install directory (extra skills in `<dir>/skills` are listed too).
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SkillsReadArgs {
    /// Skill name (see `anvil skills list`).
    pub name: String,
    /// Install directory to look for non-embedded skills in.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SkillsSyncArgs {
    /// Install directory.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Comma-separated agents (default: detected agents).
    #[arg(long, value_name = "AGENT,...", value_delimiter = ',')]
    pub only: Vec<String>,
    /// Every supported agent, even if not detected.
    #[arg(long)]
    pub all: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    /// Install directory containing toolchains and skills (default: auto-detected ANVIL_HOME).
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Default workspace directory for build and LSP requests (default: current working directory).
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct BundleArgs {
    #[command(subcommand)]
    pub command: BundleCommand,
}

#[derive(Debug, Subcommand)]
pub enum BundleCommand {
    /// Initialize a USB drive root with AGENTS.md, START_HERE.md, install scripts, skills and binaries.
    InitUsb(BundleInitUsbArgs),
    /// Pack a local toolchain directory into a `.tar.zst` bundle and update manifest.json.
    #[command(disable_version_flag = true)]
    Pack(BundlePackArgs),
    /// Download official upstream toolchains (online) and build `.tar.zst` bundles onto the USB drive.
    Sync(BundleSyncArgs),
    /// Verify SHA-256 checksums, sizes and files of an offline USB drive.
    Verify(BundleVerifyArgs),
    /// Print the built-in upstream recipe JSON (useful as a template for `--recipe`).
    PrintRecipe,
}

#[derive(Debug, Args)]
pub struct BundleInitUsbArgs {
    /// Path to the USB drive or staging directory.
    #[arg(long, value_name = "DIR")]
    pub usb: PathBuf,
    /// Optional path to `anvil.exe` (Windows x86_64) to copy to the USB root.
    #[arg(long, value_name = "FILE")]
    pub win_exe: Option<PathBuf>,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct BundlePackArgs {
    /// Path to the USB drive or staging directory.
    #[arg(long, value_name = "DIR")]
    pub usb: PathBuf,
    /// Target platform (`windows-x86_64`).
    #[arg(long, value_name = "PLATFORM", default_value = "windows-x86_64")]
    pub platform: String,
    /// Bundle id (`llvm`, `rust`, `build-tools`).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// Version label recorded in manifest.json.
    #[arg(long, value_name = "VERSION")]
    pub version: String,
    /// Local directory containing the extracted toolchain (`bin/`, `lib/`, ...).
    #[arg(long, value_name = "DIR")]
    pub source: PathBuf,
    /// Optional archive filename (default `<id>.tar.zst`).
    #[arg(long, value_name = "FILE")]
    pub archive_name: Option<String>,
    /// Zstandard compression level (1..19, default 15).
    #[arg(long, default_value_t = 15)]
    pub level: i32,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct BundleSyncArgs {
    /// Path to the USB drive or staging directory.
    #[arg(long, value_name = "DIR")]
    pub usb: PathBuf,
    /// Target platforms (`windows-x86_64`; default: all).
    #[arg(long, value_name = "PLATFORM,...", value_delimiter = ',')]
    pub targets: Vec<String>,
    /// Comma-separated bundle ids to sync (default: all).
    #[arg(long, value_name = "ID,...", value_delimiter = ',')]
    pub only: Vec<String>,
    /// Optional custom JSON recipe file (overrides built-in upstream URLs/versions).
    #[arg(long, value_name = "FILE")]
    pub recipe: Option<PathBuf>,
    /// Directory to cache downloaded upstream archives (default: `<usb>/.anvil-cache`).
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
    /// Zstandard compression level (1..19, default 15).
    #[arg(long, default_value_t = 15)]
    pub level: i32,
    /// Show the download/pack plan without downloading or modifying the USB drive.
    #[arg(long)]
    pub dry_run: bool,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct BundleVerifyArgs {
    /// Path to the USB drive or staging directory.
    #[arg(long, value_name = "DIR")]
    pub usb: PathBuf,
    /// Machine-readable output: stdout contains only a JSON document.
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_install_flags() {
        let cli = Cli::try_parse_from([
            "anvil",
            "install",
            "--from",
            "E:\\",
            "--dest",
            "D:\\x",
            "--only",
            "llvm,rust",
            "--skip-env",
            "--yes",
            "--json",
        ])
        .unwrap();
        let Command::Install(a) = &cli.command else { panic!() };
        assert_eq!(a.only, vec!["llvm", "rust"]);
        assert!(a.skip_env && a.yes && a.json && !a.force);
        assert!(cli.command.json());
    }

    #[test]
    fn parses_agents_and_skills_subcommands() {
        let cli =
            Cli::try_parse_from(["anvil", "agents", "setup", "--only", "claude,codex", "--yes", "--json"]).unwrap();
        let Command::Agents(AgentsArgs {
            command: AgentsCommand::Setup(a),
        }) = &cli.command
        else {
            panic!()
        };
        assert_eq!(a.only, vec!["claude", "codex"]);
        assert_eq!(cli.command.name(), "agents setup");
        assert!(cli.command.json());

        let cli = Cli::try_parse_from(["anvil", "agents", "print-config", "--format", "toml"]).unwrap();
        assert!(!cli.command.json(), "print-config prints the raw snippet");
        assert!(Cli::try_parse_from(["anvil", "agents", "print-config", "--format", "yaml"]).is_err());

        let cli = Cli::try_parse_from(["anvil", "skills", "read", "rust-systems", "--json"]).unwrap();
        assert_eq!(cli.command.name(), "skills read");
        assert!(Cli::try_parse_from(["anvil", "install", "--skip-agents", "--all-agents"]).is_err());
        assert!(Cli::try_parse_from(["anvil", "uninstall", "--keep-agents", "--yes"]).is_ok());
    }

    #[test]
    fn env_shell_json_implies_json_output() {
        let cli = Cli::try_parse_from(["anvil", "env", "--shell", "json"]).unwrap();
        assert!(cli.command.json());
        assert!(Cli::try_parse_from(["anvil", "env", "--shell", "cmd"]).is_err());
    }
}
