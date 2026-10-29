//! `agent-mux bus …` — CLI entrypoint for inter-agent communication and task
//! coordination. Matches the MCP bus tools 1:1 so any harness or shell script
//! can discover peers, message, lock files, split/claim/complete tasks, and
//! delegate work to newly spawned sessions.

use super::{
    DEFAULT_LEASE_TTL_NS, NewTaskSpec, TaskIsolation, TaskStatus,
    control::{self, ControlRequest},
    store as bstore,
};
use crate::tracing::analysis::default_trace_db_path;
use crate::tracing::store::{now_ns, open_aux};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "usage: agent-mux bus <command> [options]

commands:
  peers [--all] [--workspace DIR] [--db PATH] [--json]
                                list active peer sessions, their state, tasks and file leases
  send <body...> [--to LAUNCH_ID] [--topic message|question|urgent] [--reply-to MSG_ID] [--wake] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
                                send a message to a peer (or broadcast when --to is omitted)
  recv [--from-peer LAUNCH_ID] [--all] [--no-ack] [--wait-ms MS] [--limit N] [--workspace DIR] [--db PATH] [--json]
                                read unread messages for this session
  tasks [--status S] [--assigned-to ID] [--parent ID] [--workspace DIR] [--db PATH] [--json]
                                list tasks and active file leases on the workspace board
  split --title TITLE --instructions TEXT [--files p1,p2] [--harness H] [--agent A] [--isolation shared|worktree] [--assign ID] [--parent ID] [--workspace DIR] [--db PATH] [--json]
                                create a task on the workspace board
  claim <task-id> [--from LAUNCH_ID] [--db PATH] [--json]
                                atomically claim an open task and acquire its file leases
  done <task-id> [--result JSON_OR_TEXT] [--from LAUNCH_ID] [--wake] [--db PATH] [--json]
                                mark a task done, release its leases, and notify its creator
  fail <task-id> <error...> [--from LAUNCH_ID] [--wake] [--db PATH] [--json]
                                mark a task failed, release its leases, and notify its creator
  wait <task-id...> [--wait-ms MS] [--db PATH] [--json]
                                wait until the given task(s) finish or timeout
  delegate --title TITLE --instructions TEXT [--task-id ID] [--harness claude|codex|agy] [--agent NAME] [--files p1,p2] [--isolation shared|worktree] [--wait-ms MS] [--workspace DIR] [--db PATH] [--json]
                                spawn a new worker session in agent-mux to execute a task
  lease <pattern...> [--task-id ID] [--ttl-s SEC] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
                                acquire cooperative file leases
  unlease [pattern...] [--task-id ID] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
                                release cooperative file leases";

struct Args {
    positional: Vec<String>,
    flags: HashMap<String, Option<String>>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut positional = Vec::new();
        let mut flags = HashMap::new();
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            if let Some(name) = a.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    flags.insert(k.to_string(), Some(v.to_string()));
                } else if matches!(name, "all" | "json" | "wake" | "no-ack" | "help") {
                    flags.insert(name.to_string(), None);
                } else {
                    let value = raw.get(i + 1).cloned();
                    if value.is_some() {
                        i += 1;
                    }
                    flags.insert(name.to_string(), value);
                }
            } else {
                positional.push(a.clone());
            }
            i += 1;
        }
        Args { positional, flags }
    }

    fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }
}

fn resolve_db(args: &Args) -> PathBuf {
    args.value("db")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("AGENT_MUX_TRACE_DB")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            crate::config::load()
                .ok()
                .and_then(|c| {
                    crate::config::resolve_tracing(c.tracing.as_ref(), &|k| std::env::var(k).ok())
                })
                .map(|r| r.db_path)
        })
        .unwrap_or_else(default_trace_db_path)
}

