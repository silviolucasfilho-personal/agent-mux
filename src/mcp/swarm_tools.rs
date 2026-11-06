//! The coordinator-first swarm's MCP tools
//! (`docs/superpowers/specs/2026-11-05-swarm-coordinator-first-design.md` §6):
//! the coordinator starts a swarm from its own session, launches the
//! workers once the operator says yes in its chat, waits on the swarm's
//! log, reads the board and relays the operator's other decisions. Every
//! tool writes rows through `bus::swarm::record`; the running TUI's engine
//! acts on them at its next pass, so a tool call never depends on the
//! control socket except where a session must be started or stopped
//! (`agent_mux_swarm_action` tries the socket first and falls back to the
//! records). `agent_mux_swarm_plan` stays in `server.rs`.

use super::server::{
    ToolSpec, bus_caller_id, bus_ok_envelope, bus_workspace, schema_of, tool_error,
};
use crate::bus::swarm::record::{self, DecidedBy, Decider, LaunchedBy, SwarmRun, SwarmVerb};
use crate::bus::swarm::view;
use crate::tracing::analysis::{ServiceError, TraceService};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

/// The swarm tools, in the order `tools/list` returns them after the bus
/// tools.
pub fn catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "agent_mux_swarm_start",
            description: "Become the coordinator of a new agent-mux swarm in this workspace: your session is recorded as its coordinator and the running agent-mux TUI supervises it. Give the goal when you know it; limits (max parallel workers, isolation, check command, workers' harness pool) are optional. Then plan with agent_mux_swarm_plan.",
            input_schema: schema_of::<SwarmStartArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_swarm_launch",
            description: "Start the swarm's workers once the operator confirmed in your chat. Pass the operator's own words in operator_said: a plan in review is approved only on their word. Without tasks, every ready task starts (within the cap); with tasks (plan keys), those start now. Call it again as dependencies finish, or let agent-mux start a ready task itself after the grace period.",
            input_schema: schema_of::<SwarmLaunchArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_swarm_wait",
            description: "Block until something happens in the swarm (a task finishes, fails, is lost, asks, is ready to start, a decision, the final check) or wait_ms pass (default 5 minutes, at most 10). A timeout stops nothing: report where things stand to the operator and ask whether to keep waiting. Pass after (the last_event_id you got) to continue from there.",
            input_schema: schema_of::<SwarmWaitArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_swarm_board",
            description: "The swarm's board as the Swarm screen shows it: each task's state (ready, working, asking, stalled, done, review, failed, lost…), worker, result or error, branch and open question; the swarm's status, workers against the cap, ready tasks nobody launched yet, open questions and the last event id.",
            input_schema: schema_of::<SwarmBoardArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_swarm",
            description: "One swarm as one trace: the swarm, its coordinator and workers (each launch's turns, tools, tokens, cost and sub-agents), every task's state, the decisions, the log (swarm_events) and the messages. Defaults to the calling session's swarm; an id or prefix names another.",
            input_schema: schema_of::<GetSwarmArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_swarm_action",
            description: "Relay an operator decision on the swarm: pause, resume, retry <task>, cancel <task>, merge <task>, set-aside <task>, accept <task>, drop <task>, close. Merge, set-aside, cancel and close need the operator's words in operator_said; the decision is recorded with them.",
            input_schema: schema_of::<SwarmActionArgs>(),
            read_only: false,
        },
    ]
}

/// Dispatches one swarm tool call; `None` for a name that is not one.
pub fn call(service: &TraceService, name: &str, arguments: Value) -> Option<Value> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    Some(match name {
        "agent_mux_swarm_start" => start(service, arguments),
        "agent_mux_swarm_launch" => launch(service, arguments),
        "agent_mux_swarm_wait" => wait(service, arguments),
        "agent_mux_swarm_board" => board(service, arguments),
        "agent_mux_get_swarm" => get_swarm(service, arguments),
        "agent_mux_swarm_action" => action(service, arguments),
        _ => return None,
    })
}

fn invalid(what: &str, e: impl std::fmt::Display) -> Value {
    tool_error(&ServiceError::InvalidArgument(format!(
        "invalid arguments for {what}: {e}"
    )))
}

