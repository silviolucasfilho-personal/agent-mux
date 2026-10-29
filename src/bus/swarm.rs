//! Swarm snapshot and orchestration model (`src/bus/swarm.rs`).
//!
//! Aggregates the workspace's active Coordinator, Workers, Task Board,
//! Cooperative File Leases, and Inter-Agent Message Feed from `traces.db`
//! and live `App` sessions into a single [`SwarmSnapshot`].

use super::{
    BusMessage, BusTask, FileLease, PeerInfo, TaskIsolation, TaskStatus, short_id, store as bstore,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Role of an agent session inside a workspace swarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwarmRole {
    Coordinator,
    Worker,
}

impl SwarmRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SwarmRole::Coordinator => "coordinator",
            SwarmRole::Worker => "worker",
        }
    }
}

/// Live session telemetry supplied directly by the running `App` (when in TUI mode).
#[derive(Debug, Clone)]
pub struct LiveSwarmSession {
    pub session_idx: usize,
    pub session_id: usize,
    pub launch_id: String,
    pub provider: String,
    pub profile_name: String,
    pub agent_persona: Option<String>,
    pub state: String,
    pub workspace: String,
    pub is_delegated_worker: bool,
    pub turns: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    pub running_tool: Option<String>,
}

/// One member (Coordinator or Worker) of a workspace swarm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmMember {
    pub launch_id: String,
    pub session_idx: Option<usize>,
    pub session_id: Option<usize>,
    pub label: String,
    pub role: SwarmRole,
    pub provider: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_persona: Option<String>,
    pub state: String,
    pub active_tasks: Vec<String>,
    pub leased_files: Vec<String>,
    pub unread_messages: usize,
    pub turns: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl SwarmMember {
    pub fn is_active(&self) -> bool {
        self.state != "exited" && self.state != "disconnected"
    }
}

/// Complete point-in-time snapshot of a workspace swarm.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmSnapshot {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<SwarmMember>,
    pub workers: Vec<SwarmMember>,
    pub tasks: Vec<BusTask>,
    pub leases: Vec<FileLease>,
    pub messages: Vec<BusMessage>,
    pub total_turns: usize,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
}

impl SwarmSnapshot {
    /// All members in roster order: Coordinator first (if present), followed by Workers.
    pub fn all_members(&self) -> Vec<&SwarmMember> {
        let mut out =
            Vec::with_capacity(self.workers.len() + usize::from(self.coordinator.is_some()));
        if let Some(c) = &self.coordinator {
            out.push(c);
        }
        out.extend(self.workers.iter());
        out
    }

    /// Friendly display label for any launch ID in the message feed or task board.
    pub fn member_label(&self, launch_id: &str) -> String {
        if launch_id == "user" || launch_id == "operator" || launch_id == "cli" {
            return "User".to_string();
        }
        if let Some(c) = &self.coordinator
            && c.launch_id == launch_id
        {
            return format!("★ Coord ({})", c.provider);
        }
        for w in &self.workers {
            if w.launch_id == launch_id {
                return format!("{} ({})", w.label, w.provider);
            }
        }
        short_id(launch_id).to_string()
    }

    /// Counts tasks by status category: `(running, open_or_claimed, done, failed_or_cancelled)`.
    pub fn task_counts(&self) -> (usize, usize, usize, usize) {
        let (mut running, mut open, mut done, mut failed) = (0, 0, 0, 0);
        for t in &self.tasks {
            match t.status {
                TaskStatus::Running => running += 1,
                TaskStatus::Open | TaskStatus::Claimed => open += 1,
                TaskStatus::Done => done += 1,
                TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
            }
        }
        (running, open, done, failed)
    }
}

