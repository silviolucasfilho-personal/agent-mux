//! A minimal MCP server: line-delimited JSON-RPC 2.0 on stdin/stdout with
//! `initialize`, `notifications/initialized`, `ping`, `tools/list` and
//! `tools/call`. Every tool is one `TraceService` request; the service
//! owns scope, deadlines, admission and response bounds. Stdout carries
//! protocol messages only; diagnostics go to stderr.

use crate::tracing::analysis::service::{
    AnalyzeSkillsArgs, BriefingArgs, CompareRunsArgs, GetSessionArgs, HealthArgs, ListSessionsArgs,
    SearchArgs, TimelineArgs,
};
use crate::tracing::analysis::{Request, Scope, ServiceConfig, ServiceError, TraceService};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// The protocol version answered when the client's is unknown; the newest
/// this server implements.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
/// Versions whose `initialize` / `tools` messages this server speaks.
pub const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// One tool as `tools/list` publishes it.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
}

fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({"type": "object"}))
}

/// The MCP tools (ten read-only trace tools plus seven inter-agent bus
/// coordination tools), in the order `tools/list` returns them.
pub fn tool_catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "agent_mux_get_briefing",
            description: "Briefing of active and recent sessions in the workspace scope: goal, current activity, files, commands, last output, tokens and cost per session. Defaults to the last 24 hours.",
            input_schema: schema_of::<BriefingArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_list_sessions",
            description: "Sessions in the workspace scope with turns, tools, tokens, cost and runtime state; filter by provider, runtime state and time window.",
            input_schema: schema_of::<ListSessionsArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_session",
            description: "One session's card by session key (optionally bound to a launch id).",
            input_schema: schema_of::<GetSessionArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_timeline",
            description: "A session's turns in order with their observations (tools, generations, errors) and durations.",
            input_schema: schema_of::<TimelineArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_search_traces",
            description: "Search prompts and outputs of recorded turns for a phrase within the scope and window.",
            input_schema: schema_of::<SearchArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_analyze_skills",
            description: "Per-skill attribution metrics: attributed calls, errors, latency percentiles, tokens, cost and turns loaded.",
            input_schema: schema_of::<AnalyzeSkillsArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_compare_runs",
            description: "Two launches side by side: turns, tools, tokens, cost and duration with deltas.",
            input_schema: schema_of::<CompareRunsArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_health",
            description: "Reader version, database availability, collector freshness and the tools this server offers. Never needs the database.",
            input_schema: schema_of::<HealthArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_loop_context",
            description: "The Loop Engineering context of a loop run, recomputed now: effective level and why, today's budget against the caps, circuit breaker state, gate globs, readiness score, recent runs and the human inbox. Defaults to the calling run (AGENT_MUX_LOOP_RUN_ID) or the workspace's loop.",
            input_schema: schema_of::<LoopContextArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_get_workflow_run",
            description: "A workflow run: its document, status, args, sessions (step, phase, harness, result kind, tokens, cost) and result. Defaults to the calling run (AGENT_MUX_WORKFLOW_RUN_ID); an id prefix is accepted.",
            input_schema: schema_of::<WorkflowRunArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_list_peers",
            description: "List active peer CLI sessions in the workspace (Claude Code, Codex, Antigravity) with their launch_id, runtime state, current goal, active tools, claimed tasks, file leases and unread messages.",
            input_schema: schema_of::<BusListPeersArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_send_message",
            description: "Send a direct or broadcast ('*') message to another live agent session on the inter-agent bus. Set urgent=true to inject into the peer's next PreToolUse hook, or wake=true to wake an idle peer's PTY.",
            input_schema: schema_of::<BusSendMessageArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_recv_messages",
            description: "Read messages addressed to this session (or broadcast '*') from the inter-agent bus, with optional long-polling (wait_ms).",
            input_schema: schema_of::<BusRecvMessagesArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_task_board",
            description: "Inspect the shared workspace task board and active cooperative file leases.",
            input_schema: schema_of::<BusTaskBoardArgs>(),
            read_only: true,
        },
        ToolSpec {
            name: "agent_mux_task_split",
            description: "Split a goal into coordinated subtasks (with optional dependencies, file scopes and target harnesses) on the shared workspace task board.",
            input_schema: schema_of::<BusTaskSplitArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_task_update",
            description: "Claim, complete, fail, cancel or wait on tasks on the shared task board, or acquire/release cooperative file leases.",
            input_schema: schema_of::<BusTaskUpdateArgs>(),
            read_only: false,
        },
        ToolSpec {
            name: "agent_mux_delegate",
            description: "Dynamically spawn a new worker CLI session (Claude Code, Codex or Antigravity) in the running agent-mux TUI to execute a delegated task (in the shared workspace or an isolated git worktree).",
            input_schema: schema_of::<BusDelegateArgs>(),
            read_only: false,
        },
    ]
}

/// Arguments of `agent_mux_get_workflow_run`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunArgs {
    /// A run id (`workflow_runs.id`) or unique prefix; absent means the
    /// calling run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// The workflow run tool: the run row, its sessions and its result.
