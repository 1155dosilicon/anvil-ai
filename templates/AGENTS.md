# AGENTS.md - Anvil offline toolchain

This drive contains Anvil: Clang/LLVM, Rust, CMake, Ninja, clangd,
rust-analyzer and lldb for Windows x86_64 and Linux x86_64. No internet and
no administrator rights are needed. Follow these steps exactly, in order.

## Step 1 - install (from the root of this drive)

Windows (PowerShell):

    .\anvil.exe install --yes --json

Linux (sh):

    sh ./install.sh --json

Read the JSON on stdout. Continue only if `"ok": true`.
If `"ok": false`, report the `error` field to the user and stop.

## Step 2 - verify

Run `<install_dir>/bin/anvil doctor --smoke-test --json`, where
`install_dir` comes from the step 1 JSON. Defaults:

Windows:

    & "$env:LOCALAPPDATA\anvil\bin\anvil.exe" doctor --smoke-test --json

Linux:

    ~/.local/share/anvil/bin/anvil doctor --smoke-test --json

Success = `"ok": true` and every `smoke_tests[].status` is `"ok"`.
Otherwise report `errors[]` to the user.

## Step 3 - use

- Open a NEW terminal so PATH includes `ANVIL_HOME/bin`, or run
  `anvil env | Invoke-Expression` (PowerShell) / `eval "$(anvil env)"` (sh).
- Tools: `clang`, `clang++`, `cargo`, `rustc`, `cmake`, `ninja`, `clangd`,
  `rust-analyzer`, `lldb`, `clang-format`, `clang-tidy`.
- Do NOT install compilers with rustup, apt, winget, choco or Visual Studio.
- Guides: `anvil skills list` then `anvil skills read <name>`.
- Agents (Claude, Codex, Antigravity, SiliconFlower) are configured by
  step 1 automatically (MCP server `anvil mcp` + skills).
  Check with `anvil agents detect --json`.

## Rules

- Always pass `--json` and `--yes`; never wait for interactive prompts.
- Never edit files under `ANVIL_HOME` by hand.
- Re-running step 1 is safe (idempotent).
