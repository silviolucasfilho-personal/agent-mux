//! A review of each finished task (schema v25). When a task is done, the
//! dispatcher puts a review on the board: one more task, run by the
//! `reviewer` agent on the review harness (by default one other than the
//! worker's, when the swarm's pool has one), read-only, in the reviewed
//! task's worktree. Its result starts with a verdict, `APPROVE` or
//! `CHANGES`, that the board, the coordinator and the merge read. The
//! review is on for every new swarm; the launch form, `agent_mux_swarm_start`
//! (`review`) or the plan (`review.enabled`, a task's `review = false`)
//! dismiss it.

use super::record::{BoardTask, SwarmRun};
use crate::bus::TaskStatus;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The agent a review runs as unless the swarm names another.
pub const DEFAULT_AGENT: &str = "reviewer";

/// How a swarm reviews its finished tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSettings {
    pub enabled: bool,
    /// The review harness; `None`: one other than the worker's, from the
    /// swarm's pool when it has one.
    pub harness: Option<String>,
    pub agent: String,
}

impl Default for ReviewSettings {
    /// On, on another harness than the worker's, as `reviewer`.
    fn default() -> Self {
        ReviewSettings {
            enabled: true,
            harness: None,
            agent: DEFAULT_AGENT.into(),
        }
    }
}

impl ReviewSettings {
    pub fn off() -> Self {
        ReviewSettings {
            enabled: false,
            ..Default::default()
        }
    }

    /// `auto` (on, another harness than the worker's), `off`, `on`, or a
    /// harness name (on, on that harness).
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "" | "auto" | "on" | "yes" | "true" => Ok(Self::default()),
            "off" | "no" | "none" | "false" => Ok(Self::off()),
            h => match h.parse::<crate::harness::Harness>() {
                Ok(h) => Ok(ReviewSettings {
                    harness: Some(h.as_str().to_string()),
                    ..Default::default()
                }),
                Err(_) => Err(format!(
                    "review: `{s}` is not `auto`, `off` or a harness (claude, codex, agy, opencode, muse)"
                )),
            },
        }
    }

    /// The setting as `parse` reads it back: `off`, `auto` or the harness.
    pub fn as_choice(&self) -> String {
        match (&self.enabled, &self.harness) {
            (false, _) => "off".into(),
            (true, None) => "auto".into(),
            (true, Some(h)) => h.clone(),
        }
    }

    /// One line for the plan table and the context.
    pub fn describe(&self) -> String {
        if !self.enabled {
            return "off: finished tasks go to the operator unreviewed".into();
        }
        let on = match &self.harness {
            Some(h) => format!("on {h}"),
            None => "on a harness other than the worker's (from the pool, when it has one)".into(),
        };
        format!(
            "each finished task is reviewed by the `{}` agent {on}, read-only, before the merge",
            self.agent
        )
    }
}

/// The swarm's review setting; a swarm without one (older ones) has none.
pub fn settings(conn: &Connection, swarm_id: &str) -> ReviewSettings {
    conn.query_row(
        "SELECT mode, harness, agent FROM swarm_review_settings WHERE swarm_id = ?1",
        params![swarm_id],
        |r| {
            Ok(ReviewSettings {
                enabled: r.get::<_, String>(0)? == "on",
                harness: r.get(1)?,
                agent: r
                    .get::<_, Option<String>>(2)?
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_AGENT.into()),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or_else(ReviewSettings::off)
}

pub fn set_settings(
    conn: &Connection,
    swarm_id: &str,
    s: &ReviewSettings,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_review_settings (swarm_id, mode, harness, agent, updated_ns)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (swarm_id) DO UPDATE SET mode = excluded.mode, harness = excluded.harness,
             agent = excluded.agent, updated_ns = excluded.updated_ns",
        params![
            swarm_id,
            if s.enabled { "on" } else { "off" },
            s.harness,
            s.agent,
            now
        ],
    )?;
    Ok(())
}

/// The plan dismissed (or brought back) one task's review.
pub fn set_skip(conn: &Connection, task_id: &str, skip: bool, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_task_reviews (task_id, skip, updated_ns) VALUES (?1, ?2, ?3)
         ON CONFLICT (task_id) DO UPDATE SET skip = excluded.skip, updated_ns = excluded.updated_ns",
        params![task_id, skip, now],
    )?;
    Ok(())
}

/// `review_task` reviews `task_id`.
pub fn link(conn: &Connection, task_id: &str, review_task: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_task_reviews (task_id, review_task, updated_ns) VALUES (?1, ?2, ?3)
         ON CONFLICT (task_id) DO UPDATE SET review_task = excluded.review_task,
             updated_ns = excluded.updated_ns",
        params![task_id, review_task, now],
    )?;
    Ok(())
}

/// What a review concluded, read from the first line of its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    Changes,
}