fn workflow_run_result(service: &TraceService, arguments: Value) -> Value {
    let args: WorkflowRunArgs = match serde_json::from_value(if arguments.is_null() {
        json!({})
    } else {
        arguments
    }) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for get_workflow_run: {e}"
            )));
        }
    };
    let Some(run_id) = args
        .run_id
        .or_else(|| std::env::var("AGENT_MUX_WORKFLOW_RUN_ID").ok())
        .filter(|s| !s.trim().is_empty())
    else {
        return tool_error(&ServiceError::InvalidArgument(
            "run_id is required outside a workflow session".into(),
        ));
    };
    let config = service.config();
    let conn = match crate::tracing::store::open_ro(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let run = match crate::workflows::store::resolve_run(&conn, &run_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return tool_error(&ServiceError::NotFound(format!("no workflow run {run_id}")));
        }
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e.to_string())),
    };
    let steps = crate::workflows::store::steps_of(&conn, &run.id).unwrap_or_default();
    let value = json!({
        "schema_version": 1,
        "scope": { "workspace": run.workspace, "all_workspaces": false },
        "window": Value::Null,
        "data": {
            "id": run.id, "workflow": run.workflow, "source": run.source, "status": run.status,
            "harness": run.harness, "profile": run.profile, "args": run.args,
            "budget_tokens": run.budget_tokens, "started_ns": run.started_ns, "ended_ns": run.ended_ns,
            "sessions": steps.iter().map(|s| json!({
                "session": s.session, "step": s.step_id, "phase": s.phase, "harness": s.harness,
                "kind": s.kind, "tokens": s.tokens, "cost_usd": s.cost_usd, "launch_id": s.launch_id,
                "worktree": s.worktree, "changed_files": s.changed_files, "result": s.result,
            })).collect::<Vec<_>>(),
            "tokens": run.tokens, "cost_usd": run.cost_usd, "result": run.result, "error": run.error,
            "document": run.document,
        },
        "coverage": { "status": "complete" },
        "warnings": [],
        "next_cursor": Value::Null,
        "truncated": false,
    });
    let text = serde_json::to_string(&value).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

/// Arguments of `agent_mux_get_loop_context`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopContextArgs {
    /// A run id (`loop_runs.id`); absent means the calling run or the
    /// workspace's most recent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// The loop context tool: outside `TraceService` because it reads the
/// registry and the workspace files besides the store.
fn loop_context_result(service: &TraceService, arguments: Value) -> Value {
    let args: LoopContextArgs = match serde_json::from_value(if arguments.is_null() {
        json!({})
    } else {
        arguments
    }) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for get_loop_context: {e}"
            )));
        }
    };
    let run_id = args
        .run_id
        .or_else(|| std::env::var("AGENT_MUX_LOOP_RUN_ID").ok())
        .filter(|s| !s.trim().is_empty());
    let config = service.config();
    let workspace = config
        .scope
        .workspace_path()
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var("AGENT_MUX_LOOP_WORKSPACE")
                .ok()
                .map(PathBuf::from)
        });
    if !config.db_path.is_file() {
        return tool_error(&ServiceError::DbUnavailable(format!(
            "no trace store at {}",
            config.db_path.display()
        )));
    }
    match crate::loops::context::live(&config.db_path, workspace.as_deref(), run_id.as_deref()) {
        Ok(doc) => {
            let value = json!({
                "schema_version": 1,
                "as_of": doc.as_of,
                "scope": {
                    "workspace": workspace.as_ref().map(|w| w.to_string_lossy().into_owned()),
                    "all_workspaces": workspace.is_none(),
                },
                "window": Value::Null,
                "data": doc,
                "coverage": { "status": "complete" },
                "warnings": [],
                "next_cursor": Value::Null,
                "truncated": false,
            });
            let text = serde_json::to_string(&value).unwrap_or_default();
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": value,
                "isError": false,
            })
        }
        Err(e) => tool_error(&ServiceError::NotFound(e)),
    }
}

pub fn tool_names() -> Vec<&'static str> {
    tool_catalog().into_iter().map(|t| t.name).collect()
}

/// Arguments of `agent_mux_list_peers`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusListPeersArgs {
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// When true, lists active peer sessions across all workspaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_workspaces: Option<bool>,
    /// When true, includes exited sessions in the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_exited: Option<bool>,
}

