//! Running child processes with captured output and a timeout.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
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
/// Uses `taskkill /F /T /PID <pid>` to walk the Windows process tree (it must
/// run while the child is still alive, so it comes before `Child::kill`).
///
/// Grandchildren matter: they inherit the stdout/stderr pipes, and a surviving
/// one would block the output readers long after the timeout.
pub fn kill_tree(child: &mut Child) -> io::Result<()> {
    let pid = child.id().to_string();
    let system_taskkill = std::env::var_os("SystemRoot")
        .map(|root| std::path::Path::new(&root).join("System32").join("taskkill.exe"))
        .filter(|p| p.is_file());
    let mut taskkill = Command::new(system_taskkill.unwrap_or_else(|| "taskkill".into()));
    let _ = taskkill
        .args(["/F", "/T", "/PID", &pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match child.kill() {
        // Already exited (e.g. killed with its tree above).
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

/// How long the output readers may keep running once the child has exited
/// (or been killed). A detached grandchild that inherited the pipes would
/// otherwise keep them open – and block the caller – indefinitely.
const READER_GRACE: Duration = Duration::from_secs(5);

type SharedBuf = Arc<Mutex<Vec<u8>>>;

/// Drain `pipe` on a background thread into a shared buffer; `done` fires at EOF.
fn pump(pipe: Option<Box<dyn Read + Send>>, done: mpsc::Sender<()>) -> SharedBuf {
    let buf: SharedBuf = Arc::default();
    let sink = Arc::clone(&buf);
    thread::spawn(move || {
        if let Some(mut p) = pipe {
            let mut chunk = [0u8; 8192];
            loop {
                match p.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => sink
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = done.send(());
    });
    buf
}

fn snapshot(buf: &SharedBuf) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
}

/// Spawn `cmd` with stdin closed and stdout/stderr captured; kill its whole
/// process tree after `timeout` (see [`kill_tree`]).
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> io::Result<Captured> {
    let started = Instant::now();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let (done_tx, done_rx) = mpsc::channel();
    let out = pump(
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        done_tx.clone(),
    );
    let err = pump(
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        done_tx,
    );

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

    // Both readers normally hit EOF right away; never wait on them forever.
    let readers_deadline = Instant::now() + READER_GRACE;
    for _ in 0..2 {
        let left = readers_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }

    Ok(Captured {
        status,
        stdout: snapshot(&out),
        stderr: snapshot(&err),
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
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "ping -n 30 127.0.0.1 & echo done"]);
        let out = run_with_timeout(&mut cmd, Duration::from_millis(700)).unwrap();
        assert!(out.timed_out());
        assert!(!out.stdout.contains("done"));
        assert!(
            out.elapsed < Duration::from_secs(15),
            "process tree survived the timeout: {:?}",
            out.elapsed
        );
    }

    /// The child exits at once but leaves a background grandchild holding the
    /// output pipes: the call must return shortly, with the output so far.
    #[test]
    fn lingering_grandchild_does_not_block_after_exit() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "echo started & start /B ping -n 30 127.0.0.1 >NUL"]);
        let out = run_with_timeout(&mut cmd, Duration::from_secs(60)).unwrap();
        assert!(out.success(), "{}", out.describe_failure());
        assert!(out.stdout.contains("started"), "{:?}", out.stdout);
        assert!(
            out.elapsed < Duration::from_secs(15),
            "blocked on inherited pipes: {:?}",
            out.elapsed
        );
    }
}
