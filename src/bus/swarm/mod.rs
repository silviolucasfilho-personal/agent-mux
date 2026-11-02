//! The orchestrated swarm (`src/bus/swarm/`,
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`).
//! agent-mux runs a swarm itself: a headless planner's answer becomes a
//! checked [`plan::Plan`] on the board ([`record`]), and the dispatcher
//! ([`engine::next_actions`]) starts workers whose opening prompt is their
//! whole task ([`prompt`]). This module keeps what the screen, the
//! dispatcher and the CLI share: open questions, live session telemetry,
//! ages and the branch merge.

pub mod check;
pub mod engine;
pub mod plan;
pub mod prompt;
pub mod record;

use super::BusMessage;
use std::path::Path;

/// The `question` messages nobody answered: no later message names one in
/// `reply_to`, and its asker got nothing back from whom it asked (anyone,
/// for a broadcast question).
pub fn open_questions(messages: &[BusMessage]) -> Vec<&BusMessage> {
    messages
        .iter()
        .filter(|q| q.topic == "question")
        .filter(|q| {
            !messages.iter().any(|m| {
                m.created_ns >= q.created_ns
                    && m.id != q.id
                    && (m.reply_to.as_deref() == Some(q.id.as_str())
                        || (m.to_launch.as_deref() == Some(q.from_launch.as_str())
                            && q.to_launch.as_ref().is_none_or(|to| *to == m.from_launch)))
            })
        })
        .collect()
}

/// Live session telemetry supplied directly by the running `App` (when in TUI mode).
#[derive(Debug, Clone)]
pub struct LiveSwarmSession {
    pub session_idx: usize,
    pub session_id: usize,
    pub launch_id: String,
    pub provider: String,
    pub profile_name: String,
    pub agent_persona: Option<String>,
    pub state: String,
    pub workspace: String,
    pub is_delegated_worker: bool,
    pub turns: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    pub running_tool: Option<String>,
    /// Nanoseconds since the session last printed anything.
    pub quiet_ns: Option<i64>,
}

/// A duration in nanoseconds as `42s`, `5m` or `3h`.
pub fn short_age(ns: i64) -> String {
    let s = (ns / 1_000_000_000).max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3_600)
    }
}

/// Merges `branch` into `workspace`'s checked-out branch with a merge
/// commit (`git merge --no-ff --no-edit`). Git itself refuses when local
/// changes would be overwritten; on a conflict the merge is aborted and the
/// workspace left as it was. Answers the new commit's short hash.
pub fn merge_branch(workspace: &Path, branch: &str) -> Result<String, String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let out = git(&["merge", "--no-ff", "--no-edit", branch])?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let conflicted =
            git(&["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok_and(|o| o.status.success());
        if conflicted {
            let _ = git(&["merge", "--abort"]);
        }
        let why = text
            .lines()
            .map(str::trim)
            .find(|l| {
                l.starts_with("CONFLICT") || l.starts_with("error:") || l.starts_with("fatal:")
            })
            .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("git merge failed")
            .to_string();
        return Err(if conflicted {
            format!("{why} (merge aborted, nothing changed)")
        } else {
            why
        });
    }
    let head = git(&["rev-parse", "--short", "HEAD"])?;
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::store as bstore;
    use crate::bus::{NewTaskSpec, TaskStatus};
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};
    use rusqlite::Connection;

    fn store() -> (tempfile::TempDir, std::path::PathBuf, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-lanes".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        (dir, db, conn, ws)
    }

    fn task(
        conn: &Connection,
        db: &Path,
        ws: &str,
        title: &str,
        to: Option<&str>,
        at: i64,
    ) -> String {
        bstore::split_tasks(
            conn,
            Some(db),
            ws,
            "coord-1",
            &[NewTaskSpec {
                title: title.into(),
                instructions: "…".into(),
                assigned_to: to.map(str::to_string),
                ..Default::default()
            }],
            at,
        )
        .unwrap()
        .remove(0)
        .id
    }

    #[test]
    fn a_question_is_open_until_answered_by_reply_or_by_the_addressee() {
        let (_dir, db, conn, ws) = store();
        let q = bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            Some("coord-1"),
            "question",
            "TTL?",
            None,
            1,
        )
        .unwrap();
        let msgs = bstore::list_workspace_messages(&conn, &ws, 50).unwrap();
        assert_eq!(open_questions(&msgs).len(), 1);

        // A message from someone else to the asker does not answer it.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-2",
            Some("w-1"),
            "message",
            "hi",
            None,
            3,
        )
        .unwrap();
        let msgs = bstore::list_workspace_messages(&conn, &ws, 50).unwrap();
        assert_eq!(open_questions(&msgs).len(), 1);

        // The addressee writing back does.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            Some("w-1"),
            "answer",
            "1h",
            None,
            5,
        )
        .unwrap();
        let msgs = bstore::list_workspace_messages(&conn, &ws, 50).unwrap();
        assert!(open_questions(&msgs).is_empty());

        // A reply naming the question answers it, whoever sends it.
        let q2 = bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            None,
            "question",
            "anyone?",
            None,
            7,
        )
        .unwrap();
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "user",
            None,
            "answer",
            "me",
            Some(&q2.id),
            8,
        )
        .unwrap();
        let msgs = bstore::list_workspace_messages(&conn, &ws, 50).unwrap();
        assert!(open_questions(&msgs).is_empty(), "{:?}", q.id);
    }

    #[test]
    fn a_failed_task_reopens_for_another_worker_and_a_done_one_does_not() {
        let (_dir, db, conn, ws) = store();
        let id = task(&conn, &db, &ws, "ui tests", Some("w-1"), 1);
        bstore::update_task(
            &conn,
            Some(&db),
            &id,
            "w-1",
            TaskStatus::Failed,
            None,
            Some("boom".into()),
            None,
            Some("bus-x".into()),
            2,
        )
        .unwrap();
        let t = bstore::reopen_task(&conn, Some(&db), &id, 3).unwrap();
        assert_eq!(t.status, TaskStatus::Open);
        assert_eq!(t.assigned_to, None);
        assert_eq!(t.error, None);
        assert_eq!(t.ended_ns, None);
        assert_eq!(
            t.branch.as_deref(),
            Some("bus-x"),
            "the branch stays for the retry"
        );
        bstore::update_task(
            &conn,
            Some(&db),
            &id,
            "w-2",
            TaskStatus::Done,
            None,
            None,
            None,
            None,
            4,
        )
        .unwrap();
        assert!(bstore::reopen_task(&conn, Some(&db), &id, 5).is_err());
    }
}
