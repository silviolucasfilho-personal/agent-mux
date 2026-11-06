//! One swarm, one trace (coordinator-first spec §7): everything the store
//! knows about a swarm read as one tree. Every launch of a swarm carries
//! `launches.metadata.swarm_id` / `swarm_role` / `swarm_task_key` /
//! `parent_launch` (`bus::swarm::SwarmLaunch`), so the coordinator, the
//! workers, their turns and their sub-agents (`observations.type = 'agent'`)
//! are one query away; `swarm_events` is the log and `agent_messages` the
//! conversation. `agent-mux trace swarm <id> [--tree | --timeline | --log |
//! --json]`, `agent_mux_get_swarm`, the Swarm screen's Timeline and Log
//! tabs, the Langfuse summary trace and Heimdall's `swarms` dossier section
//! all read the [`SwarmReport`] this module builds.

use crate::bus::swarm::engine::Worker;
use crate::bus::swarm::record::{self, SwarmDecision, SwarmEvent, SwarmRun};
use crate::bus::swarm::view::{self, QuestionView, TaskView};
use crate::bus::{BusMessage, short_id, store as bstore};
use crate::tracing::cli::{fmt_cost, fmt_time, fmt_tokens};
use crate::tracing::view::{Window, axis, bar};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashMap;

/// One launch of the swarm: the coordinator or a worker, with its turns,
/// its sub-agents and its totals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaunchReport {
    pub launch_id: String,
    /// `coordinator` or `worker`.
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_launch: Option<String>,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub started_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
    pub turns: Vec<TurnReport>,
    pub tools: i64,
    pub errors: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Some generation had usage but no price (agy): the cost is a floor.
    pub unpriced: bool,
    pub sub_agents: Vec<AgentReport>,
}

/// One turn of a launch (a `traces` row with its stats).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnReport {
    pub id: String,
    pub ordinal: i64,
    pub name: String,
    pub status: String,
    pub start_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ns: Option<i64>,
    pub tools: i64,
    pub errors: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A sub-agent of a launch: an `agent` observation (a Claude `Task` run,
/// a Codex sub-agent) under one of its turns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentReport {
    pub id: String,
    pub trace_id: String,
    pub name: String,
    pub start_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One message of the swarm's conversation, its body cut to a line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageReport {
    pub id: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub topic: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub created_ns: i64,
}

/// The whole swarm as the store tells it.
#[derive(Debug, Clone, Serialize)]
pub struct SwarmReport {
    pub swarm: SwarmRun,
    pub gate_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<String>,
    pub done: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub unpriced: bool,
    /// The coordinator first, then the workers in plan order, then any
    /// other launch of the swarm (a reconnected worker's earlier session).
    pub launches: Vec<LaunchReport>,
    pub tasks: Vec<TaskView>,
    /// What waits for the operator, one line each.
    pub attention: Vec<String>,
    pub questions: Vec<QuestionView>,
    pub decisions: Vec<SwarmDecision>,
    pub events: Vec<SwarmEvent>,
    pub messages: Vec<MessageReport>,
}

