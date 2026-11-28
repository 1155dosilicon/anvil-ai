---
name: anvil-toolkit
title: Anvil Toolkit
description: Use the portable `anvil` toolchain for Windows x86_64 (Clang/LLVM, Rust, CMake, CTest, Ninja, clangd, rust-analyzer, lldb) offline - install it, verify it, wire it into AI agents and call its MCP server for builds and LSP diagnostics.
---

Anvil installs a complete offline native Windows x86_64 toolchain into the user
profile (no admin rights, no Visual Studio Build Tools) and exposes every tool
through shims in `ANVIL_HOME\bin`. Always prefer `--json`: stdout is then
exactly one JSON document, progress and warnings go to stderr, and the exit
code is `0` on success.

## 1. Is Anvil installed and healthy?

```powershell
anvil doctor --smoke-test --json
```

- `ok: true` -> ready. `tools[]` lists every shim with its version;
  `smoke_tests[]` compiles C, C++ (`-static`), Rust and Rust `gnullvm` and runs
  each `.exe` with a clean Windows system `PATH` to catch missing-DLL
  regressions. `"skipped"` only means that toolchain is not installed
  (e.g. after `install --only`); `"failed"` is a real problem.
- `ok: false` -> read `errors[]` and fix them; the most common fix is step 2.
- Command not found -> run `.\anvil.exe` from the USB drive root or use the
  absolute path `%LOCALAPPDATA%\anvil\bin\anvil.exe` (or `$env:ANVIL_HOME\bin\anvil.exe`).

## 2. Install / repair (idempotent, safe to re-run)

```powershell
.\anvil.exe install --yes --json
```

- Verifies SHA-256 of every bundle while streaming it; unchanged bundles are skipped.
- Adds `ANVIL_HOME\bin` to `HKCU\Environment\Path` (open a NEW terminal
  afterwards), or load it into the current PowerShell session:
  `anvil env | Invoke-Expression`.
- Useful flags: `--only llvm,rust`, `--dest <dir>`, `--skip-env`,
  `--skip-agents`, `--all-agents`, `--force`.

## 3. Wire Anvil into AI agents

```powershell
anvil agents detect --json          # what is installed, what is configured
anvil agents setup --yes --json     # MCP + skills for every detected agent
anvil agents setup --only codex,claude --yes --json
anvil agents print-config --format json   # snippet for any other MCP client
```

Supported: `siliconflower`, `claude`, `codex`, `antigravity`. Existing
settings, other MCP servers and comments are preserved; a backup of every
modified file goes to `ANVIL_HOME\backups\`. `anvil agents remove --yes`
removes only Anvil's entries and Anvil's own skills.

## 4. Skills (this file is one of them)

```powershell
anvil skills list --json
anvil skills read rust-systems
anvil skills sync --json            # refresh Anvil's skills in every detected agent
```

Available: `anvil-toolkit`, `rust-systems`, `c-modern`, `cpp-modern`,
`cmake-ninja-debug`. Read the one that matches the task before coding.

## 5. MCP server: `anvil mcp`

Agents configured by `anvil agents setup` start `ANVIL_HOME\bin\anvil.exe mcp`
(stdio). It offers tools to build, test and run projects with the Anvil
toolchain (`rust_cargo`, `cc_build`, `cc_lint_format`) and an LSP bridge
(`clangd` / `rust-analyzer`) for diagnostics, hover, definition, references and
symbols. Call `tools/list` to discover exact tool names and schemas.

`cc_build` produces standalone `.exe` files by default: C++ is linked with
`-static` (single files, and fresh CMake build directories via
`CMAKE_EXE_LINKER_FLAGS_INIT`), except with sanitizers or shared libraries.
Pass `static_link: true|false` to force or disable it.

## Rules

- Never call `rustup`, `winget`, `choco` or Visual Studio to get a compiler:
  Anvil already provides `clang`, `clang++`, `cargo`, `rustc`, `cmake`,
  `ctest`, `ninja`, `lldb`, `llvm-ar`, `llvm-windres`, `windres`, etc.
- Shims set `CC=clang.exe`, `CXX=clang++.exe`, `CMAKE_GENERATOR=Ninja`,
  `CARGO_HOME`, `RUSTC`, `RUST_SRC_PATH` and Cargo linker/self-contained flags
  automatically (including `+crt-static` for the `x86_64-pc-windows-gnullvm`
  target, so its binaries need no `libunwind.dll`).
- Updating: re-run `install.ps1` / `anvil.exe install --yes` from a drive with
  the newer `anvil.exe`; installed toolchains with the same version are kept.
- Uninstall: `anvil uninstall --yes --json` (add `--keep-agents` to keep agent configs).
