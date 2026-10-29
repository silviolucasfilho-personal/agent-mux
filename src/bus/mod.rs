//! Inter-agent communication and task coordination bus.
//!
//! Harnesses (`claude`, `codex`, `agy`) run in separate PTY processes, each
//! carrying `$AGENT_MUX_SESSION_ID` (its `launch_id` in `traces.db`) and
//! `$AGENT_MUX_WORKSPACE`. This module provides:
//!
//! 1. **Peer discovery**: live sessions from `~/.agent-mux/snapshots/*.json`
//!    joined with `launches`, `agent_tasks` and `agent_leases` in `traces.db`.
//! 2. **Messaging**: point-to-point and workspace-broadcast messages in
//!    `agent_messages`, with mid-turn urgent delivery through `PreToolUse`
//!    hooks (`additionalContext`) and idle PTY wakeups through the TUI
//!    control socket (`$AGENT_MUX_CONTROL_SOCK`).
//! 3. **Task board & cooperative file leases**: atomic task splitting,
//!    claiming, status updates, long-polling waits, and `PreToolUse` write
//!    protection for files leased by another active peer.
//! 4. **Lead-to-worker delegation**: dynamic spawning of interactive or
//!    isolated worktree worker sessions on any harness through the running
//!    `agent-mux` TUI.

pub mod cli;
pub mod control;
pub mod store;
pub mod swarm;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default lease TTL (30 minutes in nanoseconds) when not specified.
pub const DEFAULT_LEASE_TTL_NS: i64 = 30 * 60 * 1_000_000_000;

/// Environment variable holding the current session's `launch_id`.
pub const ENV_SESSION_ID: &str = "AGENT_MUX_SESSION_ID";

/// Maximum long-poll wait duration allowed in a single MCP/CLI call (30 seconds).
pub const MAX_WAIT_MS: u64 = 30_000;

/// Short 8-character display prefix for a launch or task ID.
pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// Status of a coordinated task on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Open,
    Claimed,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<TaskStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(TaskStatus::Open),
            "claimed" => Some(TaskStatus::Claimed),
            "running" => Some(TaskStatus::Running),
            "done" => Some(TaskStatus::Done),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" | "canceled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

/// Workspace isolation mode for a task or delegated worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TaskIsolation {
    #[default]
    Shared,
    Worktree,
}

impl TaskIsolation {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskIsolation::Shared => "shared",
            TaskIsolation::Worktree => "worktree",
        }
    }

    pub fn parse(s: &str) -> Option<TaskIsolation> {
        match s.trim().to_ascii_lowercase().as_str() {
            "shared" | "" => Some(TaskIsolation::Shared),
            "worktree" => Some(TaskIsolation::Worktree),
            _ => None,
        }
    }
}

/// A live or recent peer session discovered in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PeerInfo {
    pub launch_id: String,
    pub session_id: Option<usize>,
    pub provider: String,
    pub profile: String,
    pub workspace: String,
    pub state: String,
    pub is_caller: bool,
    pub active_tasks: Vec<String>,
    pub leased_files: Vec<String>,
    pub unread_messages: usize,
    pub started_ns: i64,
}

/// A message on the inter-agent bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BusMessage {
    pub id: String,
    pub workspace: String,
    pub from_launch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_launch: Option<String>,
    pub topic: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub created_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_ns: Option<i64>,
}

/// A task on the shared workspace task board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BusTask {
    pub id: String,
    pub workspace: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent: Option<String>,
    pub title: String,
    pub instructions: String,
    pub files: Vec<String>,
    pub isolation: TaskIsolation,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub created_ns: i64,
    pub updated_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
}

/// A cooperative file or glob lease held by an agent in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileLease {
    pub workspace: String,
    pub pattern: String,
    pub owner_launch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub acquired_ns: i64,
    pub expires_ns: i64,
}

/// Specification for creating a new task in `split_tasks`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTaskSpec {
    pub title: String,
    pub instructions: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent: Option<String>,
    #[serde(default)]
    pub isolation: TaskIsolation,
}
