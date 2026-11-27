---
name: anvil-toolkit
title: Anvil Toolkit
description: Use the portable `anvil` toolchain (Clang/LLVM, Rust, CMake, Ninja, clangd, rust-analyzer, lldb) offline - install it, verify it, wire it into AI agents and call its MCP server for builds and LSP diagnostics.
---

Anvil installs a complete offline native toolchain into the user profile
(no admin rights) and exposes every tool through shims in `ANVIL_HOME/bin`.
Always prefer `--json`: stdout is then exactly one JSON document, progress and
warnings go to stderr, and the exit code is `0` on success.

## 1. Is Anvil installed and healthy?

```sh
anvil doctor --smoke-test --json
```

- `ok: true` -> ready. `tools[]` lists every shim with its version;
  `smoke_tests[]` compiles and runs hello-world in C, C++ and Rust.
- `ok: false` -> read `errors[]` and fix them; the most common fix is step 2.
- `command not found` -> run the binary from the USB drive root
  (`anvil.exe` on Windows, `./anvil` on Linux) or use the absolute path
  `ANVIL_HOME/bin/anvil` (`%LOCALAPPDATA%\anvil\bin\anvil.exe` on Windows,
  `~/.local/share/anvil/bin/anvil` on Linux).

## 2. Install / repair (idempotent, safe to re-run)

```sh
anvil install --yes --json
```

- Verifies SHA-256 of every bundle while streaming it; unchanged bundles are skipped.
- Adds `ANVIL_HOME/bin` to the user PATH (open a NEW terminal afterwards), or
  load it into the current shell: `eval "$(anvil env)"` (bash/zsh) or
  `anvil env | Invoke-Expression` (PowerShell).
- Useful flags: `--only llvm,rust`, `--dest <dir>`, `--skip-env`,
  `--skip-agents`, `--all-agents`, `--force`.

## 3. Wire Anvil into AI agents

```sh
anvil agents detect --json          # what is installed, what is configured
anvil agents setup --yes --json     # MCP + skills for every detected agent
anvil agents setup --only codex,claude --yes --json
anvil agents print-config --format json   # snippet for any other MCP client
```

Supported: `siliconflower`, `claude`, `codex`, `antigravity`. Existing
settings, other MCP servers and comments are preserved; a backup of every
modified file goes to `ANVIL_HOME/backups/`. `anvil agents remove --yes`
removes only Anvil's entries and Anvil's own skills.

## 4. Skills (this file is one of them)

```sh
anvil skills list --json
anvil skills read rust-systems
anvil skills sync --json            # refresh Anvil's skills in every detected agent
```

Available: `anvil-toolkit`, `rust-systems`, `c-modern`, `cpp-modern`,
`cmake-ninja-debug`. Read the one that matches the task before coding.

## 5. MCP server: `anvil mcp`

Agents configured by `anvil agents setup` start `ANVIL_HOME/bin/anvil mcp`
(stdio). It offers tools to build, test and run projects with the Anvil
toolchain and an LSP bridge (clangd / rust-analyzer) for diagnostics, hover
and go-to-definition. Call `tools/list` to discover the exact tool names and
schemas instead of guessing them.

## Rules

- Never call `rustup`, `apt`, `winget`, `choco` or Visual Studio to get a compiler:
  Anvil already provides `clang`, `clang++`, `cargo`, `rustc`, `cmake`, `ninja`.
- Shims set `CC=clang`, `CXX=clang++`, `CMAKE_GENERATOR=Ninja`, `CARGO_HOME`,
  `RUSTC`, `RUST_SRC_PATH` and (Windows) the Cargo linker automatically.
- Uninstall: `anvil uninstall --yes --json` (add `--keep-agents` to keep agent configs).