/// The swarm's launches from `launches.metadata.swarm_id`, each with its
/// turns and sub-agents; the coordinator first, then by start.
pub fn launches(conn: &Connection, swarm_id: &str) -> rusqlite::Result<Vec<LaunchReport>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, json_extract(l.metadata, '$.swarm_role'),
                json_extract(l.metadata, '$.swarm_task'),
                json_extract(l.metadata, '$.swarm_task_key'),
                json_extract(l.metadata, '$.parent_launch'),
                l.provider, l.session_key, l.started_ns, l.ended_ns
         FROM launches l
         WHERE json_extract(l.metadata, '$.swarm_id') = ?1
         ORDER BY json_extract(l.metadata, '$.swarm_role') = 'coordinator' DESC, l.started_ns ASC",
    )?;
    let rows: Vec<LaunchReport> = stmt
        .query_map(params![swarm_id], |r| {
            Ok(LaunchReport {
                launch_id: r.get(0)?,
                role: r
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| "worker".into()),
                task_id: r.get(2)?,
                task_key: r.get(3)?,
                parent_launch: r.get(4)?,
                provider: r.get(5)?,
                session_key: r.get(6)?,
                started_ns: r.get(7)?,
                ended_ns: r.get(8)?,
                turns: Vec::new(),
                tools: 0,
                errors: 0,
                tokens: None,
                cost_usd: None,
                unpriced: false,
                sub_agents: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for mut l in rows {
        l.turns = turns(conn, &l.launch_id)?;
        l.sub_agents = sub_agents(conn, &l.launch_id)?;
        l.tools = l.turns.iter().map(|t| t.tools).sum();
        l.errors = l.turns.iter().map(|t| t.errors).sum();
        let tokens: i64 = l.turns.iter().filter_map(|t| t.tokens).sum();
        let cost: f64 = l.turns.iter().filter_map(|t| t.cost_usd).sum();
        l.tokens = (tokens > 0).then_some(tokens);
        l.cost_usd = (cost > 0.0).then_some(cost);
        l.unpriced = conn
            .query_row(
                "SELECT COALESCE(SUM(unpriced_generations), 0) > 0 FROM trace_stats WHERE launch_id = ?1",
                params![l.launch_id],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        out.push(l);
    }
    Ok(out)
}

fn turns(conn: &Connection, launch_id: &str) -> rusqlite::Result<Vec<TurnReport>> {
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, name, status, start_ns, end_ns, tool_count, error_count,
                total_tokens, total_cost_usd
         FROM trace_stats WHERE launch_id = ?1 ORDER BY ordinal, rid",
    )?;
    let rows = stmt.query_map(params![launch_id], |r| {
        Ok(TurnReport {
            id: r.get(0)?,
            ordinal: r.get(1)?,
            name: r.get(2)?,
            status: r.get(3)?,
            start_ns: r.get(4)?,
            end_ns: r.get(5)?,
            tools: r.get(6)?,
            errors: r.get(7)?,
            tokens: r.get(8)?,
            cost_usd: r.get(9)?,
        })
    })?;
    rows.collect()
}

fn sub_agents(conn: &Connection, launch_id: &str) -> rusqlite::Result<Vec<AgentReport>> {
    let mut stmt = conn.prepare(
        "SELECT o.id, o.trace_id, o.name, o.start_ns, o.end_ns, o.total_tokens, o.total_cost_usd
         FROM observations o JOIN traces t ON t.id = o.trace_id
         WHERE t.launch_id = ?1 AND o.type = 'agent' ORDER BY o.start_ns ASC LIMIT 200",
    )?;
    let rows = stmt.query_map(params![launch_id], |r| {
        Ok(AgentReport {
            id: r.get(0)?,
            trace_id: r.get(1)?,
            name: r.get(2)?,
            start_ns: r.get(3)?,
            end_ns: r.get(4)?,
            tokens: r.get(5)?,
            cost_usd: r.get(6)?,
        })
    })?;
    rows.collect()
}

fn message_report(m: &BusMessage) -> MessageReport {
    MessageReport {
        id: m.id.clone(),
        from: m.from_launch.clone(),
        to: m.to_launch.clone(),
        topic: m.topic.clone(),
        body: first_line(&m.body),
        reply_to: m.reply_to.clone(),
        created_ns: m.created_ns,
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

/// The swarm's messages: those between its launches (and the operator),
/// oldest first.
pub fn messages(conn: &Connection, swarm: &SwarmRun, launches: &[LaunchReport]) -> Vec<BusMessage> {
    let ids: Vec<&str> = launches.iter().map(|l| l.launch_id.as_str()).collect();
    bstore::list_workspace_messages(conn, &swarm.workspace, 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| {
            ids.contains(&m.from_launch.as_str())
                || m.to_launch.as_deref().is_some_and(|t| ids.contains(&t))
        })
        .collect()
}

/// Everything about one swarm. `workers` are the live sessions as the
/// caller knows them (the TUI's own, or `view::snapshot_workers()`); a task
/// whose worker is not among them while it runs reads as lost.
pub fn report(
    conn: &Connection,
    swarm: &SwarmRun,
    workers: &HashMap<String, Worker>,
    now_ns: i64,
) -> rusqlite::Result<SwarmReport> {
    let board = view::board_view(conn, swarm, workers, now_ns)?;
    let mut launches = launches(conn, &swarm.id)?;
    // workers in plan order after the coordinator; a launch of no task last
    let order: HashMap<&str, usize> = board
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.key.as_str(), i))
        .collect();
    launches.sort_by_key(|l| {
        (
            l.role != "coordinator",
            l.task_key
                .as_deref()
                .and_then(|k| order.get(k).copied())
                .unwrap_or(usize::MAX),
            l.started_ns,
        )
    });
    let tokens: i64 = launches.iter().filter_map(|l| l.tokens).sum();
    let cost: f64 = launches.iter().filter_map(|l| l.cost_usd).sum();
    let unpriced = launches.iter().any(|l| l.unpriced);
    let msgs = messages(conn, swarm, &launches);
    let attention = attention(swarm, &board);
    Ok(SwarmReport {
        gate_mode: board.gate_mode,
        coordinator: board.coordinator,
        done: board.done,
        total: board.total,
        tokens: (tokens > 0).then_some(tokens),
        cost_usd: (cost > 0.0).then_some(cost),
        unpriced,
        launches,
        tasks: board.tasks,
        attention,
        questions: board.questions,
        decisions: record::decisions(conn, &swarm.id)?,
        events: record::recent_events(conn, &swarm.id, 1000)?,
        messages: msgs.iter().map(message_report).collect(),
        swarm: swarm.clone(),
    })
}

/// What waits for the operator, from the board: the plan's confirmation,
/// questions, failed or lost tasks, branches to review, the final word.
fn attention(swarm: &SwarmRun, board: &view::BoardView) -> Vec<String> {
    use crate::bus::swarm::record::SwarmStatus;
    let mut out = Vec::new();
    if swarm.status == SwarmStatus::Review {
        out.push("the plan waits for your go in the coordinator's chat".to_string());
    }
    for t in &board.tasks {
        match t.state.as_str() {
            "asking" => out.push(format!(
                "{} asks: {}",
                t.key,
                t.question.as_deref().unwrap_or("")
            )),
            "prompt" => out.push(format!("{} waits on a prompt in its terminal", t.key)),
            "failed" => out.push(format!(
                "{} failed: {}",
                t.key,
                t.error.as_deref().map(first_line).unwrap_or_default()
            )),
            "lost" => out.push(format!("{} lost its worker", t.key)),
            "stalled" => out.push(format!("{} is stalled", t.key)),
            "review" => out.push(format!(
                "{} waits for review: {}",
                t.key,
                t.branch.as_deref().unwrap_or("")
            )),
            "proposed" => out.push(format!("{} was proposed by a worker", t.key)),
            _ => {}
        }
    }
    if matches!(swarm.status, SwarmStatus::Finished) && swarm.ended_ns.is_none() {
        out.push("every task is settled: close the swarm".to_string());
    }
    out
}

/// Swarms of `workspace` (every workspace with `None`) that ran within the
/// window: live ones and those that ended at or after `since_ns`, newest
/// first, at most `limit`.
pub fn swarms_since(
    conn: &Connection,
    workspace: Option<&str>,
    since_ns: i64,
    limit: usize,
) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut all = record::live_swarms(conn)?;
    all.extend(record::ended_swarms_since(conn, since_ns)?);
    if let Some(ws) = workspace {
        let norm = bstore::normalize_workspace(std::path::Path::new(ws));
        all.retain(|s| s.workspace == norm || s.workspace == ws);
    }
    all.sort_by_key(|s| std::cmp::Reverse(s.started_ns));
    all.dedup_by(|a, b| a.id == b.id);
    all.truncate(limit);
    Ok(all)
}

// ---------------------------------------------------------------------------
// The tree

fn glyph(state: &str) -> &'static str {
    match state {
        "done" => "✓",
        "working" => "●",
        "starting" => "◌",
        "asking" => "◷",
        "review" => "⇡",
        "ready" => "◦",
        "blocked" => "⋯",
        "cancelled" | "set-aside" => "–",
        "failed" | "lost" | "stalled" | "prompt" => "!",
        _ => "·",
    }
}

