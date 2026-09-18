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
}

fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({"type": "object"}))
}

/// The ten read-only tools, in the order `tools/list` returns them.
pub fn tool_catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "agent_mux_get_briefing",
            description: "Briefing of active and recent sessions in the workspace scope: goal, current activity, files, commands, last output, tokens and cost per session. Defaults to the last 24 hours.",
            input_schema: schema_of::<BriefingArgs>(),
        },
        ToolSpec {
            name: "agent_mux_list_sessions",
            description: "Sessions in the workspace scope with turns, tools, tokens, cost and runtime state; filter by provider, runtime state and time window.",
            input_schema: schema_of::<ListSessionsArgs>(),
        },
        ToolSpec {
            name: "agent_mux_get_session",
            description: "One session's card by session key (optionally bound to a launch id).",
            input_schema: schema_of::<GetSessionArgs>(),
        },
        ToolSpec {
            name: "agent_mux_get_timeline",
            description: "A session's turns in order with their observations (tools, generations, errors) and durations.",
            input_schema: schema_of::<TimelineArgs>(),
        },
        ToolSpec {
            name: "agent_mux_search_traces",
            description: "Search prompts and outputs of recorded turns for a phrase within the scope and window.",
            input_schema: schema_of::<SearchArgs>(),
        },
        ToolSpec {
            name: "agent_mux_analyze_skills",
            description: "Per-skill attribution metrics: attributed calls, errors, latency percentiles, tokens, cost and turns loaded.",
            input_schema: schema_of::<AnalyzeSkillsArgs>(),
        },
        ToolSpec {
            name: "agent_mux_compare_runs",
            description: "Two launches side by side: turns, tools, tokens, cost and duration with deltas.",
            input_schema: schema_of::<CompareRunsArgs>(),
        },
        ToolSpec {
            name: "agent_mux_get_health",
            description: "Reader version, database availability, collector freshness and the tools this server offers. Never needs the database.",
            input_schema: schema_of::<HealthArgs>(),
        },
        ToolSpec {
            name: "agent_mux_get_loop_context",
            description: "The Loop Engineering context of a loop run, recomputed now: effective level and why, today's budget against the caps, circuit breaker state, gate globs, readiness score, recent runs and the human inbox. Defaults to the calling run (AGENT_MUX_LOOP_RUN_ID) or the workspace's loop.",
            input_schema: schema_of::<LoopContextArgs>(),
        },
        ToolSpec {
            name: "agent_mux_get_workflow_run",
            description: "A workflow run: its document, status, args, sessions (step, phase, harness, result kind, tokens, cost) and result. Defaults to the calling run (AGENT_MUX_WORKFLOW_RUN_ID); an id prefix is accepted.",
            input_schema: schema_of::<WorkflowRunArgs>(),
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
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
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
                "instructions": "Read-only analytics over the agent-mux trace store: sessions, turns, tools, skills, subagents, tokens and cost. Every response is an envelope with scope, window, coverage and warnings; treat coverage and warnings as part of the answer.",
            })
        }
        "ping" => json!({}),
        "tools/list" => tools_list_value(),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            if name == "agent_mux_get_loop_context" {
                return Some(rpc_result(id, loop_context_result(service, arguments)));
            }
            if name == "agent_mux_get_workflow_run" {
                return Some(rpc_result(id, workflow_run_result(service, arguments)));
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
    fn catalog_contains_exactly_ten_read_tools() {
        let names = tool_names();
        assert_eq!(names.len(), 10);
        assert!(names.contains(&"agent_mux_get_loop_context"));
        assert!(names.contains(&"agent_mux_get_workflow_run"));
        assert!(names.contains(&"agent_mux_get_briefing"));
        assert!(names.contains(&"agent_mux_compare_runs"));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("shell") || n.contains("write"))
        );
        let list = tools_list_value();
        for t in list["tools"].as_array().unwrap() {
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["annotations"]["destructiveHint"], false);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }
}