/// Arguments of `agent_mux_send_message`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusSendMessageArgs {
    /// Target session `launch_id` (full or unique >=6-char prefix), or `"*"` for workspace broadcast.
    pub to: String,
    /// Message text to deliver.
    pub body: String,
    /// Optional topic tag (e.g. `"api-contract"`, `"blocker"`, `"handoff"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// When true, injects this message into the recipient's next `PreToolUse` hook before its next tool call executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<bool>,
    /// When true, wakes an idle recipient session in `agent-mux` by writing a prompt to its PTY.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<bool>,
    /// Optional sender `launch_id`; defaults to `$AGENT_MUX_SESSION_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Arguments of `agent_mux_recv_messages`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusRecvMessagesArgs {
    /// Optional topic filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Optional long-poll timeout in milliseconds (bounded to 30,000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Only return unread messages (defaults to true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_only: Option<bool>,
    /// Mark returned messages as read (defaults to true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_read: Option<bool>,
    /// Optional recipient `launch_id`; defaults to `$AGENT_MUX_SESSION_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Maximum messages to return (defaults to 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Arguments of `agent_mux_task_board`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusTaskBoardArgs {
    /// Optional status filter (`open`, `claimed`, `running`, `done`, `failed`, `cancelled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Maximum tasks to return (defaults to 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Arguments of `agent_mux_task_split`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusTaskSplitArgs {
    /// Subtasks to create on the shared workspace task board.
    pub tasks: Vec<crate::bus::NewTaskSpec>,
    /// Optional parent task id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Optional creator `launch_id`; defaults to `$AGENT_MUX_SESSION_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Arguments of `agent_mux_task_update`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusTaskUpdateArgs {
    /// Action to perform: `"claim"`, `"running"`, `"complete"`, `"fail"`, `"cancel"`, `"wait"`, `"lease"`, or `"unlease"`.
    pub action: String,
    /// Task id (or unique >=6-char prefix) for `"claim"`, `"running"`, `"complete"`, `"fail"`, `"cancel"`, or `"lease"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Task ids to wait on when `action == "wait"` (empty waits on all tasks in the workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ids: Option<Vec<String>>,
    /// Summary result or error message for `"complete"` / `"fail"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// File paths (relative to workspace) for `"lease"` / `"unlease"`, or additional files to lease on `"claim"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Lease time-to-live in seconds (defaults to 1800).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
    /// Timeout in milliseconds when `action == "wait"` (bounded to 60,000 ms, defaults to 30,000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Optional calling session `launch_id`; defaults to `$AGENT_MUX_SESSION_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Arguments of `agent_mux_delegate`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusDelegateArgs {
    /// Short title of the delegated task.
    pub title: String,
    /// Detailed instructions for the worker session.
    #[serde(default)]
    pub instructions: String,
    /// Target harness (`"claude"`, `"codex"`, or `"agy"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Optional persona agent name (e.g. `"reviewer"`, `"skeptic"`, `"planner"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Workspace isolation mode: `"shared"` (default, cooperative file leases) or `"worktree"` (isolated git worktree).
    #[serde(default)]
    pub isolation: crate::bus::TaskIsolation,
    /// Files (relative to workspace) this worker will edit.
    #[serde(default)]
    pub files: Vec<String>,
    /// Task IDs this delegated task depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Optional milliseconds to wait for the worker to complete the task (0 = return immediately after spawning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Optional workspace directory override (defaults to the session's workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

fn bus_workspace(service: &TraceService, explicit: Option<&str>) -> PathBuf {
    let raw = explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            service
                .config()
                .scope
                .workspace_path()
                .map(Path::to_path_buf)
        })
        .or_else(|| {
            std::env::var("AGENT_MUX_WORKSPACE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    PathBuf::from(crate::bus::store::normalize_workspace(&raw))
}

fn bus_caller_id(explicit: Option<&str>) -> String {
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    std::env::var(crate::bus::ENV_SESSION_ID)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "cli".to_string())
}

fn bus_ok_envelope(workspace: &Path, data: Value) -> Value {
    let now_rfc3339 = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let value = json!({
        "schema_version": 1,
        "as_of": now_rfc3339,
        "scope": {
            "workspace": workspace.to_string_lossy(),
            "all_workspaces": false,
        },
        "window": Value::Null,
        "data": data,
        "coverage": { "status": "complete" },
        "warnings": [],
        "next_cursor": Value::Null,
        "truncated": false,
    });
    let text = serde_json::to_string(&value).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

fn resolve_task_prefix(
    conn: &rusqlite::Connection,
    workspace: &str,
    prefix: &str,
) -> Result<String, ServiceError> {
    let tasks = crate::bus::store::list_tasks(conn, workspace, None, None, None)
        .map_err(|e| ServiceError::DbUnavailable(e.to_string()))?;
    if tasks.iter().any(|t| t.id == prefix) {
        return Ok(prefix.to_string());
    }
    let matches: Vec<&crate::bus::BusTask> = tasks
        .iter()
        .filter(|t| t.id.starts_with(prefix) || t.title.eq_ignore_ascii_case(prefix))
        .collect();
    match matches.len() {
        1 => Ok(matches[0].id.clone()),
        0 => Ok(prefix.to_string()),
        _ => Err(ServiceError::InvalidArgument(format!(
            "ambiguous task prefix '{prefix}'"
        ))),
    }
}

fn bus_list_peers_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusListPeersArgs = match serde_json::from_value(if arguments.is_null() {
        json!({})
    } else {
        arguments
    }) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for list_peers: {e}"
            )));
        }
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let config = service.config();
    let conn = match crate::tracing::store::open_aux(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let now_ns = crate::tracing::store::now_ns();
    let self_id = std::env::var(crate::bus::ENV_SESSION_ID).ok();
    let include_exited = args.include_exited.or(args.all_workspaces).unwrap_or(false);
    let peers = match crate::bus::store::list_peers(
        &conn,
        &ws_str,
        self_id.as_deref(),
        include_exited,
        now_ns,
    ) {
        Ok(p) => p,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e.to_string())),
    };
    let leases = crate::bus::store::list_leases(&conn, &ws_str, now_ns).unwrap_or_default();
    bus_ok_envelope(
        &ws,
        json!({
            "self_launch_id": self_id,
            "peers": peers,
            "leases": leases,
        }),
    )
}

