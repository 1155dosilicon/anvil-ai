# AGENTS.md - Anvil offline toolchain (Windows x86_64)

This drive contains Anvil: Clang/LLVM (llvm-mingw), Rust, CMake, CTest, Ninja,
clangd, rust-analyzer and lldb for Windows x86_64. No internet and no
administrator rights are needed. Follow these steps exactly, in order.

## Step 1 - install (from the root of this drive, PowerShell)

    .\anvil.exe install --yes --json

Read the JSON on stdout. Continue only if `"ok": true`.
If `"ok": false`, report the `error` field to the user and stop.

## Step 2 - verify

Run `<install_dir>\bin\anvil.exe doctor --smoke-test --json`, where
`install_dir` comes from the step 1 JSON (default `%LOCALAPPDATA%\anvil`):

    & "$env:LOCALAPPDATA\anvil\bin\anvil.exe" doctor --smoke-test --json

Success = `"ok": true` and no `smoke_tests[].status` is `"failed"`
(`"skipped"` only means that toolchain was not installed, e.g. with `--only`).
Otherwise report `errors[]` to the user.

## Step 3 - use

- Open a NEW terminal so PATH includes `ANVIL_HOME\bin`, or load into the
  current PowerShell session: `anvil env | Invoke-Expression`.
- Tools: `clang`, `clang++`, `cargo`, `rustc`, `cmake`, `ctest`, `ninja`,
  `clangd`, `rust-analyzer`, `lldb`, `clang-format`, `clang-tidy`, `llvm-ar`,
  `llvm-windres`, `windres`.
- Do NOT install compilers with rustup, winget, choco or Visual Studio.
- Guides: `anvil skills list` then `anvil skills read <name>`.
- Agents (Claude, Codex, Antigravity, SiliconFlower) are configured by
  step 1 automatically (MCP server `anvil mcp` + skills).
  Check with `anvil agents detect --json`.

## Rules

- Always pass `--json` and `--yes`; never wait for interactive prompts.
- Never edit files under `ANVIL_HOME` by hand.
- Re-running step 1 is safe (idempotent); it is also how an existing install
  is updated to the `anvil.exe` on this drive.