fn span(start_ns: i64, end_ns: Option<i64>, now_ns: i64) -> String {
    let from = &fmt_time(start_ns)[11..16];
    match end_ns {
        Some(e) => format!(
            "{from}–{} ({})",
            &fmt_time(e)[11..16],
            crate::bus::swarm::short_age(e - start_ns)
        ),
        None => format!(
            "{from}– ({})",
            crate::bus::swarm::short_age(now_ns - start_ns)
        ),
    }
}

fn cost_label(cost: Option<f64>, unpriced: bool) -> String {
    match (cost, unpriced) {
        (None, true) => "≥$0".into(),
        (None, false) => "—".into(),
        (Some(c), true) => format!("≥{}", fmt_cost(Some(c))),
        (Some(c), false) => fmt_cost(Some(c)),
    }
}

/// The swarm as a tree of lines: the swarm, `★` the coordinator, each
/// worker with its task's state and `↳` its sub-agents.
pub fn tree_lines(r: &SwarmReport, now_ns: i64) -> Vec<String> {
    let s = &r.swarm;
    let state = if s.ended_ns.is_some() {
        "closed"
    } else {
        s.status.as_str()
    };
    let mut lines = vec![
        format!(
            "swarm {}  {state} · {}/{} done · {} tok · {} · {}",
            s.id,
            r.done,
            r.total,
            fmt_tokens(r.tokens),
            cost_label(r.cost_usd, r.unpriced),
            span(s.started_ns, s.ended_ns, now_ns)
        ),
        format!("  goal: {}", first_line(&s.goal)),
        format!(
            "  workspace: {} · gate {} · cap {} · {} launch(es) · {} decision(s) · {} event(s)",
            s.workspace,
            r.gate_mode,
            s.max_workers,
            r.launches.len(),
            r.decisions.len(),
            r.events.len()
        ),
    ];
    if let Some(d) = r
        .decisions
        .iter()
        .find(|d| d.verb == record::SwarmVerb::Launch)
    {
        lines.push(format!(
            "  confirmed {} by {}{}",
            &fmt_time(d.created_ns)[11..16],
            d.by.as_str(),
            d.said
                .as_deref()
                .map(|w| format!(" “{w}”"))
                .unwrap_or_default()
        ));
    }
    let task_of = |l: &LaunchReport| -> Option<&TaskView> {
        r.tasks.iter().find(|t| {
            Some(t.id.as_str()) == l.task_id.as_deref()
                || t.worker.as_deref() == Some(l.launch_id.as_str())
        })
    };
    for l in &r.launches {
        let (mark, head) = match (l.role.as_str(), task_of(l)) {
            ("coordinator", _) => ("★".to_string(), "coordinator".to_string()),
            (_, Some(t)) => (
                glyph(&t.state).to_string(),
                format!("{} {} · {}", t.key, t.title, t.state),
            ),
            (_, None) => (
                "·".to_string(),
                l.task_key.clone().unwrap_or_else(|| "worker".into()),
            ),
        };
        lines.push(format!(
            "  {mark} {head}  {} {}  {} turn(s) · {} tool(s) · {} tok · {} · {}",
            l.provider,
            short_id(&l.launch_id),
            l.turns.len(),
            l.tools,
            fmt_tokens(l.tokens),
            cost_label(l.cost_usd, l.unpriced),
            span(l.started_ns, l.ended_ns, now_ns)
        ));
        for a in &l.sub_agents {
            lines.push(format!(
                "      ↳ {}  {}  {}",
                a.name,
                fmt_tokens(a.tokens),
                span(a.start_ns, a.end_ns, now_ns)
            ));
        }
    }
    for t in r.tasks.iter().filter(|t| t.worker.is_none()) {
        lines.push(format!(
            "  {} {} {} · {}{}",
            glyph(&t.state),
            t.key,
            t.title,
            t.state,
            t.harness
                .as_deref()
                .map(|h| format!(" · {h}"))
                .unwrap_or_default()
        ));
    }
    if let Some(c) = &s.check_cmd {
        lines.push(format!(
            "  check: {c} → {}",
            s.check_result.as_deref().unwrap_or("not run")
        ));
    }
    if !r.attention.is_empty() {
        lines.push(String::new());
        lines.push(format!("  needs you ({}):", r.attention.len()));
        for a in &r.attention {
            lines.push(format!("    ! {a}"));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// The timeline

/// A mark on a lane: a decision, a task event, a message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mark {
    pub ns: i64,
    pub glyph: char,
    pub label: String,
}

/// One lane of the timeline: the swarm, the coordinator, a worker, a
/// sub-agent (`depth` 1) or the check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lane {
    pub label: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub start_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ns: Option<i64>,
    /// Turns: when the agent was working.
    pub working: Vec<(i64, Option<i64>)>,
    /// From a question until its answer.
    pub asking: Vec<(i64, Option<i64>)>,
    pub marks: Vec<Mark>,
    /// The task's state word, for the lane's colour.
    pub state: String,
}

/// The whole timeline: lanes against one window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timeline {
    pub window: Window,
    pub lanes: Vec<Lane>,
}