fn bus_send_message_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusSendMessageArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for send_message: {e}"
            )));
        }
    };
    if args.to.trim().is_empty() || args.body.trim().is_empty() {
        return tool_error(&ServiceError::InvalidArgument(
            "both 'to' and 'body' must be non-empty".into(),
        ));
    }
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let sender = bus_caller_id(args.from.as_deref());
    let config = service.config();
    let conn = match crate::tracing::store::open_aux(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let now_ns = crate::tracing::store::now_ns();
    let target = if args.to.trim() == "*" {
        "*".to_string()
    } else {
        let q = args.to.trim();
        let peers =
            crate::bus::store::list_peers(&conn, &ws_str, None, true, now_ns).unwrap_or_default();
        if peers.iter().any(|p| p.launch_id == q) {
            q.to_string()
        } else {
            let m: Vec<_> = peers
                .iter()
                .filter(|p| p.launch_id.starts_with(q))
                .collect();
            if m.len() == 1 {
                m[0].launch_id.clone()
            } else {
                q.to_string()
            }
        }
    };
    let to_opt = (target != "*").then_some(target.as_str());
    let topic_str = if args.urgent.unwrap_or(false) {
        "urgent"
    } else {
        args.topic.as_deref().unwrap_or("message")
    };
    let msg = match crate::bus::store::send_message(
        &conn,
        Some(&config.db_path),
        &ws_str,
        &sender,
        to_opt,
        topic_str,
        &args.body,
        None,
        now_ns,
    ) {
        Ok(m) => m,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e.to_string())),
    };
    crate::tracing::store::poke_wake(&config.db_path);

    let wake_result = if args.wake.unwrap_or(false) {
        let prompt = format!(
            "[agent-mux bus] New message #{} from {}: {}. Call agent_mux_recv_messages or `agent-mux bus recv` to read and respond.",
            msg.id,
            crate::bus::short_id(&sender),
            topic_str
        );
        let req = if target == "*" {
            crate::bus::control::ControlRequest::WakeWorkspace {
                workspace: ws_str.clone(),
                prompt,
                exclude: Some(sender.clone()),
            }
        } else {
            crate::bus::control::ControlRequest::WakeSession {
                launch_id: target.clone(),
                prompt,
            }
        };
        match crate::bus::control::send_request(None, &req, std::time::Duration::from_secs(2)) {
            Ok(resp) => serde_json::to_value(resp).unwrap_or(Value::Null),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    } else {
        Value::Null
    };

    bus_ok_envelope(
        &ws,
        json!({
            "message": msg,
            "wake": wake_result,
        }),
    )
}

fn bus_recv_messages_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusRecvMessagesArgs = match serde_json::from_value(if arguments.is_null() {
        json!({})
    } else {
        arguments
    }) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for recv_messages: {e}"
            )));
        }
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let recipient = bus_caller_id(args.recipient.as_deref());
    let unread_only = args.unread_only.unwrap_or(true);
    let mark_read = args.mark_read.unwrap_or(true);
    let limit = args.limit.unwrap_or(50).clamp(1, 200);
    let wait_ms = args.wait_ms.unwrap_or(0).min(30_000);
    let config = service.config();

    let mut messages = if wait_ms > 0 {
        match crate::bus::store::recv_messages_wait(
            &config.db_path,
            &ws_str,
            &recipient,
            unread_only,
            mark_read,
            limit,
            wait_ms,
        ) {
            Ok(m) => m,
            Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
        }
    } else {
        let conn = match crate::tracing::store::open_aux(&config.db_path) {
            Ok(c) => c,
            Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
        };
        match crate::bus::store::recv_messages(
            &conn,
            &ws_str,
            &recipient,
            unread_only,
            mark_read,
            limit,
            crate::tracing::store::now_ns(),
        ) {
            Ok(m) => m,
            Err(e) => return tool_error(&ServiceError::DbUnavailable(e.to_string())),
        }
    };
    if let Some(topic) = args.topic.as_deref() {
        messages.retain(|m| m.topic == topic);
    }

    bus_ok_envelope(
        &ws,
        json!({
            "recipient": recipient,
            "count": messages.len(),
            "messages": messages,
        }),
    )
}

fn bus_task_board_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusTaskBoardArgs = match serde_json::from_value(if arguments.is_null() {
        json!({})
    } else {
        arguments
    }) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for task_board: {e}"
            )));
        }
    };
    let status = match args.status.as_deref() {
        Some(s) => match crate::bus::TaskStatus::parse(s) {
            Some(st) => Some(st),
            None => {
                return tool_error(&ServiceError::InvalidArgument(format!(
                    "unknown task status '{s}'"
                )));
            }
        },
        None => None,
    };
    let limit = args.limit.unwrap_or(100).clamp(1, 500);
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let config = service.config();
    let conn = match crate::tracing::store::open_aux(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let now_ns = crate::tracing::store::now_ns();
    let mut tasks = match crate::bus::store::list_tasks(&conn, &ws_str, status, None, None) {
        Ok(t) => t,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e.to_string())),
    };
    tasks.truncate(limit);
    let leases = crate::bus::store::list_leases(&conn, &ws_str, now_ns).unwrap_or_default();
    bus_ok_envelope(
        &ws,
        json!({
            "tasks": tasks,
            "leases": leases,
        }),
    )
}

