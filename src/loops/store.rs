//! `loop_runs` in the trace store: one row per scheduled loop run, plus
//! the facts of a finished run read back from its launch's traces. Every
//! function takes a connection the caller opened (`store::open_aux` to
//! write, `store::open_ro` to read); nothing here opens or migrates a
//! store.

use super::{Level, LoopLaunch, LoopPolicy, Outcome};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// A `loop_runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopRun {
    /// RFC 3339 UTC; the `run_id` in `loop-run-log.md`.
    pub id: String,
    pub loop_id: String,
    pub workspace: String,
    pub pattern: String,
    pub harness: String,
    pub level: Level,
    pub effective_level: Level,
    pub launch_id: Option<String>,
    pub scheduled_ns: i64,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
    pub outcome: Outcome,
    pub items_found: Option<i64>,
    pub actions_taken: Option<i64>,
    pub escalations: Option<i64>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub readiness_score: Option<i64>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    /// `applied` or `rejected`, once a human decided.
    pub decision: Option<String>,
    pub decided_ns: Option<i64>,
    /// `reason`, `level_reason`, `verifier`, `files`, `gate_violation`,
    /// `final_message`, `exit_code`, `timed_out`, …
    pub detail: Value,
    /// `Pattern::digest()` of the effective pattern this run executed, so
    /// a reader can tell two runs of the same id apart after the library
    /// shadowed its text. `None` for rows written before the column.
    pub pattern_hash: Option<String>,
}

impl LoopRun {
    /// A fresh row for a run that has just been scheduled.
    pub fn new(
        id: impl Into<String>,
        loop_id: impl Into<String>,
        workspace: impl Into<String>,
        pattern: impl Into<String>,
        harness: impl Into<String>,
        level: Level,
        scheduled_ns: i64,
    ) -> LoopRun {
        LoopRun {
            id: id.into(),
            loop_id: loop_id.into(),
            workspace: workspace.into(),
            pattern: pattern.into(),
            harness: harness.into(),
            level,
            effective_level: level,
            launch_id: None,
            scheduled_ns,
            started_ns: None,
            ended_ns: None,
            outcome: Outcome::NoOp,
            items_found: None,
            actions_taken: None,
            escalations: None,
            tokens: None,
            cost_usd: None,
            readiness_score: None,
            worktree: None,
            branch: None,
            decision: None,
            decided_ns: None,
            detail: Value::Object(serde_json::Map::new()),
            pattern_hash: None,
        }
    }

    /// Sets one key of `detail`.
    pub fn set_detail(&mut self, key: &str, value: Value) {
        if !self.detail.is_object() {
            self.detail = Value::Object(serde_json::Map::new());
        }
        if let Some(m) = self.detail.as_object_mut() {
            m.insert(key.to_string(), value);
        }
    }

    pub fn detail_str(&self, key: &str) -> Option<&str> {
        self.detail.get(key).and_then(Value::as_str)
    }

    /// Wall-clock duration in seconds, once both ends are known.
    pub fn duration_s(&self) -> Option<i64> {
        Some((self.ended_ns? - self.started_ns?).max(0) / 1_000_000_000)
    }

    /// Waits on a human decision.
    pub fn in_inbox(&self) -> bool {
        self.outcome.needs_human() && self.decision.is_none()
    }
}

