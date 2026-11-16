//! Unattended intake: open issues that carry the factory's label become
//! work items, when the login that added the label may triage the
//! repository. Rust only, no session: `gh` lists the issues, reads who
//! labeled each one and that login's permission.
//!
//! - A login below `triage` gets no item and nothing is recorded.
//! - A `gh` call that fails inserts nothing for that issue.
//! - An issue that already has an item is skipped by the store's claim.
//!
//! Also here: the files under `<runtime>/factory/` that say which process
//! works an item, so another process can tell a running item from one
//! whose process is gone, and stop it.

use super::deliver::{Issue, Tools};
use super::{Item, Source, store};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gh(tools: &Tools, workspace: &Path, args: &[&str], what: &str) -> Result<String, String> {
    let out = Command::new(&tools.gh)
        .args(args)
        .current_dir(workspace)
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{what}: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// Open issues that carry `label`, oldest first.
pub fn labeled_issues(tools: &Tools, workspace: &Path, label: &str) -> Result<Vec<Issue>, String> {
    let out = gh(
        tools,
        workspace,
        &[
            "issue",
            "list",
            "--label",
            label,
            "--state",
            "open",
            "--limit",
            "50",
            "--json",
            "number,title,body,author,state",
        ],
        "gh issue list",
    )?;
    let v: Value =
        serde_json::from_str(&out).map_err(|e| format!("gh issue list: not JSON: {e}"))?;
    let text = |i: &Value, k: &str| {
        i.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut issues: Vec<(i64, Issue)> = v
        .as_array()
        .ok_or("gh issue list: not a list")?
        .iter()
        .filter_map(|i| {
            let n = i.get("number")?.as_i64()?;
            Some((
                n,
                Issue {
                    number: n.to_string(),
                    title: text(i, "title"),
                    body: text(i, "body"),
                    author: i
                        .get("author")
                        .and_then(|a| a.get("login"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    state: text(i, "state"),
                },
            ))
        })
        .collect();
    issues.sort_by_key(|(n, _)| *n);
    Ok(issues.into_iter().map(|(_, i)| i).collect())
}

/// The login that last added `label` to issue `number`, from the issue's
/// events. `None` when no event says so.
pub fn labeler(
    tools: &Tools,
    workspace: &Path,
    repo: &str,
    number: &str,
    label: &str,
) -> Result<Option<String>, String> {
    let out = gh(
        tools,
        workspace,
        &[
            "api",
            "--paginate",
            &format!("repos/{repo}/issues/{number}/events"),
            "--jq",
            r#".[] | select(.event == "labeled") | [.label.name, .actor.login] | @tsv"#,
        ],
        "gh api (issue events)",
    )?;
    Ok(last_labeler(&out, label))
}

/// The last `<label>\t<login>` line of `events` for `label`. GitHub's
/// label names do not differ by case.
pub fn last_labeler(events: &str, label: &str) -> Option<String> {
    events
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(name, login)| name.trim().eq_ignore_ascii_case(label) && !login.trim().is_empty())
        .map(|(_, login)| login.trim().to_string())
        .next_back()
}

/// `login`'s role on `repo`: `admin`, `maintain`, `write`, `triage`,
/// `read` or `none`.
pub fn permission(
    tools: &Tools,
    workspace: &Path,
    repo: &str,
    login: &str,
) -> Result<String, String> {
    gh(
        tools,
        workspace,
        &[
            "api",
            &format!("repos/{repo}/collaborators/{login}/permission"),
            "--jq",
            ".role_name // .permission",
        ],
        "gh api (permission)",
    )
    .map(|r| r.trim().to_ascii_lowercase())
}

/// Triage or above may hand the factory work.
pub fn may_request(role: &str) -> bool {
    matches!(role, "admin" | "maintain" | "write" | "triage")
}

/// What the poll did with one labeled issue.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    /// A new item, queued.
    New(Box<Item>),
    /// The issue already has an item.
    Known { number: String, item: String },
    /// Labeled by a login that may not triage the repository, or by none
    /// the events name.
    Refused {
        number: String,
        login: Option<String>,
        role: String,
    },
    /// A `gh` call failed: nothing was inserted.
    Error { number: String, why: String },
}

/// Reads the labeled issues of `workspace`'s repository and inserts an
/// item for each one a triager labeled. With `dry_run` nothing is written
/// and `New` holds the item that would be.
pub fn poll(
    conn: &rusqlite::Connection,
    tools: &Tools,
    workspace: &Path,
    repo: &str,
    label: &str,
    dry_run: bool,
) -> Result<Vec<Seen>, String> {
    let mut out = Vec::new();
    for issue in labeled_issues(tools, workspace, label)? {
        let number = issue.number.clone();
        match store::resolve(conn, &format!("#{number}"), repo) {
            Ok(Some(it)) => {
                out.push(Seen::Known {
                    number,
                    item: it.id,
                });
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                out.push(Seen::Error {
                    number,
                    why: e.to_string(),
                });
                continue;
            }
        }
        let login = match labeler(tools, workspace, repo, &number, label) {
            Ok(l) => l,
            Err(why) => {
                out.push(Seen::Error { number, why });
                continue;
            }
        };
        let Some(login) = login else {
            out.push(Seen::Refused {
                number,
                login: None,
                role: "unknown".into(),
            });
            continue;
        };
        let role = match permission(tools, workspace, repo, &login) {
            Ok(r) => r,
            Err(why) => {
                out.push(Seen::Error { number, why });
                continue;
            }
        };
        if !may_request(&role) {
            out.push(Seen::Refused {
                number,
                login: Some(login),
                role,
            });
            continue;
        }
        let mut item = Item::new(workspace, repo, Source::Issue, &number, &issue.title);
        item.body = issue.body;
        item.requester = issue.author;
        item.attended = false;
        if dry_run {
            out.push(Seen::New(Box::new(item)));
            continue;
        }
        match store::insert(conn, &item) {
            Ok(store::Inserted::New) => {
                let _ = store::add_event(
                    conn,
                    &item.id,
                    "created",
                    serde_json::json!({ "source": "label", "label": label, "labeled_by": login, "role": role }),
                );
                out.push(Seen::New(Box::new(item)));
            }
            Ok(store::Inserted::Claimed(it)) => out.push(Seen::Known {
                number,
                item: it.id,
            }),
            Err(e) => out.push(Seen::Error {
                number,
                why: e.to_string(),
            }),
        }
    }
    Ok(out)
}

// ---- which process works what ----------------------------------------------

/// `<runtime>/factory/`.
pub fn dir(runtime: &Path) -> PathBuf {
    runtime.join("factory")
}

fn pid_file(runtime: &Path, name: &str) -> PathBuf {
    dir(runtime).join(format!("{name}.pid"))
}

/// Whether process `pid` exists.
pub fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The process recorded under `name`, when it is still there.
pub fn holder(runtime: &Path, name: &str) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file(runtime, name))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    alive(pid).then_some(pid)
}