/// `█` turn · `░` alive · `?` asking · the marks' glyphs.
pub const LEGEND: &str = "█ working  ░ alive  ? asking  ▲ launch  ◆ merge  ✓ done  ✗ failed  │ message  ▤ plan  ■ closed";

fn mark_for(e: &SwarmEvent) -> Option<(char, String)> {
    let detail = |k: &str| {
        e.detail
            .as_ref()
            .and_then(|d| d.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    Some(match e.kind.as_str() {
        "plan.proposed" | "plan.revised" => ('▤', e.kind.clone()),
        "decision.launch" => ('▲', format!("launch {}", detail("said"))),
        "decision.merge" => ('◆', "merge".into()),
        "decision.set-aside" => ('◇', "set aside".into()),
        "task.spawned" | "task.reconnected" => ('▲', format!("{} {}", e.kind, detail("by"))),
        "task.done" => ('✓', "done".into()),
        "task.failed" | "task.spawn_failed" => ('✗', e.kind.clone()),
        "task.cancelled" => ('–', "cancelled".into()),
        "task.nudged" => ('·', "nudged".into()),
        "message" | "question.asked" | "question.answered" => ('│', detail("topic")),
        "check.started" => ('▶', "check".into()),
        "check.done" => ('■', format!("check {}", detail("result"))),
        "closed" => ('■', "closed".into()),
        _ => return None,
    })
}

/// The timeline of a report: one lane per agent, the coordinator first,
/// workers in plan order, sub-agents under their worker; marks from the
/// log; the check last.
pub fn timeline(r: &SwarmReport, now_ns: i64) -> Timeline {
    let s = &r.swarm;
    let mut end = s.ended_ns.unwrap_or(now_ns);
    for l in &r.launches {
        end = end.max(l.ended_ns.unwrap_or(now_ns));
    }
    let start = r
        .launches
        .iter()
        .map(|l| l.started_ns)
        .chain(std::iter::once(s.started_ns))
        .min()
        .unwrap_or(s.started_ns);
    let window = Window {
        start_ns: start,
        end_ns: end.max(start + 1),
    };
    let task_of = |l: &LaunchReport| -> Option<&TaskView> {
        r.tasks.iter().find(|t| {
            Some(t.id.as_str()) == l.task_id.as_deref()
                || t.worker.as_deref() == Some(l.launch_id.as_str())
        })
    };
    // open questions per asker: (asked, answered)
    let mut asking: HashMap<String, Vec<(i64, Option<i64>)>> = HashMap::new();
    for q in r.messages.iter().filter(|m| m.topic == "question") {
        let answered = r
            .messages
            .iter()
            .filter(|m| m.created_ns >= q.created_ns && m.id != q.id)
            .find(|m| {
                m.reply_to.as_deref() == Some(q.id.as_str())
                    || (m.to.as_deref() == Some(q.from.as_str())
                        && q.to.as_ref().is_none_or(|to| *to == m.from))
            })
            .map(|m| m.created_ns);
        asking
            .entry(q.from.clone())
            .or_default()
            .push((q.created_ns, answered));
    }
    let mut lanes = Vec::new();
    // the swarm itself: plan, launch, closed
    let mut swarm_lane = Lane {
        label: format!("◆ {}", first_line(&s.goal)),
        depth: 0,
        launch: None,
        task_id: None,
        start_ns: s.started_ns,
        end_ns: s.ended_ns,
        working: Vec::new(),
        asking: Vec::new(),
        marks: Vec::new(),
        state: if s.ended_ns.is_some() {
            "closed".into()
        } else {
            s.status.as_str().to_string()
        },
    };
    for e in &r.events {
        if e.task_id.is_none()
            && !e.kind.starts_with("check.")
            && !matches!(
                e.kind.as_str(),
                "message" | "question.asked" | "question.answered"
            )
            && let Some((g, label)) = mark_for(e)
        {
            swarm_lane.marks.push(Mark {
                ns: e.created_ns,
                glyph: g,
                label,
            });
        }
    }
    lanes.push(swarm_lane);
    for l in &r.launches {
        let task = task_of(l);
        let label = match (l.role.as_str(), task) {
            ("coordinator", _) => format!("★ coordinator · {}", l.provider),
            (_, Some(t)) => format!("{} {} · {}", glyph(&t.state), t.key, l.provider),
            (_, None) => format!(
                "· {} · {}",
                l.task_key.as_deref().unwrap_or("worker"),
                l.provider
            ),
        };
        let mut lane = Lane {
            label,
            depth: 0,
            launch: Some(l.launch_id.clone()),
            task_id: task.map(|t| t.id.clone()).or_else(|| l.task_id.clone()),
            start_ns: l.started_ns,
            end_ns: l.ended_ns,
            working: l.turns.iter().map(|t| (t.start_ns, t.end_ns)).collect(),
            asking: asking.get(&l.launch_id).cloned().unwrap_or_default(),
            marks: Vec::new(),
            state: task
                .map(|t| t.state.clone())
                .unwrap_or_else(|| l.role.clone()),
        };
        for e in &r.events {
            let mine = match e.kind.as_str() {
                "message" | "question.asked" | "question.answered" => {
                    e.actor.as_deref() == Some(l.launch_id.as_str())
                }
                _ => e.task_id.is_some() && e.task_id == lane.task_id,
            };
            if mine && let Some((g, label)) = mark_for(e) {
                lane.marks.push(Mark {
                    ns: e.created_ns,
                    glyph: g,
                    label,
                });
            }
        }
        lanes.push(lane);
        for a in &l.sub_agents {
            lanes.push(Lane {
                label: format!("↳ {}", a.name),
                depth: 1,
                launch: Some(l.launch_id.clone()),
                task_id: lane_task(l, task),
                start_ns: a.start_ns,
                end_ns: a.end_ns,
                working: vec![(a.start_ns, a.end_ns)],
                asking: Vec::new(),
                marks: Vec::new(),
                state: if a.end_ns.is_some() {
                    "done".into()
                } else {
                    "working".into()
                },
            });
        }
    }
    // tasks nobody started yet, so the plan is whole
    for t in r.tasks.iter().filter(|t| t.worker.is_none()) {
        lanes.push(Lane {
            label: format!("{} {} · {}", glyph(&t.state), t.key, t.state),
            depth: 0,
            launch: None,
            task_id: Some(t.id.clone()),
            start_ns: s.started_ns,
            end_ns: Some(s.started_ns),
            working: Vec::new(),
            asking: Vec::new(),
            marks: Vec::new(),
            state: t.state.clone(),
        });
    }
    // the check, last
    let started = r.events.iter().find(|e| e.kind == "check.started");
    if let Some(c) = started {
        let done = r
            .events
            .iter()
            .find(|e| e.kind == "check.done" && e.created_ns >= c.created_ns);
        lanes.push(Lane {
            label: format!(
                "▶ check {}",
                s.check_cmd.as_deref().map(first_line).unwrap_or_default()
            ),
            depth: 0,
            launch: None,
            task_id: None,
            start_ns: c.created_ns,
            end_ns: done.map(|d| d.created_ns),
            working: vec![(c.created_ns, done.map(|d| d.created_ns))],
            asking: Vec::new(),
            marks: done.and_then(mark_for).map_or(Vec::new(), |(g, label)| {
                vec![Mark {
                    ns: done.map(|d| d.created_ns).unwrap_or(c.created_ns),
                    glyph: g,
                    label,
                }]
            }),
            state: s
                .check_result
                .as_deref()
                .map(|r| {
                    if r.starts_with("ok") {
                        "done"
                    } else {
                        "failed"
                    }
                })
                .unwrap_or("working")
                .to_string(),
        });
    }
    Timeline { window, lanes }
}

fn lane_task(l: &LaunchReport, task: Option<&TaskView>) -> Option<String> {
    task.map(|t| t.id.clone()).or_else(|| l.task_id.clone())
}

impl Timeline {
    /// The part of the window a `zoom`× magnification shows, panned by
    /// `pan` steps of a tenth of the shown span each (clamped inside).
    pub fn view(&self, zoom: u32, pan: i32) -> Window {
        let zoom = i64::from(zoom.clamp(1, 64));
        let span = (self.window.span_ns() / zoom).max(1);
        let step = (span / 10).max(1);
        let max_start = self.window.end_ns - span;
        let start = (self.window.start_ns + i64::from(pan) * step)
            .clamp(self.window.start_ns, max_start.max(self.window.start_ns));
        Window {
            start_ns: start,
            end_ns: (start + span).min(self.window.end_ns).max(start + 1),
        }
    }
}

/// One lane's track as cells: `░` alive, `█` working, `?` asking, then the
/// marks on top; `▶` at the right edge while it runs past the window.
pub fn lane_cells(lane: &Lane, w: &Window, cols: usize, now_ns: i64) -> Vec<char> {
    let mut cells = vec![' '; cols];
    if cols == 0 {
        return cells;
    }
    let paint = |cells: &mut Vec<char>, start: i64, end: Option<i64>, ch: char| {
        let end_eff = end.unwrap_or(now_ns).max(start);
        if end_eff < w.start_ns || start > w.end_ns {
            return;
        }
        let b = bar(start.max(w.start_ns), Some(end_eff.min(w.end_ns)), w, cols);
        for c in cells.iter_mut().skip(b.offset).take(b.width.max(1)) {
            *c = ch;
        }
    };
    let alive_end = lane.end_ns;
    if lane.end_ns.is_none_or(|e| e > lane.start_ns) {
        paint(&mut cells, lane.start_ns, alive_end, '░');
    }
    for (s, e) in &lane.working {
        paint(&mut cells, *s, *e, '█');
    }
    for (s, e) in &lane.asking {
        paint(&mut cells, *s, *e, '?');
    }
    if lane.end_ns.is_none() && now_ns >= w.end_ns {
        cells[cols - 1] = '▶';
    }
    for m in &lane.marks {
        if m.ns >= w.start_ns && m.ns <= w.end_ns {
            let b = bar(m.ns, Some(m.ns), w, cols);
            cells[b.offset.min(cols - 1)] = m.glyph;
        }
    }
    cells
}

/// The timeline as plain lines: the axis, one line per lane, the legend.
pub fn timeline_lines(tl: &Timeline, label_width: usize, track: usize, now_ns: i64) -> Vec<String> {
    let w = tl.window;
    let mut lines = vec![format!("{:<label_width$} {}", "", axis(&w, track))];
    for lane in &tl.lanes {
        let label = format!("{}{}", "  ".repeat(lane.depth.min(3)), lane.label);
        let cells: String = lane_cells(lane, &w, track, now_ns).into_iter().collect();
        lines.push(format!(
            "{:<label_width$} {}",
            truncate(&label, label_width),
            cells
        ));
    }
    lines.push(String::new());
    lines.push(format!("{:<label_width$} {LEGEND}", ""));
    lines
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

// ---------------------------------------------------------------------------
// The log

/// Which events a log view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogKind {
    #[default]
    All,
    /// Plan, launch, the operator's verbs, the close.
    Decisions,
    /// `task.*` and the check.
    Tasks,
    /// Messages and questions.
    Messages,
}

impl LogKind {
    pub const ALL: [LogKind; 4] = [
        LogKind::All,
        LogKind::Decisions,
        LogKind::Tasks,
        LogKind::Messages,
    ];
    pub fn label(self) -> &'static str {
        match self {
            LogKind::All => "all",
            LogKind::Decisions => "decisions",
            LogKind::Tasks => "tasks",
            LogKind::Messages => "messages",
        }
    }
    pub fn next(self) -> LogKind {
        let i = LogKind::ALL.iter().position(|k| *k == self).unwrap_or(0);
        LogKind::ALL[(i + 1) % LogKind::ALL.len()]
    }
    pub fn admits(self, e: &SwarmEvent) -> bool {
        let k = e.kind.as_str();
        match self {
            LogKind::All => true,
            LogKind::Decisions => {
                k.starts_with("plan.")
                    || k.starts_with("decision.")
                    || k.starts_with("swarm.")
                    || matches!(k, "started" | "closed")
            }
            LogKind::Tasks => k.starts_with("task.") || k.starts_with("check."),
            LogKind::Messages => matches!(k, "message" | "question.asked" | "question.answered"),
        }
    }
}

/// The log's lines, oldest first, filtered by kind and by text.
pub fn log_lines<'a>(
    events: impl IntoIterator<Item = &'a SwarmEvent>,
    kind: LogKind,
    filter: &str,
) -> Vec<String> {
    let needle = filter.trim().to_lowercase();
    events
        .into_iter()
        .filter(|e| kind.admits(e))
        .map(view::event_line)
        .filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle))
        .collect()
}