/// Builds a [`SwarmSnapshot`] for `workspace` by combining `traces.db` with optional
/// in-memory `live_sessions` from the running `App`.
pub fn load_snapshot(
    conn: Option<&Connection>,
    workspace_path: &Path,
    live_sessions: &[LiveSwarmSession],
    explicit_coordinator: Option<&str>,
    now: i64,
) -> SwarmSnapshot {
    let workspace = bstore::normalize_workspace(workspace_path);
    let mut tasks = Vec::new();
    let mut leases = Vec::new();
    let mut messages = Vec::new();
    let mut db_peers: Vec<PeerInfo> = Vec::new();
    let mut db_coord: Option<String> = None;
    let mut swarm_goal: Option<String> = None;

    if let Some(c) = conn {
        let _ = bstore::prune_stale_leases(c, now);
        tasks = bstore::list_tasks(c, &workspace, None, None, None).unwrap_or_default();
        leases = bstore::list_leases(c, &workspace, now).unwrap_or_default();
        messages = bstore::list_workspace_messages(c, &workspace, 150).unwrap_or_default();
        db_peers = bstore::list_peers(c, &workspace, None, true, now).unwrap_or_default();
        db_coord = bstore::active_coordinator_launch(c, &workspace);
        swarm_goal = messages
            .iter()
            .rev()
            .find(|m| m.topic == "swarm-coordinator")
            .map(|m| m.body.clone())
            .filter(|b| !b.starts_with("Swarm Coordinator set to "));
    }

    // Also include worktree paths created for tasks in this workspace so workers running
    // inside isolated worktrees are recognized as members of this workspace's swarm.
    let worktree_dirs: HashSet<String> = tasks
        .iter()
        .filter_map(|t| t.worktree.as_deref())
        .map(|p| bstore::normalize_workspace(Path::new(p)))
        .collect();

    let live_by_launch: HashMap<String, &LiveSwarmSession> = live_sessions
        .iter()
        .filter(|s| {
            let norm = bstore::normalize_workspace(Path::new(&s.workspace));
            norm == workspace || worktree_dirs.contains(&norm)
        })
        .map(|s| (s.launch_id.clone(), s))
        .collect();

    // Determine which launch_id is the Coordinator:
    // 1. Explicit coordinator override from `App`
    // 2. Latest `swarm-coordinator` bus record if that launch is known
    // 3. A live session in this workspace with `agent_persona == Some("coordinator")`
    // 4. A session that created tasks on the board (`created_by`) and is not a delegated worker
    let coord_launch_id: Option<String> = explicit_coordinator
        .map(str::to_string)
        .or(db_coord)
        .or_else(|| {
            live_by_launch
                .values()
                .find(|s| s.agent_persona.as_deref() == Some("coordinator"))
                .map(|s| s.launch_id.clone())
        })
        .or_else(|| {
            tasks.first().and_then(|t| {
                let creator = t.created_by.as_str();
                if creator != "user" && creator != "cli" && creator != "operator" {
                    Some(creator.to_string())
                } else {
                    None
                }
            })
        });

    let mut seen_launches = HashSet::new();
    let mut raw_members: Vec<SwarmMember> = Vec::new();

    // Merge live sessions first (freshest state and direct `session_idx`)
    let mut ordered_live: Vec<&LiveSwarmSession> = live_by_launch.values().copied().collect();
    ordered_live.sort_by_key(|s| s.session_idx);

    for ls in ordered_live {
        seen_launches.insert(ls.launch_id.clone());
        let is_coord = coord_launch_id.as_deref() == Some(ls.launch_id.as_str());
        let active_tasks: Vec<String> = tasks
            .iter()
            .filter(|t| {
                t.assigned_to.as_deref() == Some(ls.launch_id.as_str()) && !t.status.is_terminal()
            })
            .map(|t| format!("{}: {}", short_id(&t.id), t.title))
            .collect();
        let leased_files: Vec<String> = leases
            .iter()
            .filter(|l| l.owner_launch == ls.launch_id)
            .map(|l| l.pattern.clone())
            .collect();
        let unread_messages = conn
            .and_then(|c| bstore::count_unread_messages(c, &workspace, &ls.launch_id).ok())
            .unwrap_or(0);
        let assigned_task = tasks
            .iter()
            .rev()
            .find(|t| t.assigned_to.as_deref() == Some(ls.launch_id.as_str()));

        raw_members.push(SwarmMember {
            launch_id: ls.launch_id.clone(),
            session_idx: Some(ls.session_idx),
            session_id: Some(ls.session_id),
            label: String::new(),
            role: if is_coord {
                SwarmRole::Coordinator
            } else {
                SwarmRole::Worker
            },
            provider: ls.provider.clone(),
            profile: ls.profile_name.clone(),
            agent_persona: ls
                .agent_persona
                .clone()
                .or_else(|| assigned_task.and_then(|t| t.target_agent.clone())),
            state: ls.state.clone(),
            active_tasks,
            leased_files,
            unread_messages,
            turns: ls.turns,
            tokens: ls.tokens,
            cost_usd: ls.cost_usd,
            running_tool: ls.running_tool.clone(),
            worktree: assigned_task.and_then(|t| t.worktree.clone()),
            branch: assigned_task.and_then(|t| t.branch.clone()),
        });
    }

    // Add DB peers not already represented in `live_sessions`
    for peer in db_peers {
        if seen_launches.contains(&peer.launch_id) {
            continue;
        }
        let is_coord = coord_launch_id.as_deref() == Some(peer.launch_id.as_str());
        let has_swarm_activity = is_coord
            || !peer.active_tasks.is_empty()
            || !peer.leased_files.is_empty()
            || tasks
                .iter()
                .any(|t| t.assigned_to.as_deref() == Some(peer.launch_id.as_str()))
            || peer.state != "exited";
        if !has_swarm_activity {
            continue;
        }
        seen_launches.insert(peer.launch_id.clone());
        let (turns, tokens, cost_usd) = conn
            .and_then(|c| query_launch_totals(c, &peer.launch_id))
            .unwrap_or((0, 0, 0.0));
        let assigned_task = tasks
            .iter()
            .rev()
            .find(|t| t.assigned_to.as_deref() == Some(peer.launch_id.as_str()));

        raw_members.push(SwarmMember {
            launch_id: peer.launch_id.clone(),
            session_idx: None,
            session_id: peer.session_id,
            label: String::new(),
            role: if is_coord {
                SwarmRole::Coordinator
            } else {
                SwarmRole::Worker
            },
            provider: peer.provider,
            profile: peer.profile,
            agent_persona: assigned_task.and_then(|t| t.target_agent.clone()),
            state: peer.state,
            active_tasks: peer.active_tasks,
            leased_files: peer.leased_files,
            unread_messages: peer.unread_messages,
            turns,
            tokens,
            cost_usd,
            running_tool: None,
            worktree: assigned_task.and_then(|t| t.worktree.clone()),
            branch: assigned_task.and_then(|t| t.branch.clone()),
        });
    }

    let mut coordinator: Option<SwarmMember> = None;
    let mut workers: Vec<SwarmMember> = Vec::new();
    for mut m in raw_members {
        if m.role == SwarmRole::Coordinator && coordinator.is_none() {
            m.label = "★ Coord".into();
            coordinator = Some(m);
        } else {
            m.role = SwarmRole::Worker;
            let idx = workers.len() + 1;
            m.label = format!("W{idx}");
            workers.push(m);
        }
    }

    let mut total_turns = 0usize;
    let mut total_tokens = 0i64;
    let mut total_cost_usd = 0.0f64;
    if let Some(c) = &coordinator {
        total_turns += c.turns;
        total_tokens += c.tokens;
        total_cost_usd += c.cost_usd;
    }
    for w in &workers {
        total_turns += w.turns;
        total_tokens += w.tokens;
        total_cost_usd += w.cost_usd;
    }

    SwarmSnapshot {
        workspace,
        goal: swarm_goal,
        coordinator,
        workers,
        tasks,
        leases,
        messages,
        total_turns,
        total_tokens,
        total_cost_usd,
    }
}