/// A pid file held for as long as the value lives.
pub struct Held {
    file: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

/// Records this process under `name` (an item id, or `poll-<repo key>`).
/// `None` when another live process holds it.
pub fn hold(runtime: &Path, name: &str) -> Option<Held> {
    if holder(runtime, name).is_some_and(|pid| pid != std::process::id()) {
        return None;
    }
    let file = pid_file(runtime, name);
    std::fs::create_dir_all(dir(runtime)).ok()?;
    std::fs::write(&file, format!("{}\n", std::process::id())).ok()?;
    Some(Held { file })
}

/// Asks the process that works `name` to stop. True when one was there.
pub fn stop(runtime: &Path, name: &str) -> bool {
    let Some(pid) = holder(runtime, name) else {
        return false;
    };
    Command::new("kill")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|s| s.success())
}

/// Stops the process that works `item`, when there is one, and marks the
/// item cancelled. True when a process was told to stop.
pub fn cancel(conn: &rusqlite::Connection, runtime: &Path, item: &mut Item) -> bool {
    let stopped = stop(runtime, &item.id);
    item.state = super::State::Cancelled;
    let _ = store::update(conn, item);
    let _ = store::add_event(conn, &item.id, "state.cancelled", Value::Null);
    stopped
}

/// Starts `agent-mux factory <args>` as a process of its own, on the same
/// store and runtime folder, with its output in
/// `<runtime>/factory/<name>.log`. The TUI starts items this way: a run
/// is headless in its own process and outlives the screen.
pub fn spawn_cli(
    exe: &Path,
    runtime: &Path,
    db: Option<&Path>,
    name: &str,
    args: &[String],
) -> Result<PathBuf, String> {
    let dir = dir(runtime);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let log = dir.join(format!("{name}.log"));
    let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("factory")
        .args(args)
        .env("AGENT_MUX_RUNTIME_DIR", runtime)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    if let Some(db) = db {
        cmd.env("AGENT_MUX_TRACE_DB", db);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    // reaped when it ends, so it does not stay a zombie of the TUI
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_event_for_the_label_names_who_asked() {
        let events = "bug\talice\nfactory\tmallory\nhelp wanted\tbob\nFactory\tcarol\n";
        assert_eq!(last_labeler(events, "factory").as_deref(), Some("carol"));
        assert_eq!(last_labeler(events, "bug").as_deref(), Some("alice"));
        assert_eq!(last_labeler(events, "other"), None);
        assert_eq!(last_labeler("", "factory"), None);
        // an event whose actor is gone names nobody
        assert_eq!(last_labeler("factory\t\n", "factory"), None);
    }

    #[test]
    fn triage_and_above_may_ask() {
        for role in ["admin", "maintain", "write", "triage"] {
            assert!(may_request(role), "{role}");
        }
        for role in ["read", "none", "", "unknown"] {
            assert!(!may_request(role), "{role}");
        }
    }

    #[test]
    fn a_held_name_is_refused_to_nobody_else_alive_and_freed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let held = hold(tmp.path(), "wi_1").expect("first hold");
        assert_eq!(holder(tmp.path(), "wi_1"), Some(std::process::id()));
        // the same process may take it again (a retry in one process)
        assert!(hold(tmp.path(), "wi_1").is_some());
        drop(held);
        assert_eq!(holder(tmp.path(), "wi_1"), None);
        // a pid file left by a dead process does not hold
        std::fs::create_dir_all(dir(tmp.path())).unwrap();
        std::fs::write(pid_file(tmp.path(), "wi_2"), "999999999\n").unwrap();
        assert_eq!(holder(tmp.path(), "wi_2"), None);
        assert!(hold(tmp.path(), "wi_2").is_some());
    }
}
