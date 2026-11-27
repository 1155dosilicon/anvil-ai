---
name: rust-systems
title: Rust Systems Programming
description: Idiomatic Rust with Cargo, clippy, rustfmt and rust-analyzer - project setup, error handling, tests and Windows/Linux portability using the offline Anvil toolchain.
---

## Workflow (run in this order, fix every failure before moving on)

```sh
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

Use `tempfile::tempdir()` for filesystem tests; never write to the real home
directory. Run one test: `cargo test parses_valid_input -- --nocapture`.

## Windows / Linux portability

- Build paths with `Path::join`, never with `"/"` or `"\\"` string concatenation.
- Executables: `format!("tool{}", std::env::consts::EXE_SUFFIX)`.
- Platform code: `#[cfg(windows)]` / `#[cfg(unix)]`; keep both branches compiling.
- Text files may have `\r\n`: use `str::lines()`, which handles both.
- On Windows Anvil links with llvm-mingw + lld (targets
  `x86_64-pc-windows-gnullvm` / `-gnu`), not Visual Studio's `link.exe`.
  The linker is pre-configured by the shims; do not install MSVC.

## Debugging

`cargo build` then `lldb target/debug/myapp` (`b main`, `r`, `bt`, `p var`).
Set `RUST_BACKTRACE=1` to get a backtrace on panic.