// ---------------------------------------------------------------------------
// One launch's trace (the worker Trace tab, `trace show` without a turn)

/// A launch's turns and their observations as lines: a header per turn,
/// its tools, generations and sub-agents indented, with duration, tokens
/// and cost. `agent` names only the sub-agent whose rows to show.
pub fn launch_trace_lines(
    conn: &Connection,
    launch_id: &str,
    agent: Option<&str>,
) -> rusqlite::Result<Vec<String>> {
    use crate::tracing::store::query;
    let turns = turns(conn, launch_id)?;
    let mut lines = Vec::new();
    if turns.is_empty() {
        lines.push("  No turns recorded for this session yet.".into());
        return Ok(lines);
    }
    for t in &turns {
        let obs = query::list_observations(conn, &t.id)?;
        let obs: Vec<&query::ObservationView> = match agent {
            None => obs.iter().collect(),
            Some(id) => {
                // the sub-agent's row and everything under it
                let Some(at) = obs.iter().position(|o| o.id == id) else {
                    continue;
                };
                let depth = obs[at].depth;
                let mut out = vec![&obs[at]];
                out.extend(obs[at + 1..].iter().take_while(|o| o.depth > depth));
                out
            }
        };
        if agent.is_some() && obs.is_empty() {
            continue;
        }
        lines.push(format!(
            "turn {} {:<8} {}  {} tok · {} · {} tool(s){}  {}",
            t.ordinal,
            t.status,
            &fmt_time(t.start_ns)[11..],
            fmt_tokens(t.tokens),
            fmt_cost(t.cost_usd),
            t.tools,
            if t.errors > 0 {
                format!(" · {} error(s)", t.errors)
            } else {
                String::new()
            },
            first_line(&t.name).chars().take(60).collect::<String>()
        ));
        for o in obs {
            let duration = match o.end_ns {
                Some(e) => crate::tracing::cli::fmt_ms((e - o.start_ns) / 1_000_000),
                None => "running".into(),
            };
            lines.push(format!(
                "  {:<8} {:<10} {:>8} {:>7} {:>8}  {}{}{}",
                &fmt_time(o.start_ns)[11..19],
                o.obs_type,
                duration,
                fmt_tokens(o.total_tokens),
                fmt_cost(o.total_cost_usd),
                "  ".repeat(o.depth.min(4)),
                o.name,
                match (o.level.as_str(), &o.status_message) {
                    ("ERROR", Some(m)) => format!("  ✗ {}", first_line(m)),
                    ("ERROR", None) => "  ✗".to_string(),
                    _ => String::new(),
                }
            ));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::swarm::record::{NewSwarm, SwarmStatus};
    use crate::bus::swarm::{SwarmLaunch, SwarmRole};
    use crate::bus::{TaskIsolation, TaskStatus};
    use crate::tracing::store::model::{
        LaunchRow, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
    };

    fn launch_row(id: &str, started_ns: i64, sw: &SwarmLaunch) -> LaunchRow {
        let mut meta = serde_json::Map::new();
        sw.metadata(&mut meta);
        LaunchRow {
            id: id.into(),
            run_id: "default-run".into(),
            agent_mux_session: 1,
            profile: "default".into(),
            provider: "claude".into(),
            cwd: "/ws".into(),
            project_slug: "ws".into(),
            content_mode: "full".into(),
            correlation_plan: "known".into(),
            correlation: None,
            session_key: Some(format!("claude:{id}")),
            injected_session_id: true,
            attached: false,
            started_ns,
            ended_ns: None,
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: Vec::new(),
            metadata: Some(serde_json::Value::Object(meta)),
        }
    }

    fn turn(id: &str, launch: &str, start_ns: i64, end_ns: i64) -> TraceRow {
        TraceRow {
            id: id.into(),
            session_key: format!("claude:{launch}"),
            launch_id: Some(launch.into()),
            ordinal: 1,
            name: format!("turn of {launch}"),
            status: TraceStatus::Closed,
            start_ns,
            end_ns: Some(end_ns),
            input: None,
            output: None,
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            metadata: None,
            provider: "claude".into(),
            session_id: launch.into(),
        }
    }

    fn agent_row(id: &str, trace: &str, start_ns: i64, end_ns: i64) -> ObservationRow {
        ObservationRow {
            id: id.into(),
            trace_id: trace.into(),
            parent_id: None,
            obs_type: ObservationType::Agent,
            name: "Task: write tests".into(),
            kind: Some("subagent".into()),
            start_ns,
            end_ns: Some(end_ns),
            level: crate::tracing::store::model::Level::Default,
            status_message: None,
            model: None,
            input: None,
            output: None,
            thinking: None,
            usage_raw: None,
            usage: None,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            ts_approx: false,
            metadata: serde_json::Map::new(),
        }
    }

    /// A swarm with a coordinator, one worker with a turn and a sub-agent,
    /// a confirmed launch, a done task and a message.
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, SwarmRun) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let mut store =
            crate::tracing::store::open_rw(&db, crate::tracing::store::OpenOptions::default())
                .unwrap();
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        let t0 = 1_700_000_000_000_000_000i64;
        let swarm = record::create_swarm(
            &conn,
            None,
            &NewSwarm {
                workspace: "/ws".into(),
                goal: "Ship the follow-ups".into(),
                planner_harness: "claude".into(),
                max_workers: 2,
                isolation: TaskIsolation::Shared,
                gate: true,
                check_cmd: Some("cargo test".into()),
                owner_run: None,
                worker_pool: Vec::new(),
                headless: false,
            },
            t0,
        )
        .unwrap();
        record::set_coordinator_launch(&conn, None, &swarm.id, "L-coord", t0).unwrap();
        let env = crate::bus::swarm::plan::CheckEnv {
            harnesses: vec!["claude".into()],
            agents: Vec::new(),
            writing_agents: Vec::new(),
            git_repo: false,
            default_isolation: TaskIsolation::Shared,
            pool: Vec::new(),
        };
        let plan = crate::bus::swarm::plan::Plan {
            summary: "one task".into(),
            tasks: vec![crate::bus::swarm::plan::PlannedTask {
                key: "schema".into(),
                title: "Schema".into(),
                instructions: "do it".into(),
                acceptance: None,
                files: Vec::new(),
                depends_on: Vec::new(),
                harness: None,
                agent: None,
                isolation: None,
            }],
        };
        record::submit_plan(&conn, None, &swarm, &plan, &env, t0 + 1_000_000_000).unwrap();
        let swarm = record::get_swarm(&conn, &swarm.id).unwrap().unwrap();
        let board = record::board(&conn, &swarm.id).unwrap();
        let task_id = board[0].task.id.clone();
        let who = record::Decider::chat(Some("go"), Some("L-coord"));
        record::apply(
            &conn,
            None,
            &swarm,
            record::SwarmVerb::Launch,
            None,
            &env,
            &who,
            t0 + 2_000_000_000,
        )
        .unwrap();
        // the launches, as the TUI writes them
        for op in [
            StoreOp::Launch(launch_row(
                "L-coord",
                t0,
                &SwarmLaunch::coordinator(&swarm.id, None),
            )),
            StoreOp::Launch(launch_row(
                "L-w1",
                t0 + 3_000_000_000,
                &SwarmLaunch::worker(&swarm.id, &task_id, "schema", Some("L-coord".into())),
            )),
            StoreOp::Trace(turn("T-c1", "L-coord", t0, t0 + 1_000_000_000)),
            StoreOp::Trace(turn("T-w1", "L-w1", t0 + 4_000_000_000, t0 + 9_000_000_000)),
            StoreOp::Observation(agent_row(
                "O-a1",
                "T-w1",
                t0 + 5_000_000_000,
                t0 + 7_000_000_000,
            )),
        ] {
            assert_eq!(store.apply(&[op]).unwrap(), 0, "the store refused an op");
        }
        bstore::update_task(
            &conn,
            None,
            &task_id,
            "L-coord",
            TaskStatus::Claimed,
            None,
            None,
            None,
            None,
            t0 + 3_000_000_000,
        )
        .unwrap();
        conn.execute(
            "UPDATE agent_tasks SET assigned_to = 'L-w1', status = 'running' WHERE id = ?1",
            params![task_id],
        )
        .unwrap();
        bstore::send_message(
            &conn,
            None,
            "/ws",
            "L-w1",
            Some("L-coord"),
            "question",
            "which provider first?",
            None,
            t0 + 6_000_000_000,
        )
        .unwrap();
        bstore::send_message(
            &conn,
            None,
            "/ws",
            "L-coord",
            Some("L-w1"),
            "answer",
            "github",
            None,
            t0 + 6_500_000_000,
        )
        .unwrap();
        bstore::update_task(
            &conn,
            None,
            &task_id,
            "L-w1",
            TaskStatus::Done,
            Some(serde_json::json!("schema written")),
            None,
            None,
            None,
            t0 + 9_000_000_000,
        )
        .unwrap();
        let swarm = record::get_swarm(&conn, &swarm.id).unwrap().unwrap();
        assert_eq!(swarm.status, SwarmStatus::Running);
        (dir, db, swarm)
    }

    #[test]
    fn a_swarm_reads_as_one_tree_with_its_turns_sub_agents_log_and_timeline() {
        let (_dir, db, swarm) = fixture();
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        let now = swarm.started_ns + 20_000_000_000;
        let r = report(&conn, &swarm, &HashMap::new(), now).unwrap();
        assert_eq!(r.launches.len(), 2, "{:?}", r.launches);
        assert_eq!(r.launches[0].role, "coordinator");
        assert_eq!(r.launches[0].launch_id, "L-coord");
        assert_eq!(r.launches[1].task_key.as_deref(), Some("schema"));
        assert_eq!(r.launches[1].turns.len(), 1);
        assert_eq!(r.launches[1].sub_agents.len(), 1);
        assert_eq!(r.launches[1].sub_agents[0].name, "Task: write tests");
        assert_eq!(r.launches[1].parent_launch.as_deref(), Some("L-coord"));
        assert_eq!((r.done, r.total), (1, 1));
        assert_eq!(r.tasks[0].state, "done");
        assert_eq!(r.decisions.len(), 1);
        assert_eq!(r.decisions[0].said.as_deref(), Some("go"));
        let kinds: Vec<&str> = r.events.iter().map(|e| e.kind.as_str()).collect();
        for want in [
            "plan.proposed",
            "decision.launch",
            "question.asked",
            "question.answered",
            "task.done",
        ] {
            assert!(kinds.contains(&want), "missing {want} in {kinds:?}");
        }
        // the question, its answer and the worker's `task-update` to the coordinator
        assert_eq!(r.messages.len(), 3);

        let tree = tree_lines(&r, now).join("\n");
        for want in [
            "swarm swarm-",
            "running · 1/1 done",
            "★ coordinator  claude",
            "✓ schema Schema · done",
            "↳ Task: write tests",
            "confirmed",
            "“go”",
            "check: cargo test → not run",
        ] {
            assert!(tree.contains(want), "missing {want:?} in:\n{tree}");
        }

        let tl = timeline(&r, now);
        let labels: Vec<&str> = tl.lanes.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels[0], "◆ Ship the follow-ups");
        assert_eq!(labels[1], "★ coordinator · claude");
        assert_eq!(labels[2], "✓ schema · claude");
        assert_eq!(labels[3], "↳ Task: write tests");
        assert_eq!(tl.lanes[3].depth, 1);
        assert!(
            tl.lanes[0]
                .marks
                .iter()
                .any(|m| m.glyph == '▲' && m.label == "launch go"),
            "{:?}",
            tl.lanes[0].marks
        );
        assert!(
            tl.lanes[2].marks.iter().any(|m| m.glyph == '✓'),
            "{:?}",
            tl.lanes[2].marks
        );
        assert!(
            tl.lanes[2].marks.iter().any(|m| m.glyph == '│'),
            "{:?}",
            tl.lanes[2].marks
        );
        assert_eq!(tl.lanes[2].asking.len(), 1, "a question until its answer");
        assert!(tl.lanes[2].asking[0].1.is_some());
        let lines = timeline_lines(&tl, 28, 60, now);
        // the axis, then the swarm, the coordinator, the worker, its sub-agent
        assert!(
            lines[3].contains("█"),
            "the worker's turn is a block:\n{}",
            lines.join("\n")
        );
        assert!(
            lines[3].contains('?'),
            "its question shows:\n{}",
            lines.join("\n")
        );
        assert!(
            lines[3].contains('✓'),
            "its end is marked:\n{}",
            lines.join("\n")
        );
        assert!(
            lines[4].starts_with("  ↳ Task"),
            "the sub-agent is indented:\n{}",
            lines.join("\n")
        );
        assert!(lines.last().unwrap().contains(LEGEND));
        // zoom and pan stay inside the window
        let v = tl.view(4, 100);
        assert!(v.start_ns >= tl.window.start_ns && v.end_ns <= tl.window.end_ns);
        assert!(v.span_ns() <= tl.window.span_ns() / 4 + 1);

        let log = log_lines(&r.events, LogKind::Decisions, "");
        assert!(
            log.iter()
                .all(|l| l.contains("plan.") || l.contains("decision.") || l.contains("started")),
            "{log:?}"
        );
        let log = log_lines(&r.events, LogKind::Tasks, "done");
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(log[0].contains("task.done"));
        assert_eq!(log_lines(&r.events, LogKind::Messages, "").len(), 3);

        let trace = launch_trace_lines(&conn, "L-w1", None).unwrap().join("\n");
        assert!(trace.contains("turn 1 closed"), "{trace}");
        assert!(trace.contains("Task: write tests"), "{trace}");
        let only = launch_trace_lines(&conn, "L-w1", Some("O-a1")).unwrap();
        assert_eq!(
            only.len(),
            2,
            "the turn header and the agent's row: {only:?}"
        );
        assert!(launch_trace_lines(&conn, "L-none", None).unwrap()[0].contains("No turns"));
    }

    #[test]
    fn swarms_since_lists_live_and_recently_ended_ones_of_a_workspace() {
        let (_dir, db, swarm) = fixture();
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        let all = swarms_since(&conn, Some("/ws"), 0, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, swarm.id);
        assert!(
            swarms_since(&conn, Some("/elsewhere"), 0, 10)
                .unwrap()
                .is_empty()
        );
        record::end_swarm(&conn, None, &swarm, swarm.started_ns + 30_000_000_000).unwrap();
        assert_eq!(
            swarms_since(&conn, None, swarm.started_ns, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            swarms_since(&conn, None, swarm.started_ns + 60_000_000_000, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn swarm_launch_metadata_round_trips() {
        let sw = SwarmLaunch::worker("swarm-1", "task-1", "schema", Some("L-c".into()));
        let mut meta = serde_json::Map::new();
        sw.metadata(&mut meta);
        let back = SwarmLaunch::from_metadata(&serde_json::Value::Object(meta)).unwrap();
        assert_eq!(back, sw);
        assert_eq!(back.role, SwarmRole::Worker);
        assert!(SwarmLaunch::from_metadata(&serde_json::json!({"loop_run_id": "x"})).is_none());
    }
}
