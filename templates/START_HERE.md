# Anvil - start here

Anvil installs a complete, offline C/C++/Rust toolchain from this drive:
Clang/LLVM, Rust (cargo, clippy, rustfmt, rust-analyzer), CMake, Ninja,
clangd, clang-tidy, clang-format and lldb. Everything goes into your user
profile; no administrator rights and no internet connection are needed.

## Windows 10/11 (x86_64)

1. Open the drive in Explorer.
2. Right-click `install.ps1` -> **Run with PowerShell**
   (or in a terminal: `powershell -ExecutionPolicy Bypass -File .\install.ps1`).
3. Open a **new** terminal and check: `anvil doctor --smoke-test`

Installed to `%LOCALAPPDATA%\anvil` (`bin\` is added to your user PATH).

## Linux (x86_64)

1. Open a terminal in the drive's root directory.
2. Run: `sh ./install.sh`
3. Open a **new** terminal (or log out and back in for GUI apps) and check:
   `anvil doctor --smoke-test`

Installed to `~/.local/share/anvil` (`bin/` is added via `~/.profile`,
shell rc files and `~/.config/environment.d`).

## AI agents

The installer detects Claude Code / Claude Desktop, Codex, Antigravity and
SiliconFlower and registers the `anvil` MCP server plus coding skills in each
of them. Your existing settings are kept and backed up to
`<install dir>/backups/`. Useful commands:

    anvil agents detect           # what was found and configured
    anvil agents setup --all      # configure every supported agent
    anvil agents print-config     # MCP snippet for any other client
    anvil skills list             # built-in guides for agents

Skip agent configuration with `install --skip-agents`.

## Options

    anvil install --help          # --dest DIR, --only llvm,rust, --skip-env, ...
    anvil env                     # print commands to use anvil in this shell only
    anvil uninstall --yes         # remove everything (agent entries included)

AI agents: read `AGENTS.md` on this drive.