fn bus_task_split_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusTaskSplitArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for task_split: {e}"
            )));
        }
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let creator = bus_caller_id(args.created_by.as_deref());
    let config = service.config();
    let conn = match crate::tracing::store::open_aux(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let now_ns = crate::tracing::store::now_ns();
    let mut specs = args.tasks;
    if let Some(ref pid) = args.parent_id {
        for s in &mut specs {
            if s.parent_task_id.is_none() {
                s.parent_task_id = Some(pid.clone());
            }
        }
    }
    let created = match crate::bus::store::split_tasks(
        &conn,
        Some(&config.db_path),
        &ws_str,
        &creator,
        &specs,
        now_ns,
    ) {
        Ok(t) => t,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    crate::tracing::store::poke_wake(&config.db_path);
    bus_ok_envelope(
        &ws,
        json!({
            "created": created.len(),
            "tasks": created,
        }),
    )
}

fn bus_task_update_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusTaskUpdateArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for task_update: {e}"
            )));
        }
    };
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let caller = bus_caller_id(args.session_id.as_deref());
    let config = service.config();
    let now_ns = crate::tracing::store::now_ns();

    match args.action.trim() {
        "wait" => {
            let wait_ms = args.wait_ms.unwrap_or(30_000).min(60_000);
            let conn = match crate::tracing::store::open_aux(&config.db_path) {
                Ok(c) => c,
                Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
            };
            let mut ids = Vec::new();
            for raw in args.task_ids.unwrap_or_default() {
                match resolve_task_prefix(&conn, &ws_str, &raw) {
                    Ok(id) => ids.push(id),
                    Err(e) => return tool_error(&e),
                }
            }
            if ids.is_empty()
                && let Ok(all) = crate::bus::store::list_tasks(&conn, &ws_str, None, None, None)
            {
                ids = all.into_iter().map(|t| t.id).collect();
            }
            drop(conn);
            match crate::bus::store::wait_tasks(&config.db_path, &ids, wait_ms) {
                Ok(tasks) => {
                    let all_done = tasks.iter().all(|t| t.status.is_terminal());
                    bus_ok_envelope(
                        &ws,
                        json!({
                            "all_terminal": all_done,
                            "tasks": tasks,
                        }),
                    )
                }
                Err(e) => tool_error(&ServiceError::DbUnavailable(e)),
            }
        }
        "lease" => {
            let files = args.files.unwrap_or_default();
            if files.is_empty() {
                return tool_error(&ServiceError::InvalidArgument(
                    "'files' is required when action is 'lease'".into(),
                ));
            }
            let conn = match crate::tracing::store::open_aux(&config.db_path) {
                Ok(c) => c,
                Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
            };
            let ttl_ns = args
                .ttl_secs
                .map(|s| s.saturating_mul(1_000_000_000))
                .unwrap_or(crate::bus::DEFAULT_LEASE_TTL_NS);
            match crate::bus::store::acquire_leases(
                &conn,
                &ws_str,
                &caller,
                args.task_id.as_deref(),
                &files,
                ttl_ns,
                now_ns,
            ) {
                Ok(leases) => bus_ok_envelope(&ws, json!({ "leases": leases })),
                Err(e) => tool_error(&ServiceError::Busy(e)),
            }
        }
        "unlease" => {
            let conn = match crate::tracing::store::open_aux(&config.db_path) {
                Ok(c) => c,
                Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
            };
            let files = args.files.unwrap_or_default();
            match crate::bus::store::release_leases(
                &conn,
                &ws_str,
                &caller,
                args.task_id.as_deref(),
                &files,
            ) {
                Ok(n) => bus_ok_envelope(&ws, json!({ "released": n })),
                Err(e) => tool_error(&ServiceError::DbUnavailable(e.to_string())),
            }
        }
        "claim" => {
            let Some(raw_id) = args.task_id.as_deref() else {
                return tool_error(&ServiceError::InvalidArgument(
                    "'task_id' is required for action 'claim'".into(),
                ));
            };
            let conn = match crate::tracing::store::open_aux(&config.db_path) {
                Ok(c) => c,
                Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
            };
            let task_id = match resolve_task_prefix(&conn, &ws_str, raw_id) {
                Ok(id) => id,
                Err(e) => return tool_error(&e),
            };
            let task = match crate::bus::store::claim_task(
                &conn,
                Some(&config.db_path),
                &task_id,
                &caller,
                now_ns,
            ) {
                Ok(t) => t,
                Err(e) => return tool_error(&ServiceError::Busy(e)),
            };
            if let Some(extra) = args.files
                && !extra.is_empty()
            {
                let ttl_ns = args
                    .ttl_secs
                    .map(|s| s.saturating_mul(1_000_000_000))
                    .unwrap_or(crate::bus::DEFAULT_LEASE_TTL_NS);
                if let Err(e) = crate::bus::store::acquire_leases(
                    &conn,
                    &ws_str,
                    &caller,
                    Some(&task.id),
                    &extra,
                    ttl_ns,
                    now_ns,
                ) {
                    return tool_error(&ServiceError::Busy(e));
                }
            }
            let leases = crate::bus::store::list_leases(&conn, &ws_str, now_ns)
                .unwrap_or_default()
                .into_iter()
                .filter(|l| l.owner_launch == caller)
                .collect::<Vec<_>>();
            crate::tracing::store::poke_wake(&config.db_path);
            bus_ok_envelope(&ws, json!({ "task": task, "leases": leases }))
        }
        other @ ("running" | "complete" | "completed" | "done" | "fail" | "failed" | "cancel"
        | "cancelled") => {
            let Some(raw_id) = args.task_id.as_deref() else {
                return tool_error(&ServiceError::InvalidArgument(format!(
                    "'task_id' is required for action '{other}'"
                )));
            };
            let status = match other {
                "running" => crate::bus::TaskStatus::Running,
                "complete" | "completed" | "done" => crate::bus::TaskStatus::Done,
                "fail" | "failed" => crate::bus::TaskStatus::Failed,
                _ => crate::bus::TaskStatus::Cancelled,
            };
            let conn = match crate::tracing::store::open_aux(&config.db_path) {
                Ok(c) => c,
                Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
            };
            let task_id = match resolve_task_prefix(&conn, &ws_str, raw_id) {
                Ok(id) => id,
                Err(e) => return tool_error(&e),
            };
            let (result_val, error_str) = if status == crate::bus::TaskStatus::Failed {
                (None, args.result)
            } else {
                (args.result.map(Value::String), None)
            };
            let task = match crate::bus::store::update_task(
                &conn,
                Some(&config.db_path),
                &task_id,
                &caller,
                status,
                result_val,
                error_str,
                None,
                None,
                now_ns,
            ) {
                Ok(t) => t,
                Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
            };
            crate::tracing::store::poke_wake(&config.db_path);
            bus_ok_envelope(&ws, json!({ "task": task }))
        }
        other => tool_error(&ServiceError::InvalidArgument(format!(
            "unknown task_update action '{other}'"
        ))),
    }
}

