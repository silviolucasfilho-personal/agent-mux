//! The lease of a work item (spec §8.3): who is running it, held as a git
//! ref on the hub's remote so every machine sees it.
//!
//! This is `spec-wave-agent`'s protocol, ported unchanged from
//! `spec-wave-agent/src/lease.rs` (read on 2026-11-23) so the two are safe
//! on one repository at the same time. Do not change one rule without
//! reading the whole protocol:
//!
//! - **Acquire:** a push of the ref that is **not forced**. It fails
//!   atomically when the ref exists; losing the race is a normal answer.
//! - **Renew and steal:** `--force-with-lease=<ref>:<the sha seen>`, a
//!   compare-and-swap on the exact commit read.
//! - **`generation`** grows by one on every acquire and steal: the fencing
//!   token.
//! - **A lost lease** (`RenewError::Lost`) means stop at once: kill the
//!   run, and checkpoint, release and push nothing.
//!
//! The ref is `<prefix>/<issue>` (`refs/heads/spec-wave-agent/claims` by
//! default) and its commit holds one file, `lease.json`: `issue`, `owner`
//! (`<scm login>@<host>`), `generation`, `heartbeat` (RFC 3339, UTC). A
//! heartbeat every 120 s and a TTL of 600 s are the agent's.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The agent's namespace for claim refs.
pub const DEFAULT_PREFIX: &str = "refs/heads/spec-wave-agent/claims";
/// Seconds between heartbeats.
pub const HEARTBEAT_SECS: i64 = 120;
/// Seconds without a heartbeat after which a lease may be stolen.
pub const TTL_SECS: i64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub issue: u64,
    pub owner: String,
    /// The fencing token.
    pub generation: u64,
    /// RFC 3339, UTC, as chrono writes it for the agent.
    pub heartbeat: String,
}

impl Lease {
    /// The heartbeat in nanoseconds since the epoch; `None` when it does
    /// not parse.
    pub fn heartbeat_ns(&self) -> Option<i64> {
        let t = time::OffsetDateTime::parse(
            &self.heartbeat,
            &time::format_description::well_known::Rfc3339,
        )
        .ok()?;
        i64::try_from(t.unix_timestamp_nanos()).ok()
    }
}

fn rfc3339(now_ns: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(now_ns))
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// A lease expired when its last heartbeat is older than `ttl_secs`. The
/// comparison is strict, and a heartbeat in the future (clock skew) or one
/// that does not parse is not expired: in doubt, the holder keeps it.
pub fn is_expired(heartbeat_ns: Option<i64>, now_ns: i64, ttl_secs: i64) -> bool {
    match heartbeat_ns {
        Some(h) => now_ns - h > ttl_secs * 1_000_000_000,
        None => false,
    }
}

/// Why a renewal failed: the lease is gone (stop at once, never retry), or
/// the network failed (try again; the next try runs the compare-and-swap
/// and finds a lost lease deterministically).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewError {
    Lost(String),
    Transient(String),
}

impl std::fmt::Display for RenewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenewError::Lost(m) => write!(f, "the lease was lost: {m}"),
            RenewError::Transient(m) => write!(f, "the lease was not renewed: {m}"),
        }
    }
}

/// A prefix git accepts for refs: under `refs/`, with none of the
/// characters a ref name may not hold, and no slash at the end.
pub fn valid_prefix(p: &str) -> bool {
    p.starts_with("refs/")
        && p.len() > 5
        && !p.ends_with('/')
        && !p.contains("..")
        && !p.contains("//")
        && !p
            .chars()
            .any(|c| c.is_whitespace() || "~^:?*[\\".contains(c))
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<Out, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

fn git_ok(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let out = git(dir, args, stdin)?;
    if out.ok {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            out.stderr.trim()
        ))
    }
}

/// A small repository of its own that only holds claim refs: the
/// operator's checkout is never touched.
#[derive(Debug, Clone)]
pub struct LeaseRepo {
    dir: PathBuf,
    prefix: String,
}

