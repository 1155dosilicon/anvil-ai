//! Running child processes with captured output and a timeout.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Captured {
    /// `None` if the process was killed after the timeout.
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }

    pub fn timed_out(&self) -> bool {
        self.status.is_none()
    }

    /// First non-empty line of stdout, falling back to stderr.
    pub fn first_line(&self) -> Option<String> {
        self.stdout
            .lines()
            .chain(self.stderr.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    }

    /// Short human-readable failure description.
    pub fn describe_failure(&self) -> String {
        let status = match self.status {
            None => format!("timed out after {:.0}s", self.elapsed.as_secs_f64()),
            Some(s) => match s.code() {
                Some(c) => format!("exit code {c}"),
                None => "terminated by a signal".to_string(),
            },
        };
        let tail = tail_lines(
            if self.stderr.trim().is_empty() {
                &self.stdout
            } else {
                &self.stderr
            },
            8,
        );
        if tail.is_empty() {
            status
        } else {
            format!("{status}: {tail}")
        }
    }
}

/// Last `n` non-empty lines of `text`, joined with ` | `.
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// Kill `child` **and its descendants** (best effort), then the child itself.
///
/// * Windows: `taskkill /F /T /PID <pid>` walks the process tree (it must run
///   while the child is still alive, so it comes before `Child::kill`).
/// * Unix: children spawned by [`run_with_timeout`] lead their own process
///   group (`process_group(0)`), so `kill -KILL -- -<pid>` reaches the whole
///   group. Done through the `kill` utility: no `libc`, no `unsafe`.
///
/// Grandchildren matter: they inherit the stdout/stderr pipes, and a surviving
/// one would block the output readers long after the timeout.
pub fn kill_tree(child: &mut Child) -> io::Result<()> {
    let pid = child.id().to_string();
    let quiet = |cmd: &mut Command| {
        let _ = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    if cfg!(windows) {
        let system_taskkill = std::env::var_os("SystemRoot")
            .map(|root| std::path::Path::new(&root).join("System32").join("taskkill.exe"))
            .filter(|p| p.is_file());
        let mut taskkill = Command::new(system_taskkill.unwrap_or_else(|| "taskkill".into()));
        quiet(taskkill.args(["/F", "/T", "/PID", &pid]));
    } else {
        quiet(Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]));
    }
    match child.kill() {
        // Already exited (e.g. killed with its tree above).
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

#[cfg(unix)]
fn own_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_cmd: &mut Command) {}

/// Spawn `cmd` with stdin closed and stdout/stderr captured; kill its whole
/// process tree after `timeout` (see [`kill_tree`]).
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> io::Result<Captured> {
    let started = Instant::now();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    own_process_group(cmd);
    let mut child = cmd.spawn()?;

    let pump = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = pump(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = pump(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let deadline = started + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = kill_tree(&mut child);
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };

    Ok(Captured {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_last_lines() {
        assert_eq!(tail_lines("a\n\nb\nc\n", 2), "b | c");
        assert_eq!(tail_lines("", 3), "");
    }

    #[test]
    fn captures_output_of_a_real_process() {
        // `cargo` is guaranteed to exist while running `cargo test`.
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = run_with_timeout(Command::new(cargo).arg("--version"), Duration::from_secs(60)).unwrap();
        assert!(out.success(), "{}", out.describe_failure());
        assert!(out.first_line().unwrap().starts_with("cargo "));
    }

    /// The direct child spawns a long-lived grandchild that inherits the
    /// output pipes; without a tree kill the readers would block ~30 s.
    #[test]
    fn timeout_kills_the_whole_process_tree() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 & echo done"]);
            c
        } else {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", "sleep 30; echo done"]);
            c
        };
        let out = run_with_timeout(&mut cmd, Duration::from_millis(700)).unwrap();
        assert!(out.timed_out());
        assert!(!out.stdout.contains("done"));
        assert!(
            out.elapsed < Duration::from_secs(15),
            "process tree survived the timeout: {:?}",
            out.elapsed
        );
    }
}