fn bus_delegate_result(service: &TraceService, arguments: Value) -> Value {
    let args: BusDelegateArgs = match serde_json::from_value(arguments) {
        Ok(a) => a,
        Err(e) => {
            return tool_error(&ServiceError::InvalidArgument(format!(
                "invalid arguments for delegate: {e}"
            )));
        }
    };
    if args.title.trim().is_empty() {
        return tool_error(&ServiceError::InvalidArgument(
            "'title' must not be empty".into(),
        ));
    }
    let ws = bus_workspace(service, args.workspace.as_deref());
    let ws_str = ws.to_string_lossy().into_owned();
    let caller = bus_caller_id(None);
    let config = service.config();
    let conn = match crate::tracing::store::open_aux(&config.db_path) {
        Ok(c) => c,
        Err(e) => return tool_error(&ServiceError::DbUnavailable(e)),
    };
    let now_ns = crate::tracing::store::now_ns();
    let spec = crate::bus::NewTaskSpec {
        title: args.title.clone(),
        instructions: args.instructions.clone(),
        files: args.files.clone(),
        parent_task_id: args.depends_on.first().cloned(),
        assigned_to: None,
        target_harness: args.harness.clone(),
        target_agent: args.agent.clone(),
        isolation: args.isolation,
    };
    let created = match crate::bus::store::split_tasks(
        &conn,
        Some(&config.db_path),
        &ws_str,
        &caller,
        &[spec],
        now_ns,
    ) {
        Ok(t) => t,
        Err(e) => return tool_error(&ServiceError::InvalidArgument(e)),
    };
    drop(conn);
    let task = created.into_iter().next().unwrap();
    crate::tracing::store::poke_wake(&config.db_path);

    let req = crate::bus::control::ControlRequest::SpawnWorker {
        parent_launch: Some(caller),
        workspace: ws_str.clone(),
        task_id: task.id.clone(),
        title: task.title.clone(),
        instructions: task.instructions.clone(),
        harness: args.harness,
        agent: args.agent,
        isolation: args.isolation,
    };
    let spawn = match crate::bus::control::send_request(
        None,
        &req,
        std::time::Duration::from_secs(5),
    ) {
        Ok(resp) => serde_json::to_value(&resp).unwrap_or(Value::Null),
        Err(e) => json!({
            "ok": false,
            "error": e,
            "note": "task was recorded on the shared task board as open; another peer can claim it",
        }),
    };

    let wait_ms = args.wait_ms.unwrap_or(0).min(60_000);
    let final_task = if wait_ms > 0 {
        crate::bus::store::wait_tasks(&config.db_path, std::slice::from_ref(&task.id), wait_ms)
            .ok()
            .and_then(|mut v| v.pop())
            .unwrap_or(task)
    } else {
        crate::tracing::store::open_ro(&config.db_path)
            .ok()
            .and_then(|c| crate::bus::store::get_task(&c, &task.id).ok().flatten())
            .unwrap_or(task)
    };

    bus_ok_envelope(
        &ws,
        json!({
            "task": final_task,
            "spawn": spawn,
        }),
    )
}