/// The swarm a call is about: the one named, else the caller's (as its
/// coordinator, then as one of its workers), else the one its environment
/// names, else the workspace's only live swarm.
pub fn swarm_of_caller(
    conn: &Connection,
    caller: &str,
    workspace: &Path,
    explicit: Option<&str>,
) -> Result<SwarmRun, String> {
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return record::resolve_swarm(conn, &workspace.to_string_lossy(), Some(id));
    }
    if let Some(s) = record::swarm_of_coordinator(conn, caller).ok().flatten() {
        return Ok(s);
    }
    if let Some((s, _)) = record::swarm_of_worker(conn, caller).ok().flatten() {
        return Ok(s);
    }
    if let Some(s) = std::env::var("AGENT_MUX_SWARM_ID")
        .ok()
        .and_then(|id| record::get_swarm(conn, &id).ok().flatten())
        .filter(|s| s.ended_ns.is_none())
    {
        return Ok(s);
    }
    record::resolve_swarm(conn, &workspace.to_string_lossy(), None)
}

fn open(service: &TraceService) -> Result<(Connection, std::path::PathBuf), Value> {
    let db = service.config().db_path.clone();
    match crate::tracing::store::open_aux(&db) {
        Ok(c) => Ok((c, db)),
        Err(e) => Err(tool_error(&ServiceError::DbUnavailable(e))),
    }
}