const COLUMNS: &str = "id, loop_id, workspace, pattern, harness, level, effective_level, launch_id, \
    scheduled_ns, started_ns, ended_ns, outcome, items_found, actions_taken, escalations, tokens, \
    cost_usd, readiness_score, worktree, branch, decision, decided_ns, detail, \
    (SELECT pattern_hash FROM loop_run_patterns p WHERE p.run_id = loop_runs.id)";

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<LoopRun> {
    let level: String = r.get(5)?;
    let effective: String = r.get(6)?;
    let outcome: String = r.get(11)?;
    let detail: String = r.get(22)?;
    Ok(LoopRun {
        id: r.get(0)?,
        loop_id: r.get(1)?,
        workspace: r.get(2)?,
        pattern: r.get(3)?,
        harness: r.get(4)?,
        level: Level::parse(&level).unwrap_or_default(),
        effective_level: Level::parse(&effective).unwrap_or_default(),
        launch_id: r.get(7)?,
        scheduled_ns: r.get(8)?,
        started_ns: r.get(9)?,
        ended_ns: r.get(10)?,
        outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Failed),
        items_found: r.get(12)?,
        actions_taken: r.get(13)?,
        escalations: r.get(14)?,
        tokens: r.get(15)?,
        cost_usd: r.get(16)?,
        readiness_score: r.get(17)?,
        worktree: r.get(18)?,
        branch: r.get(19)?,
        decision: r.get(20)?,
        decided_ns: r.get(21)?,
        detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
        pattern_hash: r.get(23)?,
    })
}

/// Inserts the row or updates every mutable column of an existing one.
pub fn upsert_run(conn: &Connection, run: &LoopRun) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO loop_runs (id, loop_id, workspace, pattern, harness, level, effective_level, \
             launch_id, scheduled_ns, started_ns, ended_ns, outcome, items_found, actions_taken, \
             escalations, tokens, cost_usd, readiness_score, worktree, branch, decision, decided_ns, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
         ON CONFLICT(id) DO UPDATE SET
             loop_id = excluded.loop_id, workspace = excluded.workspace, pattern = excluded.pattern,
             harness = excluded.harness, level = excluded.level, effective_level = excluded.effective_level,
             launch_id = excluded.launch_id, scheduled_ns = excluded.scheduled_ns,
             started_ns = excluded.started_ns, ended_ns = excluded.ended_ns, outcome = excluded.outcome,
             items_found = excluded.items_found, actions_taken = excluded.actions_taken,
             escalations = excluded.escalations, tokens = excluded.tokens, cost_usd = excluded.cost_usd,
             readiness_score = excluded.readiness_score, worktree = excluded.worktree,
             branch = excluded.branch, decision = excluded.decision, decided_ns = excluded.decided_ns,
             detail = excluded.detail",
        params![
            run.id,
            run.loop_id,
            run.workspace,
            run.pattern,
            run.harness,
            run.level.as_str(),
            run.effective_level.as_str(),
            run.launch_id,
            run.scheduled_ns,
            run.started_ns,
            run.ended_ns,
            run.outcome.as_str(),
            run.items_found,
            run.actions_taken,
            run.escalations,
            run.tokens,
            run.cost_usd,
            run.readiness_score,
            run.worktree,
            run.branch,
            run.decision,
            run.decided_ns,
            run.detail.to_string(),
        ],
    )?;
    // The digest lives beside the row (see schema v14). `None` leaves any
    // earlier value alone: a reconstructed row must not erase what the
    // launch recorded.
    if let Some(hash) = &run.pattern_hash {
        conn.execute(
            "INSERT INTO loop_run_patterns (run_id, pattern_hash) VALUES (?1, ?2)
             ON CONFLICT(run_id) DO UPDATE SET pattern_hash = excluded.pattern_hash",
            params![run.id, hash],
        )?;
    }
    Ok(())
}

pub fn get_run(conn: &Connection, id: &str) -> rusqlite::Result<Option<LoopRun>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM loop_runs WHERE id = ?1"),
        params![id],
        row_to_run,
    )
    .optional()
}

/// The loop's runs, newest scheduled first.
pub fn recent_runs(
    conn: &Connection,
    loop_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<LoopRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM loop_runs WHERE loop_id = ?1 ORDER BY scheduled_ns DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![loop_id, limit as i64], row_to_run)?;
    rows.collect()
}

/// Every loop's runs, newest scheduled first.
pub fn all_recent_runs(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<LoopRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM loop_runs ORDER BY scheduled_ns DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map(params![limit as i64], row_to_run)?;
    rows.collect()
}

/// What a loop spent in a window: runs that started, tokens and cost.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spend {
    pub runs: i64,
    pub tokens: i64,
    pub cost_usd: f64,
}