fn tools_list_value() -> Value {
    let tools: Vec<Value> = tool_catalog()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
                "annotations": {
                    "title": t.name,
                    "readOnlyHint": t.read_only,
                    "destructiveHint": false,
                    "idempotentHint": t.read_only,
                    "openWorldHint": false,
                },
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn tool_error(err: &ServiceError) -> Value {
    json!({
        "content": [{ "type": "text", "text": err.to_string() }],
        "structuredContent": {
            "error": { "code": err.code(), "message": err.to_string(), "retryable": err.is_retryable() }
        },
        "isError": true,
    })
}

/// Handles one incoming message. `None` means nothing is sent back (a
/// notification, or a response we were not asked for).
pub fn handle_message(service: &TraceService, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    if message.is_array() {
        return Some(rpc_error(
            Value::Null,
            -32600,
            "batch requests are not supported",
        ));
    }
    let Some(obj) = message.as_object() else {
        return Some(rpc_error(Value::Null, -32600, "invalid request"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // a response to something we never sent, or a malformed message
        return id.map(|id| rpc_error(id, -32600, "invalid request: no method"));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    if std::env::var_os("AGENT_MUX_MCP_DEBUG").is_some() {
        eprintln!("agent-mux mcp: {method}");
    }
    let is_notification = id.is_none();
    if is_notification {
        // notifications/initialized, notifications/cancelled, …: nothing to answer
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            let version = if SUPPORTED_VERSIONS.contains(&asked) {
                asked
            } else {
                PROTOCOL_VERSION
            };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": super::SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Analytics and inter-agent coordination over the agent-mux trace store: sessions, turns, tools, skills, subagents, tokens, cost, plus peer discovery, direct/broadcast messaging, cooperative file leases, shared task splitting and worker delegation.",
            })
        }
        "ping" => json!({}),
        "tools/list" => tools_list_value(),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            match name {
                "agent_mux_get_loop_context" => {
                    return Some(rpc_result(id, loop_context_result(service, arguments)));
                }
                "agent_mux_get_workflow_run" => {
                    return Some(rpc_result(id, workflow_run_result(service, arguments)));
                }
                "agent_mux_list_peers" => {
                    return Some(rpc_result(id, bus_list_peers_result(service, arguments)));
                }
                "agent_mux_send_message" => {
                    return Some(rpc_result(id, bus_send_message_result(service, arguments)));
                }
                "agent_mux_recv_messages" => {
                    return Some(rpc_result(id, bus_recv_messages_result(service, arguments)));
                }
                "agent_mux_task_board" => {
                    return Some(rpc_result(id, bus_task_board_result(service, arguments)));
                }
                "agent_mux_task_split" => {
                    return Some(rpc_result(id, bus_task_split_result(service, arguments)));
                }
                "agent_mux_task_update" => {
                    return Some(rpc_result(id, bus_task_update_result(service, arguments)));
                }
                "agent_mux_delegate" => {
                    return Some(rpc_result(id, bus_delegate_result(service, arguments)));
                }
                _ => {}
            }
            match Request::from_tool_call(name, arguments) {
                Err(e) => tool_error(&e),
                Ok(request) => match service.execute(request) {
                    Ok(envelope) => {
                        let value = serde_json::to_value(&envelope).unwrap_or(Value::Null);
                        let text = serde_json::to_string(&value).unwrap_or_default();
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "structuredContent": value,
                            "isError": false,
                        })
                    }
                    Err(e) => tool_error(&e),
                },
            }
        }
        other => return Some(rpc_error(id, -32601, format!("method not found: {other}"))),
    };
    Some(rpc_result(id, result))
}

/// Serves stdin/stdout until EOF.
pub fn serve_stdio(config: ServiceConfig) -> anyhow::Result<()> {
    let service = TraceService::new(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_message(&service, &line) {
            let text = serde_json::to_string(&reply)?;
            out.write_all(text.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// `agent-mux mcp serve --stdio [--db PATH] [--workspace DIR | --all-workspaces | --workspace-from-env]`.
pub fn serve_cli(args: &[String]) -> anyhow::Result<()> {
    let mut db: Option<PathBuf> = None;
    let mut scope: Option<Scope> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stdio" => {}
            "--db" => {
                i += 1;
                db = args.get(i).map(PathBuf::from);
            }
            "--workspace" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--workspace needs a directory"))?;
                scope = Some(Scope::workspace(Path::new(dir)).map_err(|e| anyhow::anyhow!("{e}"))?);
            }
            "--all-workspaces" => scope = Some(Scope::all_workspaces()),
            "--workspace-from-env" => {
                scope = Some(workspace_from_env());
            }
            other => anyhow::bail!("unknown serve option: {other}\n\n{}", super::USAGE),
        }
        i += 1;
    }
    let scope = scope.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|d| Scope::workspace(&d).ok())
            .unwrap_or_else(Scope::all_workspaces)
    });
    let db = db.unwrap_or_else(resolved_db_path);
    serve_stdio(ServiceConfig::new(db, scope))
}

/// `AGENT_MUX_WORKSPACE`, then the current directory, else all workspaces.
fn workspace_from_env() -> Scope {
    std::env::var("AGENT_MUX_WORKSPACE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| Scope::workspace(Path::new(s.trim())).ok())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| Scope::workspace(&d).ok())
        })
        .unwrap_or_else(Scope::all_workspaces)
}