impl LeaseRepo {
    /// Opens, creating it when it is not there, the repository under `dir`
    /// whose `origin` is `remote_url`.
    pub fn open(dir: PathBuf, remote_url: &str, prefix: &str) -> Result<LeaseRepo, String> {
        if !valid_prefix(prefix) {
            return Err(format!(
                "{prefix:?} is not a prefix for claim refs (expected refs/…)"
            ));
        }
        if !dir.join(".git").exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            git_ok(&dir, &["init", "--quiet"], None)?;
            git_ok(&dir, &["remote", "add", "origin", remote_url], None)?;
        } else {
            git_ok(&dir, &["remote", "set-url", "origin", remote_url], None)?;
        }
        // commit-tree needs an identity even with no worktree, and must not
        // depend on the machine's global configuration
        git_ok(&dir, &["config", "user.name", "agent-mux"], None)?;
        git_ok(&dir, &["config", "user.email", "agent-mux@localhost"], None)?;
        Ok(LeaseRepo {
            dir,
            prefix: prefix.to_string(),
        })
    }

    fn claim_ref(&self, issue: u64) -> String {
        format!("{}/{issue}", self.prefix)
    }

    fn read(&self, reference: &str, sha: &str) -> Result<Lease, String> {
        git_ok(
            &self.dir,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+{reference}:{reference}"),
            ],
            None,
        )?;
        let text = git_ok(&self.dir, &["show", &format!("{sha}:lease.json")], None)?;
        serde_json::from_str(text.trim()).map_err(|e| format!("lease.json is not valid: {e}"))
    }

    /// The lease on the remote now: the ref's commit and its content.
    pub fn fetch(&self, issue: u64) -> Result<Option<(String, Lease)>, String> {
        let r = self.claim_ref(issue);
        let ls = git(&self.dir, &["ls-remote", "origin", &r], None)?;
        if !ls.ok {
            return Err(format!("git ls-remote: {}", ls.stderr.trim()));
        }
        let sha = ls
            .stdout
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        Ok(Some((sha.clone(), self.read(&r, &sha)?)))
    }

    /// Every claim on the remote: what the rest of a team is running. A
    /// claim that cannot be read is left out.
    pub fn list(&self) -> Result<Vec<Lease>, String> {
        let pattern = format!("{}/*", self.prefix);
        let ls = git(&self.dir, &["ls-remote", "origin", &pattern], None)?;
        if !ls.ok {
            return Err(format!("git ls-remote: {}", ls.stderr.trim()));
        }
        let mut out = Vec::new();
        for line in ls.stdout.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(sha), Some(reference)) = (parts.next(), parts.next())
                && let Ok(lease) = self.read(reference, sha)
            {
                out.push(lease);
            }
        }
        out.sort_by_key(|l| l.issue);
        Ok(out)
    }

    /// A commit that holds only `lease.json`, made with plumbing: there is
    /// no worktree.
    fn commit(&self, lease: &Lease, parent: Option<&str>) -> Result<String, String> {
        let json = serde_json::to_string_pretty(lease).map_err(|e| e.to_string())?;
        let blob = git_ok(&self.dir, &["hash-object", "-w", "--stdin"], Some(&json))?;
        let tree = git_ok(
            &self.dir,
            &["mktree"],
            Some(&format!("100644 blob {blob}\tlease.json\n")),
        )?;
        let message = format!(
            "lease issue #{} gen {} by {}",
            lease.issue, lease.generation, lease.owner
        );
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
        if let Some(p) = parent {
            args.extend(["-p", p]);
        }
        git_ok(&self.dir, &args, None)
    }

    /// Tries to take the lease. `Some` when this owner holds it now; `None`
    /// when another holds it and still beats, or won the race.
    pub fn try_acquire(
        &self,
        issue: u64,
        owner: &str,
        ttl_secs: i64,
        now_ns: i64,
    ) -> Result<Option<Lease>, String> {
        let r = self.claim_ref(issue);
        match self.fetch(issue)? {
            None => {
                // no ref: a push that is not forced creates it or fails
                let lease = Lease {
                    issue,
                    owner: owner.to_string(),
                    generation: 1,
                    heartbeat: rfc3339(now_ns),
                };
                let sha = self.commit(&lease, None)?;
                let push = git(
                    &self.dir,
                    &["push", "--quiet", "origin", &format!("{sha}:{r}")],
                    None,
                )?;
                Ok(push.ok.then_some(lease))
            }
            Some((seen, current)) => {
                if !is_expired(current.heartbeat_ns(), now_ns, ttl_secs) {
                    return Ok(None);
                }
                // steal: only if the ref is still the stale commit that was read
                let lease = Lease {
                    issue,
                    owner: owner.to_string(),
                    generation: current.generation + 1,
                    heartbeat: rfc3339(now_ns),
                };
                let sha = self.commit(&lease, Some(&seen))?;
                let push = git(
                    &self.dir,
                    &[
                        "push",
                        "--quiet",
                        &format!("--force-with-lease={r}:{seen}"),
                        "origin",
                        &format!("{sha}:{r}"),
                    ],
                    None,
                )?;
                Ok(push.ok.then_some(lease))
            }
        }
    }

    /// Writes a new heartbeat by compare-and-swap.
    pub fn renew(&self, lease: &mut Lease, now_ns: i64) -> Result<(), RenewError> {
        let r = self.claim_ref(lease.issue);
        let (seen, current) = match self.fetch(lease.issue) {
            Err(e) => return Err(RenewError::Transient(e)),
            Ok(None) => return Err(RenewError::Lost("its ref is gone".into())),
            Ok(Some(v)) => v,
        };
        if current.owner != lease.owner || current.generation != lease.generation {
            return Err(RenewError::Lost(format!(
                "taken by {} (generation {})",
                current.owner, current.generation
            )));
        }
        let renewed = Lease {
            heartbeat: rfc3339(now_ns),
            ..lease.clone()
        };
        let sha = self
            .commit(&renewed, Some(&seen))
            .map_err(RenewError::Transient)?;
        let push = git(
            &self.dir,
            &[
                "push",
                "--quiet",
                &format!("--force-with-lease={r}:{seen}"),
                "origin",
                &format!("{sha}:{r}"),
            ],
            None,
        )
        .map_err(RenewError::Transient)?;
        if !push.ok {
            let said = format!("{}{}", push.stdout, push.stderr);
            // a refused compare-and-swap is a lost lease; anything else
            // (DNS, a timeout) may pass
            if said.contains("stale info")
                || said.contains("[rejected]")
                || said.contains("[remote rejected]")
            {
                return Err(RenewError::Lost(format!(
                    "the renewal was refused: {}",
                    said.trim()
                )));
            }
            return Err(RenewError::Transient(said.trim().to_string()));
        }
        lease.heartbeat = renewed.heartbeat;
        Ok(())
    }

    /// Deletes the claim. Best effort, as the agent's is.
    pub fn release(&self, issue: u64) {
        let r = self.claim_ref(issue);
        let _ = git(
            &self.dir,
            &["push", "--quiet", "origin", &format!(":{r}")],
            None,
        );
    }
}

