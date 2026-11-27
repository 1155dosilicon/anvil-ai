//! Progress reporting for long-running operations.
//!
//! Reporters must never write to stdout: in `--json` mode stdout is reserved
//! for the final JSON document. Use [`SilentProgress`] there.

use std::io::{IsTerminal, Write};
use std::time::Instant;

/// Receives progress events. All methods have no-op defaults.
pub trait Progress {
    /// A new unit of work starts; `total_bytes` may be 0 when unknown.
    fn begin(&mut self, _label: &str, _total_bytes: u64) {}
    /// `done_bytes` of the current unit have been processed.
    fn advance(&mut self, _done_bytes: u64) {}
    /// Free-form status line.
    fn message(&mut self, _msg: &str) {}
    /// Current unit of work is complete.
    fn finish(&mut self, _label: &str) {}
}

/// Reports nothing (used with `--json`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentProgress;

impl Progress for SilentProgress {}

/// Human-readable progress on **stderr**.
#[derive(Debug)]
pub struct StderrProgress {
    label: String,
    total: u64,
    last_step: u64,
    started: Instant,
    tty: bool,
}

impl Default for StderrProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl StderrProgress {
    pub fn new() -> Self {
        Self {
            label: String::new(),
            total: 0,
            last_step: u64::MAX,
            started: Instant::now(),
            tty: std::io::stderr().is_terminal(),
        }
    }
}

const MIB: f64 = 1024.0 * 1024.0;

impl Progress for StderrProgress {
    fn begin(&mut self, label: &str, total_bytes: u64) {
        self.label = label.to_string();
        self.total = total_bytes;
        self.last_step = u64::MAX;
        self.started = Instant::now();
        eprintln!("==> {label} ({:.1} MiB)", total_bytes as f64 / MIB);
    }

    fn advance(&mut self, done: u64) {
        if self.total == 0 {
            return;
        }
        let pct = (done.min(self.total) * 100) / self.total;
        // Redraw every percent on a terminal, every 10% when redirected to a log.
        let step = if self.tty { pct } else { pct / 10 * 10 };
        if step == self.last_step {
            return;
        }
        self.last_step = step;
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = done as f64 / MIB / secs;
        let line = format!(
            "    {:>3}%  {:>8.1} / {:.1} MiB  {:>6.1} MiB/s",
            step,
            done as f64 / MIB,
            self.total as f64 / MIB,
            rate
        );
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r{line}");
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }

    fn message(&mut self, msg: &str) {
        eprintln!("{msg}");
    }

    fn finish(&mut self, label: &str) {
        if self.tty && self.total > 0 {
            eprintln!();
        }
        eprintln!("    {label}: done in {:.1}s", self.started.elapsed().as_secs_f64());
    }
}