fn resolve_workspace(args: &Args) -> String {
    let dir = args
        .value("workspace")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("AGENT_MUX_WORKSPACE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    bstore::normalize_workspace(&dir)
}

fn resolve_caller(args: &Args) -> String {
    bstore::resolve_caller_launch(args.value("from").or_else(|| args.value("from-peer")))
        .unwrap_or_else(|| "cli".to_string())
}

pub fn run(raw: &[String]) -> anyhow::Result<()> {
    let Some(cmd) = raw.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(());
    };
    if matches!(cmd, "help" | "--help" | "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let args = Args::parse(&raw[1..]);
    if args.has("help") {
        println!("{USAGE}");
        return Ok(());
    }
    let db_path = resolve_db(&args);
    let workspace = resolve_workspace(&args);
    let caller = resolve_caller(&args);
    let now = now_ns();

    match cmd {
        "peers" => {
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let peers = bstore::list_peers(&conn, &workspace, Some(&caller), args.has("all"), now)?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&peers)?);
            } else if peers.is_empty() {
                println!("no active peers in {workspace}");
            } else {
                println!(
                    "{:<14} {:<10} {:<12} {:>6}  tasks / leases",
                    "launch_id", "harness", "state", "unread"
                );
                for p in peers {
                    let me = if p.is_caller { " *" } else { "" };
                    let mut detail = Vec::new();
                    if !p.active_tasks.is_empty() {
                        detail.push(format!("tasks=[{}]", p.active_tasks.join(", ")));
                    }
                    if !p.leased_files.is_empty() {
                        detail.push(format!("leases=[{}]", p.leased_files.join(", ")));
                    }
                    println!(
                        "{:<14} {:<10} {:<12} {:>6}  {}",
                        format!("{}{me}", p.launch_id),
                        p.provider,
                        p.state,
                        p.unread_messages,
                        detail.join(" ")
                    );
                }
            }
        }
        "send" => {
            let body = args
                .value("body")
                .map(str::to_string)
                .unwrap_or_else(|| args.positional.join(" "));
            anyhow::ensure!(!body.trim().is_empty(), "send requires a message body");
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let to = args.value("to");
            let topic = args.value("topic").unwrap_or("message");
            let msg = bstore::send_message(
                &conn,
                Some(&db_path),
                &workspace,
                &caller,
                to,
                topic,
                body.trim(),
                args.value("reply-to"),
                now,
            )?;
            if args.has("wake")
                && let Some(target) = msg.to_launch.as_deref()
            {
                let prompt = format!(
                    "[agent-mux bus] Message ({}) from peer {}: \"{}\". Reply or inspect with agent_mux_recv_messages.",
                    msg.topic, msg.from_launch, msg.body
                );
                let _ = control::send_request(
                    None,
                    &ControlRequest::WakeSession {
                        launch_id: target.to_string(),
                        prompt,
                    },
                    Duration::from_secs(3),
                );
            }
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&msg)?);
            } else {
                println!(
                    "sent {} ({}) to {}",
                    msg.id,
                    msg.topic,
                    msg.to_launch.as_deref().unwrap_or("workspace broadcast")
                );
            }
        }
        "recv" => {
            let wait_ms = args
                .value("wait-ms")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let limit = args
                .value("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50);
            let unread_only = !args.has("all");
            let ack = !args.has("no-ack");
            let msgs = if wait_ms > 0 {
                bstore::recv_messages_wait(
                    &db_path,
                    &workspace,
                    &caller,
                    unread_only,
                    ack,
                    limit,
                    wait_ms,
                )
                .map_err(|e| anyhow::anyhow!(e))?
            } else {
                let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
                bstore::recv_messages(&conn, &workspace, &caller, unread_only, ack, limit, now)?
            };
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&msgs)?);
            } else if msgs.is_empty() {
                println!("no messages");
            } else {
                for m in msgs {
                    println!(
                        "[{}] ({}) from {}: {}",
                        m.id, m.topic, m.from_launch, m.body
                    );
                }
            }
        }
        "tasks" => {
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let status = args.value("status").and_then(TaskStatus::parse);
            let tasks = bstore::list_tasks(
                &conn,
                &workspace,
                status,
                args.value("assigned-to"),
                args.value("parent"),
            )?;
            let leases = bstore::list_leases(&conn, &workspace, now)?;
            if args.has("json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "tasks": tasks, "leases": leases }))?
                );
            } else {
                if tasks.is_empty() {
                    println!("no tasks in {workspace}");
                } else {
                    for t in &tasks {
                        println!(
                            "{:<15} {:<10} {:<14} {}",
                            t.id,
                            t.status.as_str(),
                            t.assigned_to.as_deref().unwrap_or("-"),
                            t.title
                        );
                    }
                }
                if !leases.is_empty() {
                    println!("\nactive file leases:");
                    for l in leases {
                        println!(
                            "  {:<24} owner={} task={}",
                            l.pattern,
                            l.owner_launch,
                            l.task_id.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
        }
        "split" => {
            let title = args
                .value("title")
                .ok_or_else(|| anyhow::anyhow!("--title is required"))?
                .to_string();
            let instructions = args
                .value("instructions")
                .map(str::to_string)
                .unwrap_or_else(|| args.positional.join(" "));
            let files: Vec<String> = args
                .value("files")
                .map(|s| {
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let isolation = args
                .value("isolation")
                .and_then(TaskIsolation::parse)
                .unwrap_or_default();
            let spec = NewTaskSpec {
                title,
                instructions,
                files,
                parent_task_id: args.value("parent").map(str::to_string),
                assigned_to: args.value("assign").map(str::to_string),
                target_harness: args.value("harness").map(str::to_string),
                target_agent: args.value("agent").map(str::to_string),
                isolation,
            };
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let created =
                bstore::split_tasks(&conn, Some(&db_path), &workspace, &caller, &[spec], now)
                    .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&created[0])?);
            } else {
                println!("created {} ({})", created[0].id, created[0].title);
            }
        }
        "claim" => {
            let task_id = args
                .positional
                .first()
                .ok_or_else(|| anyhow::anyhow!("claim requires <task-id>"))?;
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let task = bstore::claim_task(&conn, Some(&db_path), task_id, &caller, now)
                .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&task)?);
            } else {
                println!("claimed {} ({})", task.id, task.title);
            }
        }
        "done" => {
            let task_id = args
                .positional
                .first()
                .ok_or_else(|| anyhow::anyhow!("done requires <task-id>"))?;
            let raw_res = args
                .value("result")
                .map(str::to_string)
                .or_else(|| (args.positional.len() > 1).then(|| args.positional[1..].join(" ")))
                .unwrap_or_else(|| "{\"status\":\"ok\"}".into());
            let result_val =
                serde_json::from_str::<Value>(&raw_res).unwrap_or(Value::String(raw_res));
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let task = bstore::update_task(
                &conn,
                Some(&db_path),
                task_id,
                &caller,
                TaskStatus::Done,
                Some(result_val),
                None,
                None,
                None,
                now,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("wake") && task.created_by != caller {
                let _ = control::send_request(
                    None,
                    &ControlRequest::WakeSession {
                        launch_id: task.created_by.clone(),
                        prompt: format!(
                            "[agent-mux bus] Delegated task {} ('{}') finished. Check agent_mux_task_board.",
                            task.id, task.title
                        ),
                    },
                    Duration::from_secs(3),
                );
            }
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&task)?);
            } else {
                println!("marked {} done", task.id);
            }
        }
        "fail" => {
            let task_id = args
                .positional
                .first()
                .ok_or_else(|| anyhow::anyhow!("fail requires <task-id>"))?;
            let err_msg = if args.positional.len() > 1 {
                args.positional[1..].join(" ")
            } else {
                args.value("error").unwrap_or("task failed").to_string()
            };
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let task = bstore::update_task(
                &conn,
                Some(&db_path),
                task_id,
                &caller,
                TaskStatus::Failed,
                None,
                Some(err_msg),
                None,
                None,
                now,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("wake") && task.created_by != caller {
                let _ = control::send_request(
                    None,
                    &ControlRequest::WakeSession {
                        launch_id: task.created_by.clone(),
                        prompt: format!(
                            "[agent-mux bus] Delegated task {} ('{}') failed: {}.",
                            task.id,
                            task.title,
                            task.error.as_deref().unwrap_or("")
                        ),
                    },
                    Duration::from_secs(3),
                );
            }
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&task)?);
            } else {
                println!("marked {} failed", task.id);
            }
        }
        "wait" => {
            anyhow::ensure!(
                !args.positional.is_empty(),
                "wait requires one or more <task-id>"
            );
            let wait_ms = args
                .value("wait-ms")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(10_000);
            let tasks = bstore::wait_tasks(&db_path, &args.positional, wait_ms)
                .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&tasks)?);
            } else {
                for t in tasks {
                    println!("{:<15} {:<10} {}", t.id, t.status.as_str(), t.title);
                }
            }
        }
        "delegate" => {
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let task = if let Some(tid) = args.value("task-id") {
                bstore::get_task(&conn, tid)?.ok_or_else(|| anyhow::anyhow!("no task '{tid}'"))?
            } else {
                let title = args
                    .value("title")
                    .ok_or_else(|| anyhow::anyhow!("--title or --task-id is required"))?
                    .to_string();
                let instructions = args
                    .value("instructions")
                    .map(str::to_string)
                    .unwrap_or_else(|| args.positional.join(" "));
                let files: Vec<String> = args
                    .value("files")
                    .map(|s| {
                        s.split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let isolation = args
                    .value("isolation")
                    .and_then(TaskIsolation::parse)
                    .unwrap_or_default();
                let spec = NewTaskSpec {
                    title,
                    instructions,
                    files,
                    parent_task_id: args.value("parent").map(str::to_string),
                    assigned_to: None,
                    target_harness: args.value("harness").map(str::to_string),
                    target_agent: args.value("agent").map(str::to_string),
                    isolation,
                };
                bstore::split_tasks(&conn, Some(&db_path), &workspace, &caller, &[spec], now)
                    .map_err(|e| anyhow::anyhow!(e))?
                    .into_iter()
                    .next()
                    .unwrap()
            };
            let resp = control::send_request(
                None,
                &ControlRequest::SpawnWorker {
                    parent_launch: Some(caller.clone()),
                    workspace: workspace.clone(),
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    instructions: task.instructions.clone(),
                    harness: args
                        .value("harness")
                        .map(str::to_string)
                        .or_else(|| task.target_harness.clone()),
                    agent: args
                        .value("agent")
                        .map(str::to_string)
                        .or_else(|| task.target_agent.clone()),
                    isolation: task.isolation,
                },
                Duration::from_secs(10),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if !resp.ok {
                anyhow::bail!(
                    "{}",
                    resp.error
                        .unwrap_or_else(|| "spawn worker failed".to_string())
                );
            }
            if let Some(wait_ms) = args.value("wait-ms").and_then(|s| s.parse::<u64>().ok())
                && wait_ms > 0
            {
                let _ = bstore::wait_tasks(&db_path, std::slice::from_ref(&task.id), wait_ms);
            }
            let updated = bstore::get_task(&conn, &task.id)?.unwrap_or(task);
            let out = json!({
                "task": updated,
                "spawn": resp,
            });
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!(
                    "delegated {} to launch {}",
                    updated.id,
                    resp.launch_id.as_deref().unwrap_or("?")
                );
            }
        }
        "lease" => {
            anyhow::ensure!(
                !args.positional.is_empty(),
                "lease requires one or more file/glob patterns"
            );
            let ttl_s = args
                .value("ttl-s")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(DEFAULT_LEASE_TTL_NS / 1_000_000_000);
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let leases = bstore::acquire_leases(
                &conn,
                &workspace,
                &caller,
                args.value("task-id"),
                &args.positional,
                ttl_s * 1_000_000_000,
                now,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&leases)?);
            } else {
                println!("acquired {} lease(s)", leases.len());
            }
        }
        "unlease" => {
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let count = bstore::release_leases(
                &conn,
                &workspace,
                &caller,
                args.value("task-id"),
                &args.positional,
            )?;
            if args.has("json") {
                println!("{}", json!({ "released": count }));
            } else {
                println!("released {count} lease(s)");
            }
        }
        other => {
            anyhow::bail!("unknown bus command: {other}\n\n{USAGE}");
        }
    }
    Ok(())
}