/// Arguments of `agent_mux_swarm_start`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwarmStartArgs {
    /// What the swarm should achieve; may be empty while you still talk it
    /// over with the operator.
    #[serde(default)]
    pub goal: String,
    /// How many tasks run at once (default 3, at most 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<usize>,
    /// `shared` (file leases) or `worktree` (a branch per task); default shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    /// `chat` (default: you relay the operator's yes) or `off` (a plan
    /// without errors runs at once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<record::GateMode>,
    /// A command agent-mux runs in the workspace when every task is settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The harnesses workers may run on (`claude`, `codex`, `agy`,
    /// `opencode`), the first for tasks that name none; default: yours.
    #[serde(default)]
    pub workers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Your launch id; defaults to `$AGENT_MUX_SESSION_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn start(service: &TraceService, arguments: Value) -> Value {
    let args: SwarmStartArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("swarm_start", e),
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, db) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    if caller == "cli" {
        return tool_error(&ServiceError::InvalidArgument(
            "only a traced agent-mux session can coordinate a swarm (no $AGENT_MUX_SESSION_ID)"
                .into(),
        ));
    }
    if let Some((s, t)) = record::swarm_of_worker(&conn, &caller).ok().flatten() {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "you are a worker of swarm {} (task `{}`): a worker proposes tasks with agent_mux_task_split, it does not start a swarm",
            s.id, t.ext.plan_key
        )));
    }
    if let Some(s) = record::swarm_of_coordinator(&conn, &caller).ok().flatten() {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "you already coordinate swarm {} ({}); close it first",
            s.id,
            s.status.as_str()
        )));
    }
    let harness: String = conn
        .query_row(
            "SELECT provider FROM launches WHERE id = ?1",
            rusqlite::params![caller],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|p| p.parse::<crate::harness::Harness>().ok())
        .map(|h| h.as_str().to_string())
        .unwrap_or_else(|| "claude".into());
    let isolation = match args.isolation.as_deref().map(str::trim) {
        None | Some("") => crate::bus::TaskIsolation::Shared,
        Some(s) => match crate::bus::TaskIsolation::parse(s) {
            Some(i) => i,
            None => {
                return tool_error(&ServiceError::InvalidArgument(format!(
                    "isolation is shared or worktree, not {s}"
                )));
            }
        },
    };
    let mut pool = Vec::new();
    for h in &args.workers {
        match h.parse::<crate::harness::Harness>() {
            Ok(h) => pool.push(h.as_str().to_string()),
            Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
        }
    }
    let now = crate::tracing::store::now_ns();
    let swarm = match record::create_swarm(
        &conn,
        Some(&db),
        &record::NewSwarm {
            workspace: ws.to_string_lossy().into_owned(),
            goal: args.goal.clone(),
            planner_harness: harness.clone(),
            max_workers: args.max_workers.unwrap_or(3).clamp(1, 8),
            isolation,
            gate: args.gate.unwrap_or_default() != record::GateMode::Off,
            check_cmd: args.check.clone(),
            owner_run: None,
            worker_pool: pool,
            headless: false,
        },
        now,
    ) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    if let Err(e) = record::set_coordinator_launch(&conn, Some(&db), &swarm.id, &caller, now) {
        return tool_error(&ServiceError::DbUnavailable(e.to_string()));
    }
    let _ = record::event(
        &conn,
        &swarm.id,
        None,
        "coordinator.started",
        Some(&caller),
        Some(json!({ "harness": harness, "from_session": true })),
        now,
    );
    crate::tracing::store::poke_wake(&db);
    let env = crate::bus::swarm::plan::CheckEnv::for_workspace(&ws, isolation);
    bus_ok_envelope(
        &ws,
        json!({
            "swarm": swarm.id,
            "status": swarm.status.as_str(),
            "coordinator": caller,
            "gate": swarm.gate_mode.as_str(),
            "context": {
                "workspace_path": swarm.workspace,
                "git_repo": env.git_repo,
                "harnesses": record::worker_pool(&conn, &swarm),
                "installed": env.harnesses,
                "agents": env.agents,
                "max_workers": swarm.max_workers,
                "default_isolation": swarm.isolation.as_str(),
                "check": swarm.check_cmd,
            },
            "note": format!(
                "You coordinate swarm {}. Talk the goal over with the operator, then submit the plan with agent_mux_swarm_plan; the running agent-mux TUI supervises the swarm and tells you what happens.",
                swarm.id
            ),
        }),
    )
}

/// Arguments of `agent_mux_swarm_launch`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwarmLaunchArgs {
    /// The operator's words that confirmed the start, quoted ("go", "yes,
    /// start them"). Required to approve a plan in review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_said: Option<String>,
    /// Plan keys of the tasks to start now; empty: every ready task.
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn launch(service: &TraceService, arguments: Value) -> Value {
    let args: SwarmLaunchArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("swarm_launch", e),
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, db) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let swarm = match swarm_of_caller(&conn, &caller, &ws, args.swarm.as_deref()) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    let env = crate::bus::swarm::plan::CheckEnv::for_workspace(
        Path::new(&swarm.workspace),
        swarm.isolation,
    );
    let who = Decider::chat(args.operator_said.as_deref(), Some(&caller));
    let now = crate::tracing::store::now_ns();
    match record::launch(&conn, Some(&db), &swarm, &args.tasks, &who, &env, now) {
        Ok(done) => {
            let current = record::get_swarm(&conn, &swarm.id)
                .ok()
                .flatten()
                .unwrap_or(swarm);
            let note = done.note(&current);
            let board = view::board_view(&conn, &current, &view::snapshot_workers(), now).ok();
            let mut v = bus_ok_envelope(
                &ws,
                json!({
                    "swarm": current.id,
                    "approved": done.approved,
                    "requested": done.requested,
                    "note": note,
                    "board": board,
                }),
            );
            if let Some(content) = v.get_mut("content")
                && let Some(first) = content.as_array_mut().and_then(|a| a.first_mut())
            {
                first["text"] = Value::String(note);
            }
            v
        }
        Err(e) => tool_error(&ServiceError::InvalidArgument(e)),
    }
}

/// Arguments of `agent_mux_swarm_wait`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwarmWaitArgs {
    /// How long to wait, in milliseconds (default 300000, at most 600000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// The `last_event_id` of your previous wait or board; events after it
    /// are answered. Absent: only what happens from now on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn wait(service: &TraceService, arguments: Value) -> Value {
    let args: SwarmWaitArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("swarm_wait", e),
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, db) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let swarm = match swarm_of_caller(&conn, &caller, &ws, args.swarm.as_deref()) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    let after = match args.after {
        Some(a) => a,
        None => record::last_event_id(&conn, &swarm.id).unwrap_or(0),
    };
    drop(conn);
    let wait_ms = args
        .wait_ms
        .unwrap_or(view::DEFAULT_SWARM_WAIT_MS)
        .min(view::MAX_SWARM_WAIT_MS);
    let waited = match view::wait_events(&db, &swarm.id, after, wait_ms) {
        Ok(w) => w,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let (conn, _) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let now = crate::tracing::store::now_ns();
    let current = record::get_swarm(&conn, &swarm.id)
        .ok()
        .flatten()
        .unwrap_or(swarm);
    let board = view::board_view(&conn, &current, &view::snapshot_workers(), now).ok();
    let summary = if waited.timed_out {
        format!(
            "Nothing happened in {}s. Tell the operator where the swarm stands (the board is attached) and ask whether to keep waiting or stop a task.",
            wait_ms / 1000
        )
    } else if waited.ended {
        format!("Swarm {} has ended.", current.id)
    } else {
        format!(
            "{} event(s):\n{}",
            waited.events.len(),
            waited
                .events
                .iter()
                .map(|e| format!("- {}", view::event_line(e)))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let mut v = bus_ok_envelope(
        &ws,
        json!({
            "swarm": current.id,
            "status": current.status.as_str(),
            "events": waited.events,
            "last_event_id": waited.last_event_id,
            "timed_out": waited.timed_out,
            "ended": waited.ended,
            "board": board,
            "summary": summary,
        }),
    );
    if let Some(content) = v.get_mut("content")
        && let Some(first) = content.as_array_mut().and_then(|a| a.first_mut())
    {
        first["text"] = Value::String(summary);
    }
    v
}

/// Arguments of `agent_mux_swarm_board`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwarmBoardArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn board(service: &TraceService, arguments: Value) -> Value {
    let args: SwarmBoardArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("swarm_board", e),
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, _) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let swarm = match swarm_of_caller(&conn, &caller, &ws, args.swarm.as_deref()) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    let now = crate::tracing::store::now_ns();
    match view::board_view(&conn, &swarm, &view::snapshot_workers(), now) {
        Ok(b) => bus_ok_envelope(&ws, serde_json::to_value(&b).unwrap_or(Value::Null)),
        Err(e) => tool_error(&ServiceError::DbUnavailable(e.to_string())),
    }
}

/// Arguments of `agent_mux_get_swarm`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSwarmArgs {
    /// The swarm (`swarm-<id>`, or the id's prefix); absent means the
    /// caller's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// `agent_mux_get_swarm`: the swarm's report (`tracing::swarm_report`).
fn get_swarm(service: &TraceService, arguments: Value) -> Value {
    let args: GetSwarmArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("get_swarm", e),
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, _) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let swarm = match swarm_of_caller(&conn, &caller, &ws, args.swarm.as_deref()).or_else(|e| {
        match args.swarm.as_deref() {
            // an ended swarm named by id is a trace too
            Some(id) => record::resolve_swarm(&conn, &ws.to_string_lossy(), Some(id)),
            None => Err(e),
        }
    }) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::NotFound(e)),
    };
    let now = crate::tracing::store::now_ns();
    match crate::tracing::swarm_report::report(&conn, &swarm, &view::snapshot_workers(), now) {
        Ok(r) => bus_ok_envelope(&ws, serde_json::to_value(&r).unwrap_or(Value::Null)),
        Err(e) => tool_error(&ServiceError::DbUnavailable(e.to_string())),
    }
}

/// Arguments of `agent_mux_swarm_action`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwarmActionArgs {
    /// `pause`, `resume`, `retry`, `cancel`, `merge`, `set-aside`, `accept`,
    /// `drop`, `close` (also `launch`, `approve`: see agent_mux_swarm_launch).
    pub verb: String,
    /// The task (plan key, id or prefix) for the verbs that take one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The operator's words, quoted; required for merge, set-aside, cancel
    /// and close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_said: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn action(service: &TraceService, arguments: Value) -> Value {
    let args: SwarmActionArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => return invalid("swarm_action", e),
    };
    let Some(verb) = SwarmVerb::parse(args.verb.trim()) else {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "unknown verb `{}`: pause, resume, retry, cancel, merge, set-aside, accept, drop, close",
            args.verb
        )));
    };
    if verb == SwarmVerb::Replan || verb == SwarmVerb::Restart {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "`{}` is the operator's (the Swarm screen or `agent-mux swarm {}`); revise the plan with agent_mux_swarm_plan instead",
            verb.as_str(),
            verb.as_str()
        )));
    }
    let ws = bus_workspace(service, args.workspace.as_deref());
    let caller = bus_caller_id(args.session_id.as_deref());
    let (conn, db) = match open(service) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let swarm = match swarm_of_caller(&conn, &caller, &ws, args.swarm.as_deref()) {
        Ok(s) => s,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    let said = args
        .operator_said
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if verb.needs_operator_words() && said.is_none() {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "quote what the operator said in operator_said: `{}` happens only on their word",
            verb.as_str()
        )));
    }
    if verb.takes_task() && args.task.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return tool_error(&ServiceError::InvalidArgument(format!(
            "`{}` needs a task (its plan key or id)",
            verb.as_str()
        )));
    }
    drop(conn);
    // The TUI first: it stops sessions and tells the coordinator. Without
    // one, the records alone (the TUI acts on them when it runs).
    let req = crate::bus::control::ControlRequest::SwarmAction {
        workspace: swarm.workspace.clone(),
        verb,
        task: args.task.clone(),
        notes: args.notes.clone(),
        swarm: Some(swarm.id.clone()),
        by: Some(DecidedBy::Chat),
        said: said.map(str::to_string),
        from: Some(caller.clone()),
    };
    let detail =
        match crate::bus::control::send_request(None, &req, std::time::Duration::from_secs(10)) {
            Ok(resp) if resp.ok => resp.detail.unwrap_or_default(),
            Ok(resp) => {
                return tool_error(&ServiceError::InvalidArgument(
                    resp.error.unwrap_or_else(|| "refused".into()),
                ));
            }
            Err(e) if verb.needs_tui() => {
                return tool_error(&ServiceError::Busy(format!(
                    "`{}` needs the running agent-mux TUI: {e}",
                    verb.as_str()
                )));
            }
            Err(_) => {
                let (conn, _) = match open(service) {
                    Ok(c) => c,
                    Err(v) => return v,
                };
                let env = crate::bus::swarm::plan::CheckEnv::for_workspace(
                    Path::new(&swarm.workspace),
                    swarm.isolation,
                );
                match record::apply(
                    &conn,
                    Some(&db),
                    &swarm,
                    verb,
                    args.task.as_deref(),
                    &env,
                    &Decider::chat(said, Some(&caller)),
                    crate::tracing::store::now_ns(),
                ) {
                    Ok(d) => {
                        format!("{d} (no agent-mux TUI is running: it acts on this when one does)")
                    }
                    Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
                }
            }
        };
    let mut v = bus_ok_envelope(
        &ws,
        json!({ "swarm": swarm.id, "verb": verb.as_str(), "detail": detail }),
    );
    if let Some(content) = v.get_mut("content")
        && let Some(first) = content.as_array_mut().and_then(|a| a.first_mut())
    {
        first["text"] = Value::String(detail);
    }
    v
}

/// A coordinator's `agent_mux_task_split` or `agent_mux_delegate`: the
/// tasks join its board (not as proposals: the coordinator is the one who
/// relays) and, once the plan is approved, start at the engine's next pass.
/// `None` when the caller is not a live swarm's coordinator.
pub fn coordinator_adds_tasks(
    conn: &Connection,
    db: &Path,
    ws: &Path,
    caller: &str,
    specs: &[crate::bus::NewTaskSpec],
    now_ns: i64,
) -> Option<Value> {
    let swarm = record::swarm_of_coordinator(conn, caller).ok().flatten()?;
    let mut added = Vec::new();
    for spec in specs {
        if spec.title.trim().is_empty() {
            return Some(tool_error(&ServiceError::InvalidArgument(
                "task title cannot be empty".into(),
            )));
        }
        let key = record::key_for_title(conn, &swarm.id, &spec.title);
        let pt = crate::bus::swarm::plan::PlannedTask {
            key: key.clone(),
            title: spec.title.trim().to_string(),
            instructions: spec.instructions.trim().to_string(),
            acceptance: None,
            files: spec.files.clone(),
            depends_on: Vec::new(),
            harness: spec.target_harness.clone(),
            agent: spec.target_agent.clone(),
            isolation: Some(spec.isolation.as_str().to_string()),
        };
        let id = match record::add_task(conn, Some(db), &swarm, &pt, false, now_ns) {
            Ok(id) => id,
            Err(e) => return Some(tool_error(&ServiceError::InvalidArgument(e))),
        };
        if swarm.status.approved() {
            let _ = record::request_launch(conn, Some(db), &id, LaunchedBy::Coordinator, now_ns);
        }
        let _ = record::event(
            conn,
            &swarm.id,
            Some(&id),
            "task.added",
            Some(caller),
            Some(json!({ "key": key, "title": pt.title })),
            now_ns,
        );
        added.push(json!({ "key": key, "id": id }));
    }
    crate::tracing::store::poke_wake(db);
    Some(bus_ok_envelope(
        ws,
        json!({
            "swarm": swarm.id,
            "added": added,
            "note": if swarm.status.approved() {
                format!("{} task(s) added to the board of {}; their workers start at agent-mux's next pass.", added.len(), swarm.id)
            } else {
                format!("{} task(s) added to the board of {}; they start with the plan once the operator confirms it.", added.len(), swarm.id)
            },
        }),
    ))
}
