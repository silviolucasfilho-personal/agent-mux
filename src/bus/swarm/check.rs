//! The swarm's final check (§6 of
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`): an
//! optional command (`swarms.check_cmd`, e.g. `cargo test`) agent-mux runs
//! in the workspace once every task is settled and every branch reviewed,
//! before the swarm reads `finished`. It is a shell command, not a model
//! run. Its outcome is stored as JSON in `swarms.check_result`: first a
//! `running` marker, then the exit status and the end of the output.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A check that runs longer than this is stopped and counts as failed.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How many lines of the output are kept.
pub const TAIL_LINES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Still running (started at `started_ns`).
    #[serde(default)]
    pub running: bool,
    pub started_ns: i64,
    /// The exit code, `None` while running or when it was killed.
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub passed: bool,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub secs: u64,
    /// The last [`TAIL_LINES`] lines of stdout and stderr together.
    #[serde(default)]
    pub tail: String,
}

impl CheckResult {
    pub fn started(now_ns: i64) -> CheckResult {
        CheckResult {
            running: true,
            started_ns: now_ns,
            exit: None,
            passed: false,
            timed_out: false,
            secs: 0,
            tail: String::new(),
        }
    }

    pub fn parse(json: &str) -> Option<CheckResult> {
        serde_json::from_str(json).ok()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A running marker left behind by a TUI that went away: run again.
    pub fn abandoned(&self, now_ns: i64) -> bool {
        self.running
            && now_ns.saturating_sub(self.started_ns)
                > (CHECK_TIMEOUT + Duration::from_secs(60)).as_nanos() as i64
    }

    /// One line for the screen and the CLI.
    pub fn summary(&self, cmd: &str) -> String {
        if self.running {
            return format!("check running: {cmd}");
        }
        if self.passed {
            return format!("check passed: {cmd} ({}s)", self.secs);
        }
        if self.timed_out {
            return format!("check timed out after {}s: {cmd}", self.secs);
        }
        match self.exit {
            Some(c) => format!("check failed: {cmd} exited {c} ({}s)", self.secs),
            None => format!("check failed: {cmd} did not run"),
        }
    }
}

/// Runs `cmd` with `sh -c` in `workspace`, stdout and stderr together,
/// stopped after `timeout`.
pub fn run(workspace: &Path, cmd: &str, timeout: Duration, started_ns: i64) -> CheckResult {
    let t0 = Instant::now();
    let mut result = CheckResult {
        running: false,
        ..CheckResult::started(started_ns)
    };
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1; {cmd}"))
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            result.tail = format!("could not start `sh`: {e}");
            return result;
        }
    };
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if t0.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                result.timed_out = true;
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let out = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&out);
    let lines: Vec<&str> = text.lines().collect();
    result.tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    result.secs = t0.elapsed().as_secs();
    result.exit = status.and_then(|s| s.code());
    result.passed = result.exit == Some(0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_reports_its_exit_and_the_end_of_its_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "here\n").unwrap();
        let ok = run(
            dir.path(),
            "cat marker; echo done",
            Duration::from_secs(10),
            1,
        );
        assert!(ok.passed && !ok.running, "{ok:?}");
        assert_eq!(ok.tail, "here\ndone");
        let bad = run(
            dir.path(),
            "echo oops >&2; exit 3",
            Duration::from_secs(10),
            1,
        );
        assert_eq!((bad.passed, bad.exit), (false, Some(3)));
        assert_eq!(bad.tail, "oops", "stderr is kept");
        assert_eq!(
            bad.summary("make"),
            format!("check failed: make exited 3 ({}s)", bad.secs)
        );
        let slow = run(dir.path(), "sleep 5", Duration::from_millis(300), 1);
        assert!(slow.timed_out && !slow.passed, "{slow:?}");
        let many = run(
            dir.path(),
            "i=0; while [ $i -lt 100 ]; do echo $i; i=$((i+1)); done",
            Duration::from_secs(10),
            1,
        );
        assert_eq!(many.tail.lines().count(), TAIL_LINES);
        assert!(many.tail.ends_with("99"));
    }

    #[test]
    fn a_running_marker_round_trips_and_is_abandoned_after_the_timeout() {
        let m = CheckResult::started(0);
        assert_eq!(CheckResult::parse(&m.to_json()), Some(m.clone()));
        assert!(!m.abandoned(1_000));
        assert!(m.abandoned(CHECK_TIMEOUT.as_nanos() as i64 * 2));
        assert_eq!(m.summary("cargo test"), "check running: cargo test");
    }
}