fn query_launch_totals(conn: &Connection, launch_id: &str) -> Option<(usize, i64, f64)> {
    conn.query_row(
        "SELECT COUNT(DISTINCT t.id),
                COALESCE(SUM(o.total_tokens), 0),
                COALESCE(SUM(o.total_cost_usd), 0.0)
         FROM traces t
         LEFT JOIN observations o ON o.trace_id = t.id
         WHERE t.launch_id = ?1",
        params![launch_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?.max(0) as usize,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        },
    )
    .ok()
}

/// Composes the opening mission prompt handed to a newly spawned Swarm Coordinator.
pub fn compose_coordinator_prompt(
    goal: &str,
    max_workers: usize,
    isolation: TaskIsolation,
) -> String {
    format!(
        "You are the **Swarm Coordinator** for this workspace.\n\n\
         ### Swarm Mission Goal\n\
         {goal}\n\n\
         ### Swarm Operating Parameters\n\
         - **Max Parallel Workers**: {max_workers}\n\
         - **Default Task Isolation**: `{}` ({})\n\n\
         ### Your Coordination Playbook\n\
         1. **Orient**: Inspect the codebase (read-only) and call `agent_mux_list_peers` and `agent_mux_task_board`.\n\
         2. **Decompose & Delegate**:\n\
            - Break the mission into independent subtasks with clear `files` globs.\n\
            - If idle worker peers are already running in the workspace, publish subtasks with `agent_mux_task_split` and wake them via `agent_mux_send_message(wake = true)`.\n\
            - Otherwise, spawn worker sessions directly via `agent_mux_delegate` (setting `isolation = \"{}\"` and `wait_ms = 0` so workers execute in parallel).\n\
         3. **Monitor & Communicate**:\n\
            - Wait for worker updates and questions using `agent_mux_recv_messages(wait_ms = 30000)` and `agent_mux_task_board`.\n\
            - Answer questions with `agent_mux_send_message(to = \"<worker_launch_id>\", wake = true)`.\n\
            - Encourage workers on coupled components to exchange `api-contract` messages directly.\n\
         4. **Verify & Synthesize**:\n\
            - Once all tasks reach `done`, review the changes/results, run final verification, and present a concise summary.",
        isolation.as_str(),
        match isolation {
            TaskIsolation::Shared => "cooperative file leases enforced by PreToolUse guard",
            TaskIsolation::Worktree => "isolated git worktree branch per worker",
        },
        isolation.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::NewTaskSpec;
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    #[test]
    fn snapshot_identifies_coordinator_workers_tasks_and_messages() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-swarm".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());

        bstore::record_coordinator(
            &conn,
            Some(&db),
            &ws,
            "coord-11112222",
            "Build auth + UI in parallel",
            1_000,
        )
        .unwrap();

        let created = bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-11112222",
            &[NewTaskSpec {
                title: "Implement auth".into(),
                instructions: "Add JWT validation".into(),
                files: vec!["src/auth/**".into()],
                assigned_to: Some("worker-aaaa1111".into()),
                ..Default::default()
            }],
            2_000,
        )
        .unwrap();
        assert_eq!(created.len(), 1);

        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "worker-aaaa1111",
            Some("coord-11112222"),
            "question",
            "Should tokens expire in 1h?",
            None,
            3_000,
        )
        .unwrap();

        let live = vec![
            LiveSwarmSession {
                session_idx: 0,
                session_id: 1,
                launch_id: "coord-11112222".into(),
                provider: "claude".into(),
                profile_name: "Claude Code".into(),
                agent_persona: Some("coordinator".into()),
                state: "working".into(),
                workspace: ws.clone(),
                is_delegated_worker: false,
                turns: 4,
                tokens: 12_000,
                cost_usd: 0.15,
                running_tool: Some("agent_mux_task_board".into()),
            },
            LiveSwarmSession {
                session_idx: 1,
                session_id: 2,
                launch_id: "worker-aaaa1111".into(),
                provider: "codex".into(),
                profile_name: "codex · task".into(),
                agent_persona: Some("reviewer".into()),
                state: "working".into(),
                workspace: ws.clone(),
                is_delegated_worker: true,
                turns: 2,
                tokens: 5_000,
                cost_usd: 0.05,
                running_tool: None,
            },
        ];

        let snap = load_snapshot(Some(&conn), dir.path(), &live, None, 4_000);
        assert_eq!(snap.goal.as_deref(), Some("Build auth + UI in parallel"));
        let coord = snap.coordinator.as_ref().expect("coordinator present");
        assert_eq!(coord.launch_id, "coord-11112222");
        assert_eq!(coord.unread_messages, 1);
        assert_eq!(snap.workers.len(), 1);
        assert_eq!(snap.workers[0].label, "W1");
        assert_eq!(snap.workers[0].leased_files, vec!["src/auth/**"]);
        assert_eq!(snap.leases.len(), 1);
        assert_eq!(snap.messages.len(), 2);
        assert_eq!(snap.total_turns, 6);
        assert_eq!(snap.total_tokens, 17_000);
        assert_eq!(snap.member_label("coord-11112222"), "★ Coord (claude)");
        assert_eq!(snap.member_label("worker-aaaa1111"), "W1 (codex)");
    }
}
