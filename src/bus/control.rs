//! Control socket for real-time TUI actions from `agent-mux mcp serve` and
//! `agent-mux bus`: waking an idle peer session's PTY (`WakeSession`, or
//! every peer of a workspace with `WakeWorkspace`) and dynamically spawning a
//! delegated worker session (`SpawnWorker`).
//!
//! Unix domain stream socket at `<snapshot_dir>/bus.sock` (or
//! `$AGENT_MUX_CONTROL_SOCK`). Every child session spawned by `App` receives
//! `AGENT_MUX_CONTROL_SOCK`. When no TUI is listening (or on non-Unix),
//! `send_request` returns an informative `Err` and callers fall back to
//! durable SQLite rows.

use super::TaskIsolation;
use crate::events::AppEvent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// Environment variable pointing child sessions to the owning TUI's socket.
pub const ENV_CONTROL_SOCK: &str = "AGENT_MUX_CONTROL_SOCK";

/// Request sent over the control socket to the live `App` loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    /// Wake a peer session: if idle, paste `prompt` into its PTY and press
    /// Enter as a separate write shortly after (`app::WAKE_SUBMIT_DELAY`);
    /// otherwise queue it until the session becomes idle, for at most
    /// `app::WAKE_TTL`.
    WakeSession { launch_id: String, prompt: String },
    /// Wake every active session of `workspace` (and of the worktrees its
    /// tasks run in) the way `WakeSession` wakes one, except `exclude`: the
    /// sender of a broadcast. The response's `detail` counts the sessions
    /// reached.
    WakeWorkspace {
        workspace: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude: Option<String>,
    },
    /// Spawn a new worker session in `App` to execute `task_id`.
    SpawnWorker {
        parent_launch: Option<String>,
        workspace: String,
        task_id: String,
        title: String,
        instructions: String,
        harness: Option<String>,
        agent: Option<String>,
        isolation: TaskIsolation,
        /// The worker's whole opening message, instead of the one composed
        /// from `title` and `instructions` (the swarm dispatcher's worker
        /// prompt, `bus::swarm::prompt::worker_prompt`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opening_prompt: Option<String>,
        /// The worker's place in a swarm, for its launch row (v24).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        swarm: Option<crate::bus::swarm::SwarmLaunch>,
    },
    /// Start an orchestrated swarm for `workspace` (schema v21): agent-mux
    /// runs a headless planner on `goal`, shows the plan for approval (or
    /// approves it itself when `gate` is off) and dispatches the workers.
    /// `planner` defaults to the selected session's harness.
    StartSwarm {
        workspace: String,
        goal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        planner: Option<String>,
        max_workers: usize,
        isolation: TaskIsolation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check: Option<String>,
        /// The harness of tasks that name none; absent: the coordinator's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workers: Option<String>,
        /// How finished tasks are reviewed: `auto` (absent), `off` or a
        /// harness (`bus::swarm::review`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<String>,
        /// Whether merges wait for a check: `off` (absent), `on` (the
        /// check command) or a command of its own (`bus::swarm::gate`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge_gate: Option<String>,
    },
    /// One operator decision on `workspace`'s orchestrated swarm; `task` is
    /// a task id, id prefix or plan key, `notes` a re-plan's notes.
    SwarmAction {
        workspace: String,
        verb: crate::bus::swarm::record::SwarmVerb,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        /// The swarm, when the workspace runs several (schema v23).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        swarm: Option<String>,
        /// Who relays the decision (v24): the coordinator from its chat
        /// (`chat`, with the operator's words in `said` and its launch in
        /// `from`), the screen (`tui`) or the CLI (`cli`, the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<crate::bus::swarm::record::DecidedBy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        said: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// End `workspace`'s swarm: stop its worker sessions, cancel its open
    /// tasks, release its leases, end its row.
    StopSwarm { workspace: String },
}

/// Response returned from `App` to the control socket client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn err(msg: impl Into<String>) -> Self {
        ControlResponse {
            ok: false,
            launch_id: None,
            session_id: None,
            harness: None,
            worktree: None,
            branch: None,
            detail: None,
            error: Some(msg.into()),
        }
    }
}