/// Runs of `loop_id` that started at or after `since_ns`, blocked ones
/// excluded (they never started).
pub fn spend_since(conn: &Connection, loop_id: &str, since_ns: i64) -> rusqlite::Result<Spend> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(tokens), 0), COALESCE(SUM(cost_usd), 0)
         FROM loop_runs
         WHERE loop_id = ?1 AND started_ns IS NOT NULL AND started_ns >= ?2 AND outcome != 'blocked'",
        params![loop_id, since_ns],
        |r| {
            Ok(Spend {
                runs: r.get(0)?,
                tokens: r.get(1)?,
                cost_usd: r.get(2)?,
            })
        },
    )
}

/// Runs waiting on a human: fix proposed or escalated, undecided, newest
/// first.
pub fn inbox(conn: &Connection) -> rusqlite::Result<Vec<LoopRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM loop_runs
         WHERE outcome IN ('fix-proposed','escalated') AND decision IS NULL
         ORDER BY COALESCE(started_ns, scheduled_ns) DESC"
    ))?;
    let rows = stmt.query_map([], row_to_run)?;
    rows.collect()
}

/// Records the human's decision (`applied` or `rejected`) on a run.
pub fn decide(
    conn: &Connection,
    run_id: &str,
    decision: &str,
    now_ns: i64,
) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE loop_runs SET decision = ?2, decided_ns = ?3 WHERE id = ?1",
        params![run_id, decision, now_ns],
    )?;
    Ok(n > 0)
}

/// Completed runs of a workspace since `since_ns`: the store's evidence
/// of loop activity for the readiness audit.
pub fn activity_count(
    conn: &Connection,
    workspace: &str,
    since_ns: i64,
) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM loop_runs
         WHERE workspace = ?1 AND ended_ns IS NOT NULL AND ended_ns >= ?2
           AND outcome NOT IN ('blocked','failed')",
        params![workspace, since_ns],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as usize)
}

/// Facts of one finished run, read from its launch's traces.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunFacts {
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    /// A checker sub-agent (`loop-verifier`, `loop-reviewer`: a name that
    /// contains `verifier` or `reviewer`) ran on this launch.
    pub verifier_ran: bool,
    /// The checkers' combined word (`run::combined_verdict`): `APPROVE`
    /// only when every checker that answered approved, `REJECT` when any
    /// rejected, `ESCALATE_HUMAN` when any asked for a human.
    pub verifier_verdict: Option<String>,
    /// One verdict per checker sub-agent that answered, in the order they
    /// ran.
    pub verifier_verdicts: Vec<String>,
    /// Distinct paths the launch's write tools targeted, in first-seen order.
    pub files_touched: Vec<String>,
    /// The last thing the model said.
    pub final_message: Option<String>,
}

/// Tool names that write files, per harness.
pub const WRITE_TOOLS: [&str; 6] = [
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "apply_patch",
    "write_file",
];

/// The verdict line a verifier prints, if any.
pub fn parse_verdict(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("## Verdict:") {
            let word = rest.split_whitespace().next()?;
            let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            return Some(match w.to_ascii_uppercase().as_str() {
                "APPROVE" => "APPROVE".into(),
                "ESCALATE_HUMAN" => "ESCALATE_HUMAN".into(),
                _ => "REJECT".into(),
            });
        }
    }
    None
}

/// The paths a write tool's input names: `file_path`, `path`,
/// `notebook_path`, or the `*** Update File:` / `*** Add File:` /
/// `*** Delete File:` headers of an `apply_patch` input.
pub fn paths_in_tool_input(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |p: &str| {
        let p = p.trim();
        if !p.is_empty() && !out.iter().any(|x: &String| x == p) {
            out.push(p.to_string());
        }
    };
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            push(p);
        }
    }
    // apply_patch: the patch text lives under `input`, `patch` or the
    // whole value is a string
    let patch = input
        .get("input")
        .or_else(|| input.get("patch"))
        .and_then(Value::as_str)
        .or_else(|| input.as_str());
    if let Some(text) = patch {
        for line in text.lines() {
            let t = line.trim();
            for prefix in ["*** Update File:", "*** Add File:", "*** Delete File:"] {
                if let Some(p) = t.strip_prefix(prefix) {
                    push(p);
                }
            }
        }
    }
    out
}