/// `AGENT_MUX_TRACE_DB`, then the configured `db_path`, then the default.
fn resolved_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_TRACE_DB")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    if let Ok(cfg) = crate::config::load()
        && let Some(resolved) =
            crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
    {
        return resolved.db_path;
    }
    crate::tracing::analysis::default_trace_db_path()
}

/// Spawns this binary as a server against `db`, runs `initialize` and
/// `agent_mux_get_health`, and summarizes; bounded to five seconds.
pub fn selftest(exe: Option<&Path>, db: &Path) -> Result<String, String> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};
    let exe = exe
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| "cannot locate the agent-mux binary".to_string())?;
    let mut child = Command::new(&exe)
        .args(["mcp", "serve", "--stdio", "--all-workspaces", "--db"])
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", exe.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"agent-mux doctor","version":env!("CARGO_PKG_VERSION")}}});
    let health = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agent_mux_get_health","arguments":{}}});
    let list = json!({"jsonrpc":"2.0","id":3,"method":"tools/list"});
    for m in [init, health, list] {
        writeln!(stdin, "{m}").map_err(|e| e.to_string())?;
    }
    drop(stdin);
    let deadline = std::time::Duration::from_secs(5);
    let mut version = String::new();
    let mut db_available = None;
    let mut tools = 0usize;
    for _ in 0..3 {
        let line = rx
            .recv_timeout(deadline)
            .map_err(|_| "no answer within 5 s".to_string())?;
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))?;
        match v.get("id").and_then(Value::as_i64) {
            Some(1) => {
                version = v["result"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("?")
                    .to_string();
            }
            Some(2) => {
                db_available = v["result"]["structuredContent"]["data"]["db_available"].as_bool();
            }
            Some(3) => {
                tools = v["result"]["tools"].as_array().map_or(0, Vec::len);
            }
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(format!(
        "protocol {version}, {tools} tools, db_available={}",
        db_available.map_or("?".to_string(), |b| b.to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_trace_and_bus_tools() {
        let names = tool_names();
        assert_eq!(names.len(), 17);
        assert!(names.contains(&"agent_mux_get_loop_context"));
        assert!(names.contains(&"agent_mux_get_workflow_run"));
        assert!(names.contains(&"agent_mux_get_briefing"));
        assert!(names.contains(&"agent_mux_compare_runs"));
        assert!(names.contains(&"agent_mux_list_peers"));
        assert!(names.contains(&"agent_mux_send_message"));
        assert!(names.contains(&"agent_mux_recv_messages"));
        assert!(names.contains(&"agent_mux_task_board"));
        assert!(names.contains(&"agent_mux_task_split"));
        assert!(names.contains(&"agent_mux_task_update"));
        assert!(names.contains(&"agent_mux_delegate"));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("shell") || n.contains("write"))
        );
        let list = tools_list_value();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 17);
        for t in tools {
            assert_eq!(t["annotations"]["destructiveHint"], false);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn mcp_bus_message_and_task_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = crate::tracing::store::open_rw(&db, crate::tracing::store::OpenOptions::default())
            .unwrap();
        let service = TraceService::new(ServiceConfig::new(
            db,
            Scope::workspace(dir.path()).unwrap(),
        ))
        .unwrap();

        // Send a message from peer-a to peer-b
        let send_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "agent_mux_send_message",
                "arguments": {
                    "from": "peer-a",
                    "to": "peer-b",
                    "body": "API contract ready",
                    "topic": "contract",
                    "urgent": true
                }
            }
        });
        let send_resp = handle_message(&service, &send_req.to_string()).unwrap();
        assert_eq!(send_resp["result"]["isError"], false);

        // Receive message as peer-b
        let recv_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "agent_mux_recv_messages",
                "arguments": {
                    "recipient": "peer-b"
                }
            }
        });
        let recv_resp = handle_message(&service, &recv_req.to_string()).unwrap();
        assert_eq!(recv_resp["result"]["isError"], false);
        assert_eq!(recv_resp["result"]["structuredContent"]["data"]["count"], 1);
        assert_eq!(
            recv_resp["result"]["structuredContent"]["data"]["messages"][0]["body"],
            "API contract ready"
        );

        // Split tasks and claim/complete
        let split_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "agent_mux_task_split",
                "arguments": {
                    "created_by": "peer-a",
                    "tasks": [
                        {
                            "title": "Build schema",
                            "instructions": "Add v16 tables",
                            "files": ["src/schema.rs"]
                        }
                    ]
                }
            }
        });
        let split_resp = handle_message(&service, &split_req.to_string()).unwrap();
        assert_eq!(split_resp["result"]["isError"], false);
        let tid = split_resp["result"]["structuredContent"]["data"]["tasks"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let claim_req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "agent_mux_task_update",
                "arguments": {
                    "action": "claim",
                    "task_id": tid,
                    "session_id": "peer-b"
                }
            }
        });
        let claim_resp = handle_message(&service, &claim_req.to_string()).unwrap();
        assert_eq!(claim_resp["result"]["isError"], false);
        assert_eq!(
            claim_resp["result"]["structuredContent"]["data"]["leases"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