/// `<scm login>@<host>`: who a lease says holds it.
pub fn owner(login: &str) -> String {
    let host = Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string());
    format!("{login}@{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    #[test]
    fn a_prefix_must_be_a_ref_namespace() {
        assert!(valid_prefix(DEFAULT_PREFIX));
        assert!(valid_prefix("refs/spec-wave/claims"));
        for bad in [
            "refs/",
            "heads/x",
            "refs/a/",
            "refs/a..b",
            "refs/a b",
            "refs/a:b",
            "refs/a*",
            "refs//a",
        ] {
            assert!(!valid_prefix(bad), "{bad}");
        }
    }

    #[test]
    fn expiry_is_strict_and_doubt_keeps_the_holder() {
        let now = 1_700_000_000 * S;
        // exactly at the TTL: not expired
        assert!(!is_expired(Some(now - 600 * S), now, 600));
        assert!(is_expired(Some(now - 601 * S), now, 600));
        // a heartbeat in the future (clock skew), or unreadable: not expired
        assert!(!is_expired(Some(now + 30 * S), now, 600));
        assert!(!is_expired(None, now, 600));
        assert!(is_expired(Some(now - S), now, 0));
        assert!(!is_expired(Some(now), now, 0));
    }

    #[test]
    fn the_heartbeat_is_the_agents_format() {
        // what chrono writes for `DateTime<Utc>`
        let l: Lease = serde_json::from_str(
            r#"{"issue":7,"owner":"ana@host","generation":3,"heartbeat":"2026-11-23T12:00:00.123456789Z"}"#,
        )
        .unwrap();
        let ns = l.heartbeat_ns().unwrap();
        assert_eq!(ns % S, 123_456_789);
        // and what this side writes is read back the same
        assert_eq!(rfc3339(ns), "2026-11-23T12:00:00.123456789Z");
        let json = serde_json::to_string_pretty(&l).unwrap();
        assert!(
            json.starts_with(
                "{\n  \"issue\": 7,\n  \"owner\": \"ana@host\",\n  \"generation\": 3,"
            ),
            "{json}"
        );
    }

    #[cfg(unix)]
    mod remote {
        use super::*;

        /// A bare remote and two lease repositories on it, as two machines.
        fn fx() -> (tempfile::TempDir, LeaseRepo, LeaseRepo) {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let origin = root.join("origin.git");
            std::fs::create_dir_all(&origin).unwrap();
            git_ok(&origin, &["init", "--bare", "--quiet", "."], None).unwrap();
            let url = origin.to_string_lossy().into_owned();
            let a = LeaseRepo::open(root.join("a"), &url, DEFAULT_PREFIX).unwrap();
            let b = LeaseRepo::open(root.join("b"), &url, DEFAULT_PREFIX).unwrap();
            (dir, a, b)
        }

        #[test]
        fn one_owner_holds_an_item_until_it_stops_beating() {
            let (_dir, a, b) = fx();
            let t0 = 1_700_000_000 * S;
            let mut mine = a.try_acquire(7, "ana@a", TTL_SECS, t0).unwrap().unwrap();
            assert_eq!((mine.generation, mine.owner.as_str()), (1, "ana@a"));
            // the other machine sees it held, and does not get it
            assert!(
                b.try_acquire(7, "bia@b", TTL_SECS, t0 + 60 * S)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(b.list().unwrap(), vec![mine.clone()]);
            // another item is free
            assert!(b.try_acquire(8, "bia@b", TTL_SECS, t0).unwrap().is_some());

            // a heartbeat keeps it past the first TTL
            a.renew(&mut mine, t0 + 500 * S).unwrap();
            assert!(
                b.try_acquire(7, "bia@b", TTL_SECS, t0 + 700 * S)
                    .unwrap()
                    .is_none()
            );

            // no heartbeat for more than the TTL: it is stolen, one generation up
            let stolen = b
                .try_acquire(7, "bia@b", TTL_SECS, t0 + 500 * S + 601 * S)
                .unwrap()
                .unwrap();
            assert_eq!((stolen.generation, stolen.owner.as_str()), (2, "bia@b"));
            // the first owner's next renewal finds it lost: fencing
            let e = a.renew(&mut mine, t0 + 1200 * S).unwrap_err();
            assert!(
                matches!(&e, RenewError::Lost(m) if m.contains("bia@b")),
                "{e}"
            );

            // released, the ref is gone and a renewal is lost too
            b.release(7);
            assert!(a.fetch(7).unwrap().is_none());
            let mut gone = stolen.clone();
            assert!(matches!(b.renew(&mut gone, t0), Err(RenewError::Lost(_))));
            // and anyone may take it again, from generation 1
            let again = a.try_acquire(7, "ana@a", TTL_SECS, t0).unwrap().unwrap();
            assert_eq!(again.generation, 1);
        }
    }
}
