//! The merge gate (schema v26): with a check command, a finished branch is
//! merged only once the check passes on the branch brought up to date with
//! the workspace (a light, serial version of a merge queue). `merge` queues
//! the branch (`swarm_merge_gates`); the engine starts one gate at a time
//! per swarm (`engine::Action::RunGate`), since each tests on top of the
//! merges before it; the TUI runs it on a thread:
//!
//! 1. the workspace's `HEAD` is the base;
//! 2. in the task's worktree (a temporary one when it is gone) the base is
//!    merged into the branch: a conflict fails the gate, nothing merged;
//! 3. the check runs there;
//! 4. passed, and the workspace still at the base: the branch is merged
//!    into the workspace (`merge_branch`, clean since it holds the base) and
//!    recorded as merged; the workspace moved meanwhile: queued again.
//!
//! `force-merge` merges without the gate, on the operator's word.

use super::check::{self, CheckResult};
use super::record::{self, BoardTask, SwarmRun};
use crate::bus::store as bstore;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Whether a swarm gates its merges, and on what (`swarm_merge_settings`).
/// Off unless the operator turns it on: a check command alone runs only at
/// the end, as it did before the gate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeGateSettings {
    pub enabled: bool,
    /// The gate's own command; `None`: the swarm's check command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl MergeGateSettings {
    pub fn off() -> Self {
        Self::default()
    }

    /// `off` (or empty), `on` (the swarm's check command), or a command of
    /// its own.
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "" | "off" | "no" | "none" | "false" => Self::off(),
            "on" | "yes" | "true" | "check" => MergeGateSettings {
                enabled: true,
                command: None,
            },
            _ => MergeGateSettings {
                enabled: true,
                command: Some(t.to_string()),
            },
        }
    }

    /// The setting as [`MergeGateSettings::parse`] reads it back.
    pub fn as_choice(&self) -> String {
        match (self.enabled, &self.command) {
            (false, _) => "off".into(),
            (true, None) => "on".into(),
            (true, Some(c)) => c.clone(),
        }
    }

    /// The command a gate runs for `swarm`, when its merges are gated.
    pub fn command_for(&self, swarm: &SwarmRun) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.command
            .clone()
            .or_else(|| swarm.check_cmd.clone())
            .filter(|c| !c.trim().is_empty())
    }

    /// One line for the coordinator's context and the screen.
    pub fn describe(&self, swarm: &SwarmRun) -> String {
        match (self.enabled, self.command_for(swarm)) {
            (false, _) => "off: merge merges at once".into(),
            (true, Some(c)) => format!(
                "on: merge queues the branch and merges it once `{c}` passes on it brought up to date with the workspace"
            ),
            (true, None) => "on, but the swarm has no check command: merge merges at once".into(),
        }
    }

    /// Refused when it is on without a command to run.
    pub fn check(&self, check_cmd: Option<&str>) -> Result<(), String> {
        if self.enabled && self.command.is_none() && check_cmd.is_none_or(|c| c.trim().is_empty()) {
            return Err(
                "merge gate: `on` runs the swarm's check command, and there is none: give a check command, or the gate's own command".into(),
            );
        }
        Ok(())
    }
}