pub fn verdict(result: &str) -> Option<Verdict> {
    let first = result.lines().map(str::trim).find(|l| !l.is_empty())?;
    let word: String = first
        .trim_start_matches(|c: char| !c.is_ascii_alphabetic())
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    match word.as_str() {
        "APPROVE" | "APPROVED" => Some(Verdict::Approve),
        "CHANGES" | "REQUEST" | "REJECT" => Some(Verdict::Changes),
        _ => None,
    }
}

/// Where a task's review stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewState {
    /// The plan dismissed it.
    Skipped,
    /// Done, its review not started yet.
    Pending,
    Running,
    Approved,
    Changes,
    /// Done without a verdict on its first line.
    Unclear,
    /// The review failed or was cancelled.
    Failed,
}

impl ReviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewState::Skipped => "skipped",
            ReviewState::Pending => "pending",
            ReviewState::Running => "running",
            ReviewState::Approved => "approved",
            ReviewState::Changes => "changes",
            ReviewState::Unclear => "unclear",
            ReviewState::Failed => "failed",
        }
    }
    /// The board's badge.
    pub fn glyph(self) -> &'static str {
        match self {
            ReviewState::Skipped => "",
            ReviewState::Pending | ReviewState::Running => "◔",
            ReviewState::Approved => "✓",
            ReviewState::Changes => "✗",
            ReviewState::Unclear | ReviewState::Failed => "?",
        }
    }
}

/// The task's review, if it has one: none for a review itself, a task
/// that is not done, or a swarm without reviews.
pub fn state(t: &BoardTask, board: &[BoardTask], enabled: bool) -> Option<ReviewState> {
    if t.ext.review_of.is_some() || t.task.status != TaskStatus::Done {
        return None;
    }
    if t.ext.review_skip {
        return Some(ReviewState::Skipped);
    }
    let Some(rid) = &t.ext.review_task else {
        return enabled.then_some(ReviewState::Pending);
    };
    let Some(r) = board.iter().find(|o| &o.task.id == rid) else {
        return Some(ReviewState::Failed);
    };
    Some(match r.task.status {
        TaskStatus::Open => ReviewState::Pending,
        TaskStatus::Claimed | TaskStatus::Running => ReviewState::Running,
        TaskStatus::Failed | TaskStatus::Cancelled => ReviewState::Failed,
        TaskStatus::Done => match result_text(r).as_deref().and_then(verdict) {
            Some(Verdict::Approve) => ReviewState::Approved,
            Some(Verdict::Changes) => ReviewState::Changes,
            None => ReviewState::Unclear,
        },
    })
}

/// Why merging a task in `state` goes against its review, if it does.
pub fn merge_warning(state: Option<ReviewState>) -> Option<&'static str> {
    match state? {
        ReviewState::Approved | ReviewState::Skipped => None,
        ReviewState::Changes => Some("its review asked for changes"),
        ReviewState::Pending | ReviewState::Running => Some("its review has not finished"),
        ReviewState::Unclear => Some("its review gave no verdict"),
        ReviewState::Failed => Some("its review failed"),
    }
}

