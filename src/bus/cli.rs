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
use std::path::{Path, PathBuf};
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
                } else if matches!(
                    name,
                    "all" | "json" | "wake" | "no-ack" | "help" | "no-plan-gate" | "urgent"
                ) {
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
    if cmd == "swarm" {
        return run_swarm(&raw[1..]);
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
            // a swarm worker's `--to coordinator`: its swarm's coordinator
            let coordinator = match args.value("to") {
                Some(t) if t.eq_ignore_ascii_case("coordinator") => Some(
                    super::swarm::record::coordinator_address(&conn, &caller).ok_or_else(|| {
                        anyhow::anyhow!(
                            "you are not a worker of a swarm with a live coordinator: write to user"
                        )
                    })?,
                ),
                _ => None,
            };
            let to = coordinator.as_deref().or(args.value("to"));
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
            if args.has("wake") {
                let prompt = format!(
                    "[agent-mux bus] Message ({}) from peer {}: \"{}\". Reply or inspect with agent_mux_recv_messages.",
                    msg.topic, msg.from_launch, msg.body
                );
                let req = match msg.to_launch.as_deref() {
                    Some(target) => ControlRequest::WakeSession {
                        launch_id: target.to_string(),
                        prompt,
                    },
                    None => ControlRequest::WakeWorkspace {
                        workspace: workspace.clone(),
                        prompt,
                        exclude: Some(caller.clone()),
                    },
                };
                let _ = control::send_request(None, &req, Duration::from_secs(3));
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
            if propose_for_swarm_worker(&conn, &db_path, &caller, &spec, args.has("json"), now)? {
                return Ok(());
            }
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
                if propose_for_swarm_worker(&conn, &db_path, &caller, &spec, args.has("json"), now)?
                {
                    return Ok(());
                }
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
                    opening_prompt: None,
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

/// A swarm worker's `split`/`delegate`: proposed on its swarm's board
/// instead (`record::propose`). `None` when the caller is not a worker.
fn propose_for_swarm_worker(
    conn: &rusqlite::Connection,
    db: &Path,
    caller: &str,
    spec: &NewTaskSpec,
    json_out: bool,
    now: i64,
) -> anyhow::Result<bool> {
    use super::swarm::record;
    let Some((swarm, _)) = record::swarm_of_worker(conn, caller)? else {
        return Ok(false);
    };
    let made = record::propose(conn, Some(db), &swarm, std::slice::from_ref(spec), now)
        .map_err(|e| anyhow::anyhow!(e))?;
    let note = record::proposal_note(&swarm, &made);
    if json_out {
        println!(
            "{}",
            json!({ "proposed": made.iter().map(|(k, id)| json!({"key": k, "id": id})).collect::<Vec<_>>(), "note": note })
        );
    } else {
        println!("{note}");
    }
    Ok(true)
}

pub const SWARM_USAGE: &str = "usage: agent-mux swarm <command> [options]

A swarm is run by agent-mux: a headless planner splits the goal into tasks,
you approve the plan, and agent-mux starts one worker per task.

commands:
  start <goal...> [--planner claude|codex|agy|opencode] [--workers N] [--isolation shared|worktree] [--no-plan-gate] [--check CMD] [--workspace DIR]
                                start a swarm in the running TUI: plan, then wait for approval (unless --no-plan-gate)
  status [--workspace DIR] [--db PATH] [--json]
                                the swarm, its board (each task's state) and what waits for you
  plan [--workspace DIR] [--db PATH] [--json]
                                the board with the plan's errors and warnings
  approve                       approve the plan: workers start
  replan <notes...>             run the planner again with your notes (in review)
  pause | resume                stop starting new workers / start them again
  retry <task>                  put a failed, lost or cancelled task back on the board
  merge <task> | set-aside <task>
                                merge a finished task's branch into the workspace, or keep it unmerged
  accept <task> | drop <task>   accept or drop a task a worker proposed
  close                         end the swarm: stop its sessions, cancel open tasks, release leases
  restart                       start a new coordinator in place of one that exited
  submit-plan --file PLAN.json [--from LAUNCH_ID]
                                submit a plan as the coordinator does (agent_mux_swarm_plan): checked,
                                and on the board only without errors
  broadcast <message...> [--urgent] [--wake] [--workspace DIR] [--db PATH] [--json]
                                send a message to every agent of the workspace
  stop [--workspace DIR] [--db PATH] [--json]
                                like close; without a running TUI only the records are updated

<task> is a plan key (`bus-fix`), a task id or a unique prefix of either.
The verbs take --workspace DIR (default: the current folder) and --json.";

/// What the CLI knows of live sessions without a TUI: the published
/// snapshots of running agent-mux TUIs.
fn snapshot_workers() -> HashMap<String, super::swarm::engine::Worker> {
    use super::swarm::engine::{Worker, WorkerState};
    use crate::tracing::analysis::RuntimeState;
    let root = crate::tracing::analysis::default_snapshot_dir();
    crate::tracing::analysis::read_snapshots(&root, now_ns())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|s| s.sessions)
        .map(|s| {
            let state = match s.state {
                RuntimeState::Working => WorkerState::Working,
                RuntimeState::WaitingForUser => WorkerState::Prompt,
                RuntimeState::Idle | RuntimeState::Unknown => WorkerState::Idle,
                RuntimeState::Exited | RuntimeState::Disconnected => WorkerState::Exited,
            };
            (s.launch_id, Worker { state, quiet_ns: 0 })
        })
        .collect()
}

/// The checks `approve` re-runs, from this machine and the workspace.
fn cli_check_env(workspace: &str, isolation: TaskIsolation) -> super::swarm::plan::CheckEnv {
    super::swarm::plan::CheckEnv::for_workspace(Path::new(workspace), isolation)
}

/// `swarm status` / `swarm plan` for an orchestrated swarm.
fn print_board(
    conn: &rusqlite::Connection,
    swarm: &super::swarm::record::SwarmRun,
    with_problems: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    use super::swarm::{engine, plan, record};
    let board = record::board(conn, &swarm.id)?;
    let workers = snapshot_workers();
    let messages = bstore::list_workspace_messages(conn, &swarm.workspace, 500)?;
    let asking: std::collections::HashSet<String> = super::swarm::open_questions(&messages)
        .into_iter()
        .map(|m| m.from_launch.clone())
        .collect();
    let input = engine::Input {
        swarm,
        board: &board,
        workers: &workers,
        asking: &asking,
        now_ns: now_ns(),
    };
    let problems = with_problems.then(|| {
        plan::check(
            &record::board_plan(swarm, &board),
            &cli_check_env(&swarm.workspace, swarm.isolation),
        )
    });
    if json_out {
        let tasks: Vec<Value> = board
            .iter()
            .map(|t| {
                json!({
                    "key": t.ext.plan_key,
                    "id": t.task.id,
                    "title": t.task.title,
                    "state": engine::shown(t, &input).as_str(),
                    "depends_on": t.ext.depends_on,
                    "harness": t.task.target_harness,
                    "isolation": t.task.isolation.as_str(),
                    "assigned_to": t.task.assigned_to,
                    "branch": t.task.branch,
                    "review": t.review,
                    "attempts": t.ext.attempts,
                    "result": t.task.result,
                    "error": t.task.error,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "swarm": swarm,
                "busy_workers": engine::busy_workers(&input),
                "tasks": tasks,
                "problems": problems,
            }))?
        );
        return Ok(());
    }
    let done = board
        .iter()
        .filter(|t| t.task.status == TaskStatus::Done)
        .count();
    println!(
        "{} · {} · {done}/{} done · {}/{} workers",
        swarm.id,
        swarm.status.as_str(),
        board.len(),
        engine::busy_workers(&input),
        swarm.max_workers
    );
    println!("Goal: {}", swarm.goal);
    println!("Workspace: {}", swarm.workspace);
    println!(
        "Planner: {}{}",
        swarm.planner_harness,
        swarm
            .plan_summary
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" · {s}"))
            .unwrap_or_default()
    );
    if let Some(e) = &swarm.plan_error {
        println!("Planner error: {e}");
    }
    if let Some(cmd) = &swarm.check_cmd {
        match swarm
            .check_result
            .as_deref()
            .and_then(super::swarm::check::CheckResult::parse)
        {
            Some(r) => {
                println!("{}", r.summary(cmd));
                if !r.running && !r.passed && !r.tail.is_empty() {
                    for l in r.tail.lines() {
                        println!("    {l}");
                    }
                }
            }
            None => println!("Check when done: {cmd}"),
        }
    }
    if board.is_empty() {
        println!("(no tasks yet)");
    }
    for (i, t) in board.iter().enumerate() {
        let st = engine::shown(t, &input);
        let deps = if t.ext.depends_on.is_empty() {
            String::new()
        } else {
            format!(" ← {}", t.ext.depends_on.join(","))
        };
        println!(
            "{} {:>2} {:<16} {:<9} {}{} [{} · {}]",
            st.glyph(),
            i + 1,
            t.ext.plan_key,
            st.as_str(),
            t.task.title,
            deps,
            t.task.target_harness.as_deref().unwrap_or("?"),
            t.task.isolation.as_str(),
        );
        if st == engine::Shown::Blocked {
            println!(
                "        waits for: {}",
                engine::unmet_dependencies(t, &board).join(", ")
            );
        }
        if let Some(e) = &t.task.error {
            println!("        error: {e}");
        }
        if let Some(b) = &t.task.branch {
            println!(
                "        branch: {b}{}",
                t.review
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
        }
        if let Some(p) = &problems {
            for (is_err, m) in p.for_task(&t.ext.plan_key) {
                println!("        {} {m}", if is_err { "error:" } else { "warning:" });
            }
        }
    }
    if let Some(p) = &problems {
        for (k, m) in p
            .errors
            .iter()
            .chain(p.warnings.iter())
            .filter(|(k, _)| k.is_empty())
        {
            let _ = k;
            println!("plan: {m}");
        }
        if p.is_ok() {
            println!(
                "The plan has no errors{}.",
                if p.warnings.is_empty() {
                    ""
                } else {
                    " (see the warnings)"
                }
            );
        }
    }
    let next = match swarm.status {
        record::SwarmStatus::Planning => "the planner is working",
        record::SwarmStatus::Review => {
            "next: `agent-mux swarm approve`, or `agent-mux swarm replan <notes>`"
        }
        record::SwarmStatus::Integrating => {
            "next: `agent-mux swarm merge <task>` or `set-aside <task>` for each branch"
        }
        record::SwarmStatus::Finished => "finished: `agent-mux swarm close` ends it",
        _ => "",
    };
    if !next.is_empty() {
        println!("{next}");
    }
    Ok(())
}

pub fn run_swarm(raw: &[String]) -> anyhow::Result<()> {
    use super::swarm::record::{self, SwarmVerb};
    let cmd = raw.first().map(String::as_str).unwrap_or("status");
    if matches!(cmd, "--help" | "-h" | "help") {
        println!("{SWARM_USAGE}");
        return Ok(());
    }
    let rest = if raw.is_empty() { &[][..] } else { &raw[1..] };
    let args = Args::parse(rest);
    let workspace = resolve_workspace(&args);
    let db_path = resolve_db(&args);
    let now = now_ns();

    if let Some(verb) = SwarmVerb::parse(cmd).filter(|v| *v != SwarmVerb::Close || cmd == "close") {
        let task = verb
            .takes_task()
            .then(|| args.positional.first().cloned())
            .flatten();
        if verb.takes_task() && task.is_none() {
            anyhow::bail!("swarm {cmd} needs a task (its key or id)\n\n{SWARM_USAGE}");
        }
        let notes = (verb == SwarmVerb::Replan).then(|| args.positional.join(" "));
        let req = ControlRequest::SwarmAction {
            workspace: workspace.clone(),
            verb,
            task: task.clone(),
            notes,
        };
        let done = match control::send_request(None, &req, Duration::from_secs(10)) {
            Ok(resp) if resp.ok => resp.detail.unwrap_or_default(),
            Ok(resp) => anyhow::bail!("{}", resp.error.unwrap_or_default()),
            Err(e) if verb.needs_tui() => {
                anyhow::bail!("swarm {cmd} needs the running agent-mux TUI: {e}")
            }
            Err(_) => {
                let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
                let swarm = record::live_swarm(&conn, &workspace)?
                    .ok_or_else(|| anyhow::anyhow!("no swarm runs in {workspace}"))?;
                let env = cli_check_env(&swarm.workspace, swarm.isolation);
                let d = record::apply(
                    &conn,
                    Some(&db_path),
                    &swarm,
                    verb,
                    task.as_deref(),
                    &env,
                    now,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                format!("{d} (no agent-mux TUI is running: workers start when one does)")
            }
        };
        if args.has("json") {
            println!("{}", json!({ "ok": true, "detail": done }));
        } else {
            println!("{done}");
        }
        return Ok(());
    }

    match cmd {
        "submit-plan" => {
            let file = args
                .value("file")
                .ok_or_else(|| anyhow::anyhow!("--file PLAN.json is required"))?;
            let text = std::fs::read_to_string(file)?;
            let value: Value = serde_json::from_str(&text)?;
            // the tool's argument shape ({"plan": …}) or the plan itself
            let plan: super::swarm::plan::Plan =
                serde_json::from_value(value.get("plan").cloned().unwrap_or(value))?;
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let caller = bstore::resolve_caller_launch(args.value("from"));
            let swarm = caller
                .as_deref()
                .and_then(|c| record::swarm_of_coordinator(&conn, c).ok().flatten())
                .or_else(|| record::live_swarm(&conn, &workspace).ok().flatten())
                .ok_or_else(|| anyhow::anyhow!("no swarm to plan in {workspace}"))?;
            let env = cli_check_env(&swarm.workspace, swarm.isolation);
            let done = record::submit_plan(&conn, Some(&db_path), &swarm, &plan, &env, now)
                .map_err(|e| anyhow::anyhow!(e))?;
            let note = record::submitted_note(&swarm, &done);
            let accepted = matches!(done, record::Submitted::Accepted { .. });
            if args.has("json") {
                println!("{}", json!({ "accepted": accepted, "note": note }));
            } else {
                println!("{note}");
            }
            if !accepted {
                std::process::exit(2);
            }
        }
        "status" | "plan" => {
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            if let Some(swarm) = record::live_swarm(&conn, &workspace)? {
                return print_board(&conn, &swarm, cmd == "plan", args.has("json"));
            }
            if cmd == "plan" {
                anyhow::bail!("no swarm runs in {workspace}");
            }
            if args.has("json") {
                println!("{}", json!({ "workspace": workspace, "swarm": null }));
            } else {
                println!(
                    "No swarm runs in {workspace}. Start one with `agent-mux swarm start <goal>`."
                );
            }
        }
        "start" => {
            let goal_words = args.positional.clone();
            if goal_words.is_empty() {
                anyhow::bail!("swarm start requires a goal description\n\n{SWARM_USAGE}");
            }
            let goal = goal_words.join(" ");
            let planner = args.value("planner").map(str::to_string);
            let max_workers = args
                .value("workers")
                .and_then(|w| w.parse::<usize>().ok())
                .unwrap_or(3);
            let isolation = match args.value("isolation") {
                Some("worktree") => TaskIsolation::Worktree,
                Some("shared") | None => TaskIsolation::Shared,
                Some(other) => anyhow::bail!("--isolation is shared or worktree, not {other}"),
            };
            let req = ControlRequest::StartSwarm {
                workspace: workspace.clone(),
                goal,
                planner,
                max_workers,
                isolation,
                gate: Some(!args.has("no-plan-gate")),
                check: args.value("check").map(str::to_string),
            };
            match control::send_request(None, &req, Duration::from_secs(10)) {
                Ok(resp) if resp.ok => {
                    println!("{}", resp.detail.unwrap_or_default());
                    println!("Follow it with `agent-mux swarm status`.");
                }
                Ok(resp) => anyhow::bail!(
                    "could not start the swarm: {}",
                    resp.error.unwrap_or_default()
                ),
                Err(err) => anyhow::bail!(
                    "a swarm runs inside the running agent-mux TUI, and none answered: {err}"
                ),
            }
        }
        "broadcast" => {
            if args.positional.is_empty() {
                anyhow::bail!("swarm broadcast requires a message body\n\n{SWARM_USAGE}");
            }
            let body = args.positional.join(" ");
            let urgent = args.has("urgent");
            let wake = args.has("wake");
            let topic = if urgent { "blocker" } else { "directive" };
            let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
            let norm_ws = bstore::normalize_workspace(Path::new(&workspace));
            let msg = bstore::send_message(
                &conn,
                Some(&db_path),
                &norm_ws,
                "cli",
                None,
                topic,
                &body,
                None,
                now,
            )?;
            let woken = if wake {
                let req = ControlRequest::WakeWorkspace {
                    workspace: norm_ws.clone(),
                    prompt: format!("[Swarm Broadcast on {topic}]: {body}"),
                    exclude: None,
                };
                Some(control::send_request(None, &req, Duration::from_secs(3)))
            } else {
                None
            };
            if args.has("json") {
                let wake_json = match &woken {
                    Some(Ok(resp)) => serde_json::to_value(resp)?,
                    Some(Err(e)) => json!({ "ok": false, "error": e }),
                    None => serde_json::Value::Null,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "message": msg, "wake": wake_json }))?
                );
            } else {
                println!("Broadcast message sent (id: {})", msg.id);
                match &woken {
                    Some(Ok(resp)) => {
                        println!("{}", resp.detail.as_deref().unwrap_or("woke the swarm"))
                    }
                    Some(Err(e)) => println!("no session woken: {e}"),
                    None => {}
                }
            }
        }
        "stop" => {
            let norm_ws = bstore::normalize_workspace(Path::new(&workspace));
            // The TUI stops the sessions too; without it, the store alone.
            let req = ControlRequest::StopSwarm {
                workspace: norm_ws.clone(),
            };
            let summary = match control::send_request(None, &req, Duration::from_secs(3)) {
                Ok(resp) if resp.ok => resp.detail.unwrap_or_default(),
                Ok(resp) => anyhow::bail!(
                    "could not stop the swarm: {}",
                    resp.error.unwrap_or_default()
                ),
                Err(_) => {
                    let conn = open_aux(&db_path).map_err(|e| anyhow::anyhow!(e))?;
                    let (cancelled, released) = match record::live_swarm(&conn, &norm_ws)? {
                        Some(sw) => record::end_swarm(&conn, Some(&db_path), &sw, now),
                        None => bstore::stop_swarm(&conn, Some(&db_path), &norm_ws, now),
                    }
                    .map_err(|e| anyhow::anyhow!(e))?;
                    format!(
                        "cancelled {cancelled} task(s), released {released} lease(s); no agent-mux TUI is running, so no session was stopped"
                    )
                }
            };
            if args.has("json") {
                println!("{}", json!({ "stopped": summary }));
            } else {
                println!("Swarm stopped: {summary}");
            }
        }
        other => {
            anyhow::bail!("unknown swarm subcommand: {other}\n\n{SWARM_USAGE}");
        }
    }
    Ok(())
}
