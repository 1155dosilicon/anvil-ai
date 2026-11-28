# Anvil - start here

Anvil installs a complete, offline C/C++/Rust toolchain for Windows x86_64
from this drive: Clang/LLVM (llvm-mingw), Rust (cargo, clippy, rustfmt,
rust-analyzer, rustdoc), CMake, CTest, Ninja, clangd, clang-tidy, clang-format
and lldb. Everything goes into your user profile; no administrator rights and
no internet connection are needed.

## Windows 10/11 (x86_64)

1. Open the drive in Explorer.
2. Right-click `install.ps1` -> **Run with PowerShell**
   (or in a terminal: `powershell -ExecutionPolicy Bypass -File .\install.ps1`).
3. Open a **new** terminal and check: `anvil doctor --smoke-test`

Installed to `%LOCALAPPDATA%\anvil` (`bin\` is added to your user `Path` in
`HKCU\Environment`).

To update an existing install, run `install.ps1` again from a drive with the
newer `anvil.exe`: toolchains that did not change are skipped, shims and agent
configuration are refreshed. Restart open AI agents afterwards so they load
the new `anvil mcp` server.

## AI agents

The installer detects Claude Code / Claude Desktop, Codex, Antigravity and
SiliconFlower and registers the `anvil` MCP server plus coding skills in each
of them. Your existing settings are kept and backed up to
`<install dir>\backups\`. Useful commands:

    anvil agents detect           # what was found and configured
    anvil agents setup --all      # configure every supported agent
    anvil agents print-config     # MCP snippet for any other client
    anvil skills list             # built-in guides for agents

Skip agent configuration with `install --skip-agents`.

## Options

    anvil install --help          # --dest DIR, --only llvm,rust, --skip-env, ...
    anvil env                     # print PowerShell commands for this session only
    anvil uninstall --yes         # remove everything (agent entries included)

AI agents: read `AGENTS.md` on this drive.
