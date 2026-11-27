//! Output helpers. In `--json` mode stdout carries exactly one JSON document;
//! everything else (progress, prompts, warnings) goes to stderr.

use std::io::{IsTerminal, Write};

use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Result of a command, rendered by `main` according to the output mode.
pub struct Outcome {
    pub json: Option<Value>,
    pub human: String,
    pub exit_code: i32,
}

pub fn print_json(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer_pretty(&mut out, value);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

pub fn error_json(command: &str, err: &anyhow::Error) -> Value {
    json!({
        "ok": false,
        "command": command,
        "error": err.to_string(),
        "causes": causes(err),
    })
}

/// Underlying causes of `err`, skipping those already spelled out by the
/// previous message (many error types embed their source in `Display`).
pub fn causes(err: &anyhow::Error) -> Vec<String> {
    let mut out = Vec::new();
    let mut previous = err.to_string();
    for cause in err.chain().skip(1) {
        let text = cause.to_string();
        if !previous.contains(&text) {
            out.push(text.clone());
        }
        previous = text;
    }
    out
}

/// Ask for confirmation on stderr. Non-interactive sessions must pass `--yes`.
pub fn confirm(yes: bool, json: bool, question: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if json || !std::io::stdin().is_terminal() {
        bail!("confirmation required: re-run with --yes for non-interactive use ({question})");
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" | "s" | "sim" => Ok(()),
        _ => bail!("aborted by user"),
    }
}

pub fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}