/// A task's result as text.
pub fn result_text(t: &BoardTask) -> Option<String> {
    t.task.result.as_ref().map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// The harness that reviews a task the worker ran on `worker`: the
/// swarm's choice, else the first of the pool that is neither the
/// worker's nor agy (it runs the agent read-only, without a shell), else
/// the first of the pool that is not the worker's, else the worker's.
pub fn harness_for(s: &ReviewSettings, worker: Option<&str>, pool: &[String]) -> String {
    if let Some(h) = &s.harness {
        return h.clone();
    }
    let other = |p: &&String| Some(p.as_str()) != worker;
    pool.iter()
        .filter(other)
        .find(|p| p.as_str() != "agy")
        .or_else(|| pool.iter().find(other))
        .cloned()
        .or_else(|| worker.map(str::to_string))
        .or_else(|| pool.first().cloned())
        .unwrap_or_else(|| "claude".into())
}

/// A key for the review of `key` that the board does not hold yet.
pub fn review_key(board: &[BoardTask], key: &str) -> String {
    let base = format!("{key}-review");
    let mut k = base.clone();
    let mut n = 2;
    while board.iter().any(|t| t.ext.plan_key == k) {
        k = format!("{base}-{n}");
        n += 1;
    }
    k
}

/// The review task's own instructions (what the Task tab shows).
pub fn instructions(t: &BoardTask) -> String {
    format!(
        "Review `{key}` ({title}), done by its worker on {harness}: read the change, check it \
         against its instructions and its done-when line, and answer APPROVE or CHANGES with \
         the findings. Read-only: change no file.",
        key = t.ext.plan_key,
        title = t.task.title,
        harness = t.task.target_harness.as_deref().unwrap_or("its harness"),
    )
}

/// The opening prompt of the session that reviews `reviewed`. `base` is the
/// workspace's branch a worktree branch forked from, when known.
pub fn prompt(
    swarm: &SwarmRun,
    review: &BoardTask,
    reviewed: &BoardTask,
    base: Option<&str>,
) -> String {
    let id = &review.task.id;
    let ws = &swarm.workspace;
    let mut p = format!(
        "You review one change of an agent-mux swarm. Your task is `{id}` ({key}). You are a \
         reviewer: read, run read-only commands and tests, and **change no file**: no edit, no \
         commit, no push.\n\n\
         ### The change under review: `{rkey}` ({title})\n\
         Done by its worker on {harness}. Its instructions were:\n\n{instructions}\n",
        key = review.ext.plan_key,
        rkey = reviewed.ext.plan_key,
        title = reviewed.task.title,
        harness = reviewed
            .task
            .target_harness
            .as_deref()
            .unwrap_or("its harness"),
        instructions = reviewed.task.instructions,
    );
    if let Some(a) = reviewed
        .ext
        .acceptance
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    {
        p.push_str(&format!("\n### Done when\n{a}\n"));
    }
    if let Some(r) = result_text(reviewed) {
        p.push_str(&format!("\n### What the worker says it did\n{r}\n"));
    }
    p.push_str("\n### Where the change is\n");
    match &reviewed.task.branch {
        Some(b) => {
            let base = base.unwrap_or("the branch it forked from");
            p.push_str(&format!(
                "You are in the worktree of branch `{b}`. Its commits: `git log --oneline {base}..HEAD`; \
                 the change: `git diff {base}...HEAD` (and `git status` for anything uncommitted).\n"
            ));
        }
        None => {
            p.push_str(
                "The worker changed the shared workspace you are in: `git status` and `git diff` \
                 show what is not committed yet, `git log` what is.",
            );
            if !reviewed.task.files.is_empty() {
                p.push_str(&format!(
                    " Its files: {}.",
                    reviewed
                        .task
                        .files
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            p.push('\n');
        }
    }
    p.push_str(&format!(
        "\n### What to check\n\
         - It does what the instructions ask, and the done-when line holds (run it when it is a command).\n\
         - Defects you can point at: the line, and the input that triggers it. Prefer one proven \
           finding to five guesses.\n\
         - Tests: present for the new behaviour, and passing.\n\n\
         ### Your verdict\n\
         Finish with `agent_mux_task_update(action = \"done\", task_id = \"{id}\", workspace = \"{ws}\", result = \"...\")`. \
         The result's **first line is the verdict**: `APPROVE` (followed by one line on why), or \
         `CHANGES` followed by the findings, one per line, each with `file:line`. If you cannot \
         review it, call `agent_mux_task_update(action = \"fail\", task_id = \"{id}\", workspace = \"{ws}\", result = \"<why>\")`. \
         A question for the coordinator: `agent_mux_send_message(to = \"coordinator\", topic = \"question\", wake = true, workspace = \"{ws}\", body = \"...\")`. \
         Without the MCP tools: `agent-mux bus done {id} --workspace {ws} --result '<verdict and findings>'`."
    ));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_the_first_word_of_the_first_line() {
        assert_eq!(verdict("APPROVE — tests pass"), Some(Verdict::Approve));
        assert_eq!(verdict("\n**Approved**: fine"), Some(Verdict::Approve));
        assert_eq!(
            verdict("CHANGES\n- src/a.rs:3 off by one"),
            Some(Verdict::Changes)
        );
        assert_eq!(verdict("Request changes: x"), Some(Verdict::Changes));
        assert_eq!(verdict("Looks fine to me"), None);
        assert_eq!(verdict(""), None);
    }

    #[test]
    fn the_review_harness_is_another_one_than_the_workers_when_the_pool_has_one() {
        let pool = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let auto = ReviewSettings::default();
        assert_eq!(
            harness_for(&auto, Some("claude"), &pool(&["claude", "codex"])),
            "codex"
        );
        assert_eq!(
            harness_for(&auto, Some("codex"), &pool(&["claude", "codex"])),
            "claude"
        );
        // agy only when nothing else is there
        assert_eq!(
            harness_for(&auto, Some("claude"), &pool(&["claude", "agy", "codex"])),
            "codex"
        );
        assert_eq!(
            harness_for(&auto, Some("claude"), &pool(&["claude", "agy"])),
            "agy"
        );
        assert_eq!(
            harness_for(&auto, Some("claude"), &pool(&["claude"])),
            "claude"
        );
        let fixed = ReviewSettings::parse("opencode").unwrap();
        assert_eq!(
            harness_for(&fixed, Some("claude"), &pool(&["claude", "codex"])),
            "opencode"
        );
    }

    #[test]
    fn the_setting_reads_back_what_was_chosen() {
        for s in ["auto", "off", "codex"] {
            assert_eq!(ReviewSettings::parse(s).unwrap().as_choice(), s);
        }
        assert!(ReviewSettings::parse("bogus").is_err());
        assert!(!ReviewSettings::parse("off").unwrap().enabled);
    }
}