/// A swarm's merge gate setting; no row: off.
pub fn settings(conn: &Connection, swarm_id: &str) -> MergeGateSettings {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT mode, command FROM swarm_merge_settings WHERE swarm_id = ?1",
        params![swarm_id],
        |r| {
            Ok(MergeGateSettings {
                enabled: r.get::<_, String>(0)? == "on",
                command: r
                    .get::<_, Option<String>>(1)?
                    .filter(|c| !c.trim().is_empty()),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or_default()
}

pub fn set_settings(
    conn: &Connection,
    swarm_id: &str,
    s: &MergeGateSettings,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_merge_settings (swarm_id, mode, command, updated_ns)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (swarm_id) DO UPDATE SET mode = excluded.mode, command = excluded.command,
             updated_ns = excluded.updated_ns",
        params![
            swarm_id,
            if s.enabled { "on" } else { "off" },
            s.command,
            now
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateState {
    Queued,
    Running,
    /// The check passed and the branch is merged.
    Passed,
    Failed,
}

impl GateState {
    pub fn as_str(self) -> &'static str {
        match self {
            GateState::Queued => "queued",
            GateState::Running => "running",
            GateState::Passed => "passed",
            GateState::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<GateState> {
        Some(match s {
            "queued" => GateState::Queued,
            "running" => GateState::Running,
            "passed" => GateState::Passed,
            "failed" => GateState::Failed,
            _ => return None,
        })
    }
    pub fn glyph(self) -> &'static str {
        match self {
            GateState::Queued => "⧗",
            GateState::Running => "⟳",
            GateState::Passed => "✓",
            GateState::Failed => "✗",
        }
    }
}

/// A branch's merge behind the check (`swarm_merge_gates`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeGate {
    pub state: GateState,
    pub queued_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
    /// The workspace commit the branch was tested on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// One line: the merge commit, or why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The check's run, once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CheckResult>,
}

impl MergeGate {
    /// A running gate whose thread is gone (agent-mux stopped): it runs again.
    pub fn abandoned(&self, now_ns: i64) -> bool {
        self.state == GateState::Running
            && self.started_ns.is_none_or(|s| {
                now_ns - s > check::CHECK_TIMEOUT.as_nanos() as i64 + 5 * 60 * 1_000_000_000
            })
    }
    /// What the board and the coordinator say about it.
    pub fn summary(&self, cmd: Option<&str>) -> String {
        let cmd = cmd
            .map(|c| format!("`{c}`"))
            .unwrap_or_else(|| "the check".into());
        match self.state {
            GateState::Queued => format!("merge queued behind {cmd}"),
            GateState::Running => format!("{cmd} running on the branch brought up to date"),
            GateState::Passed => format!(
                "{cmd} passed; merged{}",
                self.detail
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ),
            GateState::Failed => format!(
                "not merged: {}",
                self.detail.as_deref().unwrap_or("the check failed")
            ),
        }
    }
}

/// Queues `task_id`'s branch (again): its earlier run is forgotten.
pub fn queue(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_merge_gates (task_id, state, queued_ns, updated_ns)
         VALUES (?1, 'queued', ?2, ?2)
         ON CONFLICT (task_id) DO UPDATE SET state = 'queued', queued_ns = excluded.queued_ns,
             started_ns = NULL, ended_ns = NULL, base = NULL, detail = NULL, result = NULL,
             updated_ns = excluded.updated_ns",
        params![task_id, now],
    )?;
    record::poke(db);
    Ok(())
}

/// The gate is off now: a merge still queued waits for the operator again.
pub fn unqueue(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM swarm_merge_gates WHERE task_id = ?1 AND state = 'queued'",
        params![task_id],
    )?;
    Ok(())
}

/// The engine started the gate.
pub fn start(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_merge_gates SET state = 'running', started_ns = ?1, updated_ns = ?1
         WHERE task_id = ?2",
        params![now, task_id],
    )?;
    record::poke(db);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
    state: GateState,
    base: Option<&str>,
    detail: &str,
    result: Option<&CheckResult>,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_merge_gates SET state = ?1, ended_ns = ?2, base = ?3, detail = ?4,
             result = ?5, updated_ns = ?2
         WHERE task_id = ?6",
        params![
            state.as_str(),
            now,
            base,
            detail,
            result.map(CheckResult::to_json),
            task_id
        ],
    )?;
    record::poke(db);
    Ok(())
}

pub fn get(conn: &Connection, task_id: &str) -> rusqlite::Result<Option<MergeGate>> {
    conn.query_row(
        "SELECT state, queued_ns, started_ns, ended_ns, base, detail, result
         FROM swarm_merge_gates WHERE task_id = ?1",
        params![task_id],
        |r| row(r, 0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        e => Err(e),
    })
}