/// Distinct paths the launch's write tools named so far (observations
/// `input` JSON and the `path` column).
pub fn files_touched(conn: &Connection, launch_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT o.name, o.tool_name, o.input, o.path
         FROM observations o JOIN traces t ON t.id = o.trace_id
         WHERE t.launch_id = ?1 AND o.type = 'tool'
         ORDER BY o.start_ns, o.rid",
    )?;
    let rows = stmt.query_map(params![launch_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        let (name, tool_name, input, path) = row?;
        let tool = tool_name.as_deref().unwrap_or(name.as_str());
        if !is_write_tool(tool) {
            continue;
        }
        let mut found = Vec::new();
        if let Some(p) = path {
            found.push(p);
        }
        if let Some(text) = input
            && let Ok(v) = serde_json::from_str::<Value>(&text)
        {
            found.extend(paths_in_tool_input(&v));
        }
        for p in found {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// A tool that writes files, by harness name (case-insensitive).
pub fn is_write_tool(name: &str) -> bool {
    WRITE_TOOLS.iter().any(|w| w.eq_ignore_ascii_case(name))
}

/// Tokens, cost, verifier, files and final message of a launch.
pub fn run_facts(conn: &Connection, launch_id: &str) -> rusqlite::Result<RunFacts> {
    let stats = crate::tracing::store::query::launch_stats(conn, launch_id)?;
    let mut facts = RunFacts {
        tokens: stats.total_tokens,
        cost_usd: stats.cost_usd,
        ..Default::default()
    };
    let mut stmt = conn.prepare(
        "SELECT o.name, o.tool_name, o.output
         FROM observations o JOIN traces t ON t.id = o.trace_id
         WHERE t.launch_id = ?1 AND o.type = 'agent'
         ORDER BY o.start_ns, o.rid",
    )?;
    let rows = stmt.query_map(params![launch_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (name, tool_name, output) = row?;
        let is_checker = |s: &str| {
            let l = s.to_ascii_lowercase();
            l.contains("verifier") || l.contains("reviewer")
        };
        if !(is_checker(&name) || tool_name.as_deref().is_some_and(is_checker)) {
            continue;
        }
        facts.verifier_ran = true;
        if let Some(v) = output.as_deref().and_then(parse_verdict) {
            facts.verifier_verdicts.push(v);
        }
    }
    facts.verifier_verdict = crate::loops::run::combined_verdict(&facts.verifier_verdicts);
    facts.files_touched = files_touched(conn, launch_id)?;
    facts.final_message = crate::tracing::experiments::final_message(conn, launch_id)?;
    Ok(facts)
}

/// The loop keys a launch row carries, if it was a loop launch.
pub fn loop_launch_of(conn: &Connection, launch_id: &str) -> rusqlite::Result<Option<LoopLaunch>> {
    let meta: Option<String> = conn
        .query_row(
            "SELECT metadata FROM launches WHERE id = ?1",
            params![launch_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(meta
        .and_then(|m| serde_json::from_str::<Value>(&m).ok())
        .and_then(|v| loop_launch_from_metadata(&v)))
}

/// Reads the `loop_*` keys of a launch's metadata document.
pub fn loop_launch_from_metadata(v: &Value) -> Option<LoopLaunch> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let policy: LoopPolicy = v
        .get("loop_policy")
        .cloned()
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default();
    Some(LoopLaunch {
        loop_id: s("loop_id")?,
        run_id: s("loop_run_id")?,
        pattern: s("loop_pattern")?,
        level: s("loop_level")
            .as_deref()
            .and_then(Level::parse)
            .unwrap_or_default(),
        workspace: s("loop_workspace").unwrap_or_default(),
        policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};
    use std::path::{Path, PathBuf};

    fn fresh_store(dir: &Path) -> PathBuf {
        let db = dir.join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-l".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        db
    }

    fn run(id: &str, loop_id: &str, outcome: Outcome, started: Option<i64>) -> LoopRun {
        let mut r = LoopRun::new(
            id,
            loop_id,
            "/ws",
            "daily-triage",
            "claude",
            Level::L1,
            1_000,
        );
        r.outcome = outcome;
        r.started_ns = started;
        r.ended_ns = started.map(|s| s + 5_000_000_000);
        r.tokens = started.map(|_| 1_000);
        r.cost_usd = started.map(|_| 0.25);
        r
    }

    #[test]
    fn rows_round_trip_and_update_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let db = fresh_store(temp.path());
        let conn = open_aux(&db).unwrap();
        let mut r = run("2026-09-16T08:00:00Z", "L1", Outcome::NoOp, None);
        r.set_detail("reason", Value::from("scheduled"));
        upsert_run(&conn, &r).unwrap();
        assert_eq!(get_run(&conn, &r.id).unwrap().unwrap(), r);
        r.outcome = Outcome::FixProposed;
        r.started_ns = Some(2_000);
        r.ended_ns = Some(9_000_000_000);
        r.tokens = Some(48_210);
        r.effective_level = Level::L2;
        r.branch = Some("loop/x".into());
        r.set_detail("files", serde_json::json!(["src/a.rs"]));
        upsert_run(&conn, &r).unwrap();
        let back = get_run(&conn, &r.id).unwrap().unwrap();
        assert_eq!(back, r);
        assert_eq!(back.duration_s(), Some(8));
        assert_eq!(back.detail_str("reason"), Some("scheduled"));
        assert!(back.in_inbox());
        assert!(get_run(&conn, "nope").unwrap().is_none());
        assert_eq!(recent_runs(&conn, "L1", 5).unwrap().len(), 1);
        assert_eq!(all_recent_runs(&conn, 5).unwrap()[0].id, r.id);
    }

    /// The digest of the pattern text a run executed survives the round
    /// trip, and a row written before the column existed reads as `None`
    /// rather than failing.
    #[test]
    fn a_run_records_the_pattern_text_it_ran() {
        let temp = tempfile::tempdir().unwrap();
        let db = fresh_store(temp.path());
        let conn = open_aux(&db).unwrap();

        let mut first = run("r1", "L1", Outcome::ReportOnly, Some(1_000));
        first.pattern_hash = Some("a".repeat(64));
        upsert_run(&conn, &first).unwrap();
        assert_eq!(get_run(&conn, "r1").unwrap().unwrap(), first);

        // The same pattern id, a different effective text: the rows differ.
        let mut second = run("r2", "L1", Outcome::ReportOnly, Some(2_000));
        second.pattern_hash = Some("b".repeat(64));
        upsert_run(&conn, &second).unwrap();
        let back = get_run(&conn, "r2").unwrap().unwrap();
        assert_eq!(back.pattern, first.pattern, "same pattern id");
        assert_ne!(
            back.pattern_hash, first.pattern_hash,
            "an edited pattern must be visible in the run row"
        );

        // An upsert of an existing row updates the digest in place.
        second.pattern_hash = Some("c".repeat(64));
        upsert_run(&conn, &second).unwrap();
        assert_eq!(
            get_run(&conn, "r2").unwrap().unwrap().pattern_hash,
            Some("c".repeat(64))
        );

        // A row from before the migration carries no digest.
        let plain = run("r3", "L1", Outcome::NoOp, None);
        assert_eq!(plain.pattern_hash, None);
        upsert_run(&conn, &plain).unwrap();
        assert_eq!(get_run(&conn, "r3").unwrap().unwrap().pattern_hash, None);
    }

    #[test]
    fn spend_inbox_decisions_and_activity() {
        let temp = tempfile::tempdir().unwrap();
        let db = fresh_store(temp.path());
        let conn = open_aux(&db).unwrap();
        let midnight = 1_000_000_000_000;
        // two started today, one yesterday, one blocked today
        upsert_run(
            &conn,
            &run("a", "L1", Outcome::ReportOnly, Some(midnight + 10)),
        )
        .unwrap();
        upsert_run(
            &conn,
            &run("b", "L1", Outcome::FixProposed, Some(midnight + 20)),
        )
        .unwrap();
        upsert_run(
            &conn,
            &run("c", "L1", Outcome::Escalated, Some(midnight - 20)),
        )
        .unwrap();
        let mut blocked = run("d", "L1", Outcome::Blocked, None);
        blocked.scheduled_ns = midnight + 30;
        upsert_run(&conn, &blocked).unwrap();
        upsert_run(&conn, &run("e", "L2", Outcome::Failed, Some(midnight + 40))).unwrap();
        let spend = spend_since(&conn, "L1", midnight).unwrap();
        assert_eq!(
            spend,
            Spend {
                runs: 2,
                tokens: 2_000,
                cost_usd: 0.5
            }
        );
        assert_eq!(
            spend_since(&conn, "L1", midnight + 100).unwrap(),
            Spend::default()
        );
        let waiting: Vec<String> = inbox(&conn).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(waiting, vec!["b", "c"], "newest first, undecided only");
        assert!(decide(&conn, "b", "applied", midnight + 99).unwrap());
        assert!(!decide(&conn, "zz", "applied", 1).unwrap());
        let waiting: Vec<String> = inbox(&conn).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(waiting, vec!["c"]);
        let b = get_run(&conn, "b").unwrap().unwrap();
        assert_eq!(b.decision.as_deref(), Some("applied"));
        assert_eq!(b.decided_ns, Some(midnight + 99));
        assert!(!b.in_inbox());
        // activity: completed, not blocked/failed, ended in the window
        assert_eq!(
            activity_count(&conn, "/ws", midnight).unwrap(),
            3,
            "c ended today"
        );
        assert_eq!(
            activity_count(&conn, "/ws", midnight + 15 + 5_000_000_000).unwrap(),
            1
        );
        assert_eq!(activity_count(&conn, "/ws", 0).unwrap(), 3);
        assert_eq!(activity_count(&conn, "/elsewhere", 0).unwrap(), 0);
        // the view aggregates per loop
        let (runs, fixes, blocked_n): (i64, i64, i64) = conn
            .query_row(
                "SELECT runs, fixes_proposed, blocked FROM loop_run_stats WHERE loop_id = 'L1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((runs, fixes, blocked_n), (4, 1, 1));
    }

    #[test]
    fn run_facts_read_the_launch_traces() {
        let temp = tempfile::tempdir().unwrap();
        let db = fresh_store(temp.path());
        let conn = open_aux(&db).unwrap();
        let policy = LoopPolicy {
            report_only: false,
            reason: None,
            state_file: "STATE.md".into(),
            run_log: "loop-run-log.md".into(),
            denylist: vec!["**/.env".into()],
            max_files: Some(10),
            worktree: Some("/ws/.loop-worktrees/r1".into()),
        };
        let meta = serde_json::json!({
            "loop_id": "L1", "loop_run_id": "r1", "loop_pattern": "ci-sweeper",
            "loop_level": "L2", "loop_workspace": "/ws",
            "loop_policy": serde_json::to_value(&policy).unwrap(),
        });
        conn.execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
                content_mode, correlation_plan, started_ns, agent_mux_version, metadata)
             VALUES ('lx', 'run-l', 1, 'Claude Code', 'claude', '/ws', '-ws', 'full', 'deterministic',
                1000, 'test', ?1)",
            params![meta.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns)
             VALUES ('claude:s1', 'claude', 's1', 1000, 1000)",
            [],
        )
        .unwrap();
        for (id, ordinal, output) in [
            ("t1", 1, "working"),
            ("t2", 2, "all done\n```loop-result\n{}\n```"),
        ] {
            conn.execute(
                "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, output)
                 VALUES (?1, 'claude:s1', 'lx', ?2, 'turn', 'closed', ?3, ?4)",
                params![id, ordinal, 1_000_000_000 * ordinal, output],
            )
            .unwrap();
        }
        let obs = |id: &str,
                   trace: &str,
                   typ: &str,
                   name: &str,
                   tool: Option<&str>,
                   input: Option<&str>,
                   output: Option<&str>,
                   path: Option<&str>| {
            conn.execute(
                "INSERT INTO observations (id, trace_id, type, name, tool_name, start_ns, input, output, path, total_tokens, total_cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, 10, ?6, ?7, ?8, 100, 0.01)",
                params![id, trace, typ, name, tool, input, output, path],
            )
            .unwrap();
        };
        obs(
            "o1",
            "t1",
            "tool",
            "Write",
            Some("Write"),
            Some(r#"{"file_path":"src/a.rs","content":"x"}"#),
            None,
            None,
        );
        obs(
            "o2",
            "t1",
            "tool",
            "Edit",
            Some("Edit"),
            Some(r#"{"file_path":"src/a.rs"}"#),
            None,
            None,
        );
        obs(
            "o3",
            "t1",
            "tool",
            "Read",
            Some("Read"),
            Some(r#"{"file_path":"README.md"}"#),
            None,
            None,
        );
        obs(
            "o4",
            "t2",
            "tool",
            "apply_patch",
            Some("apply_patch"),
            Some(
                r#"{"input":"*** Begin Patch\n*** Update File: docs/b.md\n*** Add File: docs/c.md\n*** End Patch"}"#,
            ),
            None,
            None,
        );
        obs(
            "o5",
            "t2",
            "tool",
            "NotebookEdit",
            None,
            Some(r#"{"notebook_path":"nb.ipynb"}"#),
            None,
            Some("nb.ipynb"),
        );
        obs(
            "o6",
            "t2",
            "agent",
            "loop-verifier",
            Some("Task"),
            None,
            Some("Checked the diff.\n## Verdict: REJECT — tests fail\n- cargo test red"),
            None,
        );
        obs(
            "o7",
            "t2",
            "agent",
            "loop-reviewer",
            Some("Task"),
            None,
            Some("Read the diff.\n## Verdict: APPROVE\n- minimal"),
            None,
        );
        let facts = run_facts(&conn, "lx").unwrap();
        assert_eq!(facts.tokens, Some(700), "the reviewer row adds its tokens");
        assert!(facts.cost_usd.unwrap() > 0.05);
        assert!(facts.verifier_ran);
        assert_eq!(facts.verifier_verdicts, vec!["REJECT", "APPROVE"]);
        assert_eq!(
            facts.verifier_verdict.as_deref(),
            Some("REJECT"),
            "one REJECT among the checkers is a REJECT"
        );
        assert_eq!(
            facts.files_touched,
            vec!["src/a.rs", "docs/b.md", "docs/c.md", "nb.ipynb"]
        );
        assert!(facts.final_message.unwrap().contains("loop-result"));
        let empty = run_facts(&conn, "absent").unwrap();
        assert!(!empty.verifier_ran && empty.files_touched.is_empty());
        let launch = loop_launch_of(&conn, "lx").unwrap().unwrap();
        assert_eq!(launch.run_id, "r1");
        assert_eq!(launch.level, Level::L2);
        assert_eq!(launch.policy, policy);
        assert!(loop_launch_of(&conn, "absent").unwrap().is_none());
        assert_eq!(parse_verdict("## Verdict: APPROVE"), Some("APPROVE".into()));
        assert_eq!(
            parse_verdict("## Verdict: ESCALATE_HUMAN (risky)"),
            Some("ESCALATE_HUMAN".into())
        );
        assert_eq!(parse_verdict("## Verdict: maybe"), Some("REJECT".into()));
        assert_eq!(parse_verdict("no verdict here"), None);
    }
}
