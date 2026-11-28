---
name: rust-systems
title: Rust Systems Programming on Windows
description: Idiomatic Rust on Windows x86_64 with Cargo, clippy, rustfmt and rust-analyzer - project setup, error handling, tests, C interop and standalone Windows builds using the offline Anvil toolchain.
---

## Workflow (run in this order, fix every failure before moving on)

```powershell
cargo new --lib mycrate          # or: cargo new myapp
cargo build                      # debug build
cargo fmt --all                  # format (use `cargo fmt --all --check` in CI)
cargo clippy --all-targets -- -D warnings
cargo test                       # unit + integration + doc tests
cargo build --release
```

- Offline machine: add `--offline` (or `--frozen` with a committed
  `Cargo.lock`). Dependencies must already be in `CARGO_HOME` or vendored
  (`cargo vendor`).
- Read the FIRST compiler error only, fix it, rebuild. Later errors are often
  consequences of the first.
- `rust-analyzer` works offline: Anvil sets `RUST_SRC_PATH`, so `std`/`core`
  resolve without `rustup`.

## Project layout

```text
Cargo.toml        # [package] edition = "2021", rust-version = "..."
src/lib.rs        # logic (testable)
src/main.rs       # thin CLI calling the library
tests/*.rs        # integration tests (public API only)
```

Workspaces: root `Cargo.toml` with `[workspace] members = ["crates/*"]` and
shared `[workspace.dependencies]`.

## Error handling

- Libraries: one error enum per crate with `thiserror`; never `panic!` on bad input.
- Binaries: `anyhow::Result` + `.context("what failed")` at each I/O boundary.
- Use `?` to propagate. `unwrap()`/`expect()` only for real invariants
  (with a message that says why it cannot fail) and in tests.
- Return `Option` for "not found", `Result` for "failed".

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("invalid value for {0}")]
    Invalid(String),
}
```

## Idioms that avoid common bugs

- Borrow instead of clone: take `&str`, `&[T]`, `&Path`; return owned values.
- Prefer iterators (`iter().map().filter().collect()`) over index loops.
- Shared state across threads: `Arc<Mutex<T>>` or channels; never `static mut`.
- `unsafe` only with a `// SAFETY:` comment explaining the invariant.
- Exhaustive `match` on enums; avoid `_ =>` when new variants must be handled.

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_valid_input() { assert_eq!(parse("42").unwrap(), 42); }
}
```

Use `tempfile::tempdir()` for filesystem tests; never write to the real user
profile or Windows Registry. Run one test:
`cargo test parses_valid_input -- --nocapture`.

## Windows x86_64 targets and linking in Anvil

- **Default target: `x86_64-pc-windows-gnu` (self-contained):**
  - Anvil ships Rust's `x86_64-pc-windows-gnu` toolchain with `rust-mingw`
    self-contained libraries and configures `-C link-self-contained=yes`
    automatically via the shims — no Visual Studio / MSVC needed.
  - Resulting `.exe` binaries depend only on standard Windows system DLLs
    (`KERNEL32.dll`, `msvcrt.dll`, `ntdll.dll`, etc.) and run standalone on any
    Windows 10/11 PC.
  - C build scripts (`build.rs` / `cc` crate) compile with `CC=clang.exe` and
    `llvm-ar` from Anvil's `llvm-mingw` and link cleanly with the default target.
- **Optional target: `x86_64-pc-windows-gnullvm`:**
  - `rust-std` for `x86_64-pc-windows-gnullvm` is also bundled and pre-wired to
    link with `llvm-mingw`'s `clang.exe` / `ld.lld`
    (`cargo build --target x86_64-pc-windows-gnullvm`).
  - Anvil sets `CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS=-C target-feature=+crt-static`
    (unless you already set it), so `gnullvm` binaries link `libunwind` statically
    and run standalone too. If you override `RUSTFLAGS`, keep `+crt-static` or the
    `.exe` will need `libunwind.dll` from `toolchains\llvm\bin`.
- Build paths with `Path::join`, never string concatenation; handle `\r\n` with
  `str::lines()`.

## Debugging

`cargo build` then `lldb target\debug\myapp.exe` (`b main`, `r`, `bt`, `p var`).
Set `$env:RUST_BACKTRACE = "1"` to get a backtrace on panic.