/// Resolves the control socket path from `$AGENT_MUX_CONTROL_SOCK` or the
/// snapshot directory.
pub fn resolve_socket_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(val) = std::env::var(ENV_CONTROL_SOCK)
        && !val.trim().is_empty()
    {
        return PathBuf::from(val.trim());
    }
    crate::tracing::analysis::default_snapshot_dir().join("bus.sock")
}

/// Sends a synchronous request to the running `agent-mux` TUI over the Unix
/// control socket, bounded by `timeout`.
pub fn send_request(
    sock_path: Option<&Path>,
    req: &ControlRequest,
    timeout: Duration,
) -> Result<ControlResponse, String> {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let path = resolve_socket_path(sock_path);
        if !path.exists() {
            return Err(format!(
                "no live agent-mux control socket at {}",
                path.display()
            ));
        }
        let mut stream =
            UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        let payload = serde_json::to_string(req).map_err(|e| e.to_string())?;
        stream
            .write_all(payload.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .and_then(|_| stream.flush())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        reader
            .read_line(&mut line)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if line.trim().is_empty() {
            return Err("empty response from agent-mux control socket".into());
        }
        serde_json::from_str::<ControlResponse>(&line)
            .map_err(|e| format!("invalid control response: {e}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (sock_path, req, timeout);
        Err("control socket is only supported on Unix".into())
    }
}

/// Guard removing the control socket file when dropped.
pub struct ControlListenerGuard {
    pub path: PathBuf,
}

impl Drop for ControlListenerGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds a Unix domain socket at `path` and forwards every incoming
/// `ControlRequest` into `tx` as `AppEvent::BusControl`.
#[cfg(unix)]
pub fn spawn_listener(path: PathBuf, tx: Sender<AppEvent>) -> Option<ControlListenerGuard> {
    if tokio::runtime::Handle::try_current().is_err() {
        return None;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).ok()?;
    let guard_path = path.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                if let Ok(Some(line)) = lines.next_line().await {
                    let resp = match serde_json::from_str::<ControlRequest>(&line) {
                        Ok(req) => {
                            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                            if tx
                                .send(AppEvent::BusControl {
                                    request: req,
                                    reply: reply_tx,
                                })
                                .await
                                .is_ok()
                            {
                                match tokio::time::timeout(Duration::from_secs(10), reply_rx).await
                                {
                                    Ok(Ok(r)) => r,
                                    _ => ControlResponse::err("timed out waiting for App"),
                                }
                            } else {
                                ControlResponse::err("agent-mux event loop is closed")
                            }
                        }
                        Err(e) => ControlResponse::err(format!("bad control request: {e}")),
                    };
                    if let Ok(text) = serde_json::to_string(&resp) {
                        let _ = writer.write_all(text.as_bytes()).await;
                        let _ = writer.write_all(b"\n").await;
                        let _ = writer.flush().await;
                    }
                }
            });
        }
    });
    Some(ControlListenerGuard { path: guard_path })
}

#[cfg(not(unix))]
pub fn spawn_listener(_path: PathBuf, _tx: Sender<AppEvent>) -> Option<ControlListenerGuard> {
    None
}

/// Composes the opening prompt handed to a delegated worker session.
pub fn compose_worker_prompt(
    task_id: &str,
    title: &str,
    instructions: &str,
    files: &[String],
    parent_launch: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You have been delegated task `{task_id}`: **{title}**.\n\n\
         ### Instructions\n{instructions}\n"
    );
    if !files.is_empty() {
        prompt.push_str(&format!(
            "\n### Scoped Files / Leases\n{}\n",
            files
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(parent) = parent_launch {
        prompt.push_str(&format!("\nDelegated by peer launch: `{parent}`.\n"));
    }
    prompt.push_str(&format!(
        "\n### Reporting Completion\n\
         When you finish (or if you are blocked), report your structured result on the agent-mux bus:\n\
         - Via MCP tool: call `agent_mux_task_update` with `task_id = \"{task_id}\"`, `action = \"done\"` (or `\"failed\"`), and `result = {{...}}`.\n\
         - Or via CLI: `agent-mux bus done {task_id} --result '<json>'`."
    ));
    prompt
}