/// A gate from the columns `state, queued_ns, started_ns, ended_ns, base,
/// detail, result` starting at `at` (the board query's too).
pub fn row(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<MergeGate> {
    let state: String = r.get(at)?;
    Ok(MergeGate {
        state: GateState::parse(&state).unwrap_or(GateState::Failed),
        queued_ns: r.get(at + 1)?,
        started_ns: r.get(at + 2)?,
        ended_ns: r.get(at + 3)?,
        base: r.get(at + 4)?,
        detail: r.get(at + 5)?,
        result: r
            .get::<_, Option<String>>(at + 6)?
            .as_deref()
            .and_then(CheckResult::parse),
    })
}

/// What testing a branch came to (before anything is merged).
#[derive(Debug, Clone, PartialEq)]
pub enum Tested {
    /// The check passed on the branch with `base` merged into it.
    Passed { base: String, result: CheckResult },
    /// Not mergeable: `why`, with the check's run when it got that far.
    Failed {
        base: Option<String>,
        why: String,
        result: Option<CheckResult>,
    },
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(dir, args)?;
    if !out.status.success() {
        return Err(first_error(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn first_error(out: &std::process::Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| l.starts_with("CONFLICT") || l.starts_with("error:") || l.starts_with("fatal:"))
        .or_else(|| lines.first())
        .map(|l| l.to_string())
        .unwrap_or_else(|| "git failed".into())
}

/// The workspace's current commit.
pub fn head(workspace: &Path) -> Result<String, String> {
    git_text(workspace, &["rev-parse", "HEAD"])
}

/// Tests `branch` for a merge into `workspace`: its worktree (`worktree`
/// when it is a folder, else a temporary one, removed after) is brought up
/// to date with the workspace's `HEAD`, then `cmd` runs there. Nothing is
/// merged into the workspace.
pub fn test_branch(
    workspace: &Path,
    worktree: Option<&Path>,
    branch: &str,
    cmd: &str,
    timeout: Duration,
    now_ns: i64,
) -> Tested {
    let fail = |base: Option<&str>, why: String| Tested::Failed {
        base: base.map(str::to_string),
        why,
        result: None,
    };
    let base = match head(workspace) {
        Ok(b) => b,
        Err(e) => {
            return fail(
                None,
                format!("the workspace has no commit to merge into: {e}"),
            );
        }
    };
    let mut temp: Option<PathBuf> = None;
    let dir = match worktree.filter(|w| w.is_dir()) {
        Some(w) => w.to_path_buf(),
        None => {
            let short: String = branch
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let p = workspace
                .join(super::WORKTREES_DIR)
                .join(format!("merge-gate-{short}"));
            let _ = git(
                workspace,
                &["worktree", "remove", "--force", &p.to_string_lossy()],
            );
            match git(
                workspace,
                &["worktree", "add", &p.to_string_lossy(), branch],
            ) {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    return fail(
                        Some(&base),
                        format!("could not check out {branch}: {}", first_error(&o)),
                    );
                }
                Err(e) => return fail(Some(&base), e),
            }
            temp = Some(p.clone());
            p
        }
    };
    let tested = (|| {
        // not trimmed: each line starts with its two status columns
        let dirty = git(&dir, &["status", "--porcelain", "--untracked-files=no"])
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if !dirty.trim().is_empty() {
            let files: Vec<&str> = dirty
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(5)
                .map(|l| l.get(3..).unwrap_or(l))
                .collect();
            return fail(
                Some(&base),
                format!(
                    "its worktree has uncommitted changes ({}): the worker did not commit everything",
                    files.join(", ")
                ),
            );
        }
        let merged = match git(&dir, &["merge", "--no-edit", &base]) {
            Ok(o) => o,
            Err(e) => return fail(Some(&base), e),
        };
        if !merged.status.success() {
            let why = first_error(&merged);
            let _ = git(&dir, &["merge", "--abort"]);
            let short = base.get(..10).unwrap_or(&base);
            return fail(
                Some(&base),
                format!("{branch} does not merge cleanly with the workspace ({short}): {why}"),
            );
        }
        let result = check::run(&dir, cmd, timeout, now_ns);
        if result.passed {
            Tested::Passed {
                base: base.clone(),
                result,
            }
        } else {
            Tested::Failed {
                base: Some(base.clone()),
                why: result.summary(cmd),
                result: Some(result),
            }
        }
    })();
    if let Some(p) = temp {
        let _ = git(
            workspace,
            &["worktree", "remove", "--force", &p.to_string_lossy()],
        );
    }
    tested
}

/// What [`land`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    Merged(String),
    Failed(String),
    /// The workspace moved while the check ran: queued again.
    Requeued,
    /// The operator decided the branch meanwhile (set aside, force-merged).
    Decided,
}

/// Records the gate's outcome: on a pass with the workspace still at the
/// base, merges the branch and records it as merged; on a failure keeps the
/// branch waiting. Logs `merge.passed`, `merge.failed` or `merge.requeued`.
pub fn land(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    task_id: &str,
    tested: &Tested,
    now: i64,
) -> Result<Landed, String> {
    let board = record::board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let t: &BoardTask = board
        .iter()
        .find(|t| t.task.id == task_id)
        .ok_or_else(|| format!("no task {task_id} on the board"))?;
    let key = t.ext.plan_key.clone();
    let branch = t.task.branch.clone().unwrap_or_default();
    let ws = Path::new(&swarm.workspace);
    let log = |kind: &str, detail: serde_json::Value| {
        let _ = record::event(
            conn,
            &swarm.id,
            Some(task_id),
            kind,
            Some("engine"),
            Some(detail),
            now,
        );
    };
    if !super::engine::awaits_review(t) {
        let _ = finish(
            conn,
            db,
            task_id,
            GateState::Failed,
            None,
            "decided by the operator meanwhile",
            None,
            now,
        );
        return Ok(Landed::Decided);
    }
    match tested {
        Tested::Failed { base, why, result } => {
            finish(
                conn,
                db,
                task_id,
                GateState::Failed,
                base.as_deref(),
                why,
                result.as_ref(),
                now,
            )
            .map_err(|e| e.to_string())?;
            log(
                "merge.failed",
                serde_json::json!({ "key": key, "branch": branch, "why": why,
                    "tail": result.as_ref().map(|r| r.tail.clone()) }),
            );
            Ok(Landed::Failed(why.clone()))
        }
        Tested::Passed { base, result } => {
            if head(ws).ok().as_deref() != Some(base.as_str()) {
                queue(conn, db, task_id, now).map_err(|e| e.to_string())?;
                log(
                    "merge.requeued",
                    serde_json::json!({ "key": key, "base": base }),
                );
                return Ok(Landed::Requeued);
            }
            match super::merge_branch(ws, &branch) {
                Ok(commit) => {
                    bstore::record_review(
                        conn,
                        db,
                        &swarm.workspace,
                        task_id,
                        true,
                        Some(&branch),
                        Some(&commit),
                        now,
                    )
                    .map_err(|e| e.to_string())?;
                    finish(
                        conn,
                        db,
                        task_id,
                        GateState::Passed,
                        Some(base),
                        &commit,
                        Some(result),
                        now,
                    )
                    .map_err(|e| e.to_string())?;
                    log(
                        "review.merged",
                        serde_json::json!({ "branch": branch, "commit": commit, "gate": true }),
                    );
                    Ok(Landed::Merged(commit))
                }
                Err(e) => {
                    let why = format!("the check passed, but the merge failed: {e}");
                    finish(
                        conn,
                        db,
                        task_id,
                        GateState::Failed,
                        Some(base),
                        &why,
                        Some(result),
                        now,
                    )
                    .map_err(|e| e.to_string())?;
                    log(
                        "merge.failed",
                        serde_json::json!({ "key": key, "branch": branch, "why": why }),
                    );
                    Ok(Landed::Failed(why))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TaskIsolation;
    use crate::bus::swarm::plan::{CheckEnv, Plan, PlannedTask};
    use crate::bus::swarm::record::{Decider, NewSwarm, SwarmStatus, SwarmVerb};
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository on `main` with a `feature` branch that changes `file`
    /// (with `clash`, `main` changes it too).
    fn repo(dir: &Path, clash: bool) {
        sh(dir, &["init", "-q", "-b", "main"]);
        sh(dir, &["config", "user.email", "test@example.com"]);
        sh(dir, &["config", "user.name", "Test"]);
        sh(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join(".gitignore"), "/.loop-worktrees/\n").unwrap();
        std::fs::write(dir.join("file"), "base\n").unwrap();
        std::fs::write(dir.join("other"), "x\n").unwrap();
        sh(dir, &["add", "."]);
        sh(dir, &["commit", "-q", "-m", "base"]);
        sh(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("file"), "feature\n").unwrap();
        sh(dir, &["commit", "-q", "-am", "feature"]);
        sh(dir, &["checkout", "-q", "main"]);
        if clash {
            std::fs::write(dir.join("file"), "main\n").unwrap();
            sh(dir, &["commit", "-q", "-am", "main"]);
        }
    }

    /// A running swarm over `ws` with `check`, and one finished worktree
    /// task on the `feature` branch.
    fn swarm(ws: &Path, check: &str) -> (tempfile::TempDir, Connection, SwarmRun, String) {
        swarm_with(ws, Some(check), &MergeGateSettings::parse("on"))
    }

    fn swarm_with(
        ws: &Path,
        check: Option<&str>,
        gate_setting: &MergeGateSettings,
    ) -> (tempfile::TempDir, Connection, SwarmRun, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(&db, OpenOptions::default()).unwrap();
        let conn = open_aux(&db).unwrap();
        let n = NewSwarm {
            workspace: ws.to_string_lossy().into_owned(),
            goal: "ship it".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Worktree,
            gate: true,
            check_cmd: check.map(str::to_string),
            owner_run: None,
            worker_pool: vec![],
            headless: false,
            review: crate::bus::swarm::review::ReviewSettings::off(),
            merge_gate: gate_setting.clone(),
        };
        let s = record::create_swarm(&conn, None, &n, 1).unwrap();
        let plan = Plan {
            summary: "one".into(),
            tasks: vec![PlannedTask {
                key: "a".into(),
                title: "Task a".into(),
                instructions: "do it".into(),
                ..Default::default()
            }],
            review: None,
        };
        let env = CheckEnv {
            harnesses: vec!["claude".into()],
            git_repo: true,
            ..Default::default()
        };
        record::submit_plan(&conn, None, &s, &plan, &env, 2).unwrap();
        record::set_status(&conn, None, &s.id, SwarmStatus::Running, 3).unwrap();
        let id = record::board(&conn, &s.id).unwrap()[0].task.id.clone();
        conn.execute(
            "UPDATE agent_tasks SET status = 'done', branch = 'feature', ended_ns = 4 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let s = record::get_swarm(&conn, &s.id).unwrap().unwrap();
        (dir, conn, s, id)
    }

    fn task(conn: &Connection, s: &SwarmRun, id: &str) -> BoardTask {
        record::board(conn, &s.id)
            .unwrap()
            .into_iter()
            .find(|t| t.task.id == id)
            .unwrap()
    }

    fn verb(conn: &Connection, s: &SwarmRun, v: SwarmVerb) -> Result<String, String> {
        record::apply(
            conn,
            None,
            s,
            v,
            Some("a"),
            &CheckEnv::default(),
            &Decider::TUI,
            10,
        )
    }

    fn file(ws: &Path) -> String {
        std::fs::read_to_string(ws.join("file")).unwrap()
    }

    #[test]
    fn a_merge_waits_for_the_check_on_the_branch_brought_up_to_date() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        // main moves on in another file: the branch is tested with it
        std::fs::write(ws.path().join("other"), "main\n").unwrap();
        sh(ws.path(), &["commit", "-q", "-am", "main moves"]);
        let (_d, conn, s, id) = swarm(ws.path(), "grep -q feature file && grep -q main other");
        let said = verb(&conn, &s, SwarmVerb::Merge).unwrap();
        assert!(said.starts_with("queued `a`"), "{said}");
        assert_eq!(file(ws.path()), "base\n", "nothing merged yet");
        let t = task(&conn, &s, &id);
        assert_eq!(
            t.ext.gate.as_ref().map(|g| g.state),
            Some(GateState::Queued)
        );
        assert!(
            verb(&conn, &s, SwarmVerb::Merge)
                .unwrap_err()
                .contains("already queued")
        );

        start(&conn, None, &id, 11).unwrap();
        let tested = test_branch(
            ws.path(),
            None,
            "feature",
            s.check_cmd.as_deref().unwrap(),
            Duration::from_secs(30),
            11,
        );
        assert!(matches!(tested, Tested::Passed { .. }), "{tested:?}");
        let landed = land(&conn, None, &s, &id, &tested, 12).unwrap();
        assert!(matches!(landed, Landed::Merged(_)), "{landed:?}");
        assert_eq!(file(ws.path()), "feature\n");
        let t = task(&conn, &s, &id);
        assert_eq!(t.review.as_deref(), Some("merged"));
        assert_eq!(
            t.ext.gate.as_ref().map(|g| g.state),
            Some(GateState::Passed)
        );
        assert!(
            !ws.path()
                .join(".loop-worktrees/merge-gate-feature")
                .exists(),
            "the temporary worktree is removed"
        );
    }

    #[test]
    fn a_failed_check_merges_nothing_and_force_merge_goes_past_it() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, id) = swarm(ws.path(), "echo boom; exit 3");
        verb(&conn, &s, SwarmVerb::Merge).unwrap();
        let before = head(ws.path()).unwrap();
        let tested = test_branch(
            ws.path(),
            None,
            "feature",
            "echo boom; exit 3",
            Duration::from_secs(30),
            11,
        );
        let landed = land(&conn, None, &s, &id, &tested, 12).unwrap();
        match landed {
            Landed::Failed(why) => assert!(why.contains("exited 3"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            head(ws.path()).unwrap(),
            before,
            "the workspace is as it was"
        );
        let t = task(&conn, &s, &id);
        let g = t.ext.gate.clone().unwrap();
        assert_eq!(g.state, GateState::Failed);
        assert_eq!(g.result.unwrap().tail, "boom");
        assert!(t.review.is_none(), "still waiting for the operator");
        // a failed gate may be queued again, or merged past the check
        let merged = verb(&conn, &s, SwarmVerb::ForceMerge).unwrap();
        assert!(merged.starts_with("merged feature"), "{merged}");
        assert_eq!(file(ws.path()), "feature\n");
    }

    #[test]
    fn a_conflict_or_a_moved_workspace_keeps_the_branch_out() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), true);
        let (_d, conn, s, id) = swarm(ws.path(), "true");
        verb(&conn, &s, SwarmVerb::Merge).unwrap();
        let tested = test_branch(
            ws.path(),
            None,
            "feature",
            "true",
            Duration::from_secs(30),
            11,
        );
        match &tested {
            Tested::Failed { why, .. } => assert!(why.contains("does not merge cleanly"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(sh(ws.path(), &["status", "--porcelain"]), "");

        // tested clean, but the workspace moved before the merge: queued again
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, id2) = swarm(ws.path(), "true");
        let _ = id;
        verb(&conn, &s, SwarmVerb::Merge).unwrap();
        let tested = test_branch(
            ws.path(),
            None,
            "feature",
            "true",
            Duration::from_secs(30),
            11,
        );
        std::fs::write(ws.path().join("other"), "later\n").unwrap();
        sh(ws.path(), &["commit", "-q", "-am", "later"]);
        assert_eq!(
            land(&conn, None, &s, &id2, &tested, 12).unwrap(),
            Landed::Requeued
        );
        let t = task(&conn, &s, &id2);
        assert_eq!(t.ext.gate.map(|g| g.state), Some(GateState::Queued));
        assert_eq!(file(ws.path()), "base\n");
    }

    #[test]
    fn uncommitted_work_in_the_worktree_fails_the_gate() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let wt = ws.path().join(".loop-worktrees/a");
        sh(
            ws.path(),
            &["worktree", "add", "-q", &wt.to_string_lossy(), "feature"],
        );
        std::fs::write(wt.join("file"), "half done\n").unwrap();
        let tested = test_branch(
            ws.path(),
            Some(&wt),
            "feature",
            "true",
            Duration::from_secs(30),
            1,
        );
        match tested {
            Tested::Failed { why, .. } => {
                assert!(why.contains("uncommitted changes (file)"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_gate_is_a_setting_of_its_own() {
        let off = MergeGateSettings::parse("");
        assert_eq!((off.enabled, off.as_choice()), (false, "off".to_string()));
        let on = MergeGateSettings::parse("On");
        assert_eq!((on.enabled, on.command.clone()), (true, None));
        let own = MergeGateSettings::parse("  cargo test --lib ");
        assert_eq!(own.as_choice(), "cargo test --lib");
        // `on` needs the swarm's check; a command of its own does not
        assert!(on.check(None).unwrap_err().contains("there is none"));
        assert!(on.check(Some("make")).is_ok() && own.check(None).is_ok());

        // off: a check command alone merges at once, as before the gate
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, id) = swarm_with(ws.path(), Some("exit 1"), &off);
        assert_eq!(settings(&conn, &s.id), off);
        assert_eq!(off.command_for(&s), None);
        let merged = verb(&conn, &s, SwarmVerb::Merge).unwrap();
        assert!(merged.starts_with("merged feature"), "{merged}");
        assert!(task(&conn, &s, &id).ext.gate.is_none());

        // its own command: queued behind it, whatever the check command is
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, _) = swarm_with(ws.path(), Some("cargo test"), &own);
        assert_eq!(
            settings(&conn, &s.id).command_for(&s).as_deref(),
            Some("cargo test --lib")
        );
        let queued = verb(&conn, &s, SwarmVerb::Merge).unwrap();
        assert!(queued.contains("`cargo test --lib` runs"), "{queued}");
    }

    fn set_gate(conn: &Connection, s: &SwarmRun, value: &str) -> Result<String, String> {
        record::apply_with(
            conn,
            None,
            s,
            SwarmVerb::MergeGate,
            None,
            Some(value),
            &CheckEnv::default(),
            &Decider::TUI,
            20,
        )
    }

    #[test]
    fn the_operator_changes_the_gate_on_a_running_swarm() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, id) = swarm_with(ws.path(), None, &MergeGateSettings::off());
        // `on` needs a check command; a command of its own does not
        assert!(
            set_gate(&conn, &s, "on")
                .unwrap_err()
                .contains("there is none")
        );
        assert!(
            set_gate(&conn, &s, "  ")
                .unwrap_err()
                .contains("say the merge gate")
        );
        let on = set_gate(&conn, &s, "grep -q feature file").unwrap();
        assert!(
            on.starts_with("merge gate on: merge queues the branch"),
            "{on}"
        );
        assert_eq!(
            set_gate(&conn, &s, "grep -q feature file").unwrap(),
            "the merge gate is already grep -q feature file"
        );
        // merges queue now
        assert!(
            verb(&conn, &s, SwarmVerb::Merge)
                .unwrap()
                .starts_with("queued `a`")
        );
        // off: the queued merge waits for the operator again, nothing merged
        let off = set_gate(&conn, &s, "off").unwrap();
        assert!(
            off.contains("1 queued merge(s) wait for you again (a)"),
            "{off}"
        );
        let t = task(&conn, &s, &id);
        assert!(t.ext.gate.is_none() && t.review.is_none());
        assert_eq!(file(ws.path()), "base\n");
        assert!(
            verb(&conn, &s, SwarmVerb::Merge)
                .unwrap()
                .starts_with("merged feature")
        );
        // every change is a decision and an event
        let decided = record::decisions(&conn, &s.id).unwrap();
        assert_eq!(
            decided
                .iter()
                .filter(|d| d.verb == SwarmVerb::MergeGate)
                .count(),
            2,
            "{decided:?}"
        );
        let events = record::recent_events(&conn, &s.id, 20).unwrap();
        assert!(events.iter().any(|e| e.kind == "merge_gate.changed"));
        assert_eq!(settings(&conn, &s.id), MergeGateSettings::off());
    }

    #[test]
    fn turning_the_gate_off_lets_a_running_check_finish() {
        let ws = tempfile::tempdir().unwrap();
        repo(ws.path(), false);
        let (_d, conn, s, id) = swarm(ws.path(), "true");
        verb(&conn, &s, SwarmVerb::Merge).unwrap();
        start(&conn, None, &id, 11).unwrap();
        let off = set_gate(&conn, &s, "off").unwrap();
        assert!(
            off.contains("the check already running on a finishes"),
            "{off}"
        );
        let tested = test_branch(
            ws.path(),
            None,
            "feature",
            "true",
            Duration::from_secs(30),
            11,
        );
        assert!(matches!(
            land(&conn, None, &s, &id, &tested, 12).unwrap(),
            Landed::Merged(_)
        ));
    }
}
