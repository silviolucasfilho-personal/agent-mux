//! SQLite operations for the inter-agent bus (`agent_messages`, `agent_tasks`,
//! `agent_leases`, and peer discovery across `launches` + `LiveSnapshot`).

use super::{
    BusMessage, BusTask, DEFAULT_LEASE_TTL_NS, FileLease, MAX_WAIT_MS, NewTaskSpec, PeerInfo,
    TaskIsolation, TaskStatus,
};
use crate::tracing::analysis::{LiveSession, default_snapshot_dir, read_snapshots};
use crate::tracing::store::now_ns;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Resolves the caller's launch ID from explicit input or `$AGENT_MUX_SESSION_ID`.
pub fn resolve_caller_launch(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("AGENT_MUX_SESSION_ID")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// Normalizes a workspace path string for matching across tables.
pub fn normalize_workspace(workspace: &Path) -> String {
    std::fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Normalizes a relative file path or glob pattern inside a workspace.
pub fn normalize_rel_path(workspace: &str, path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    let ws = workspace.trim_end_matches('/');
    if !ws.is_empty()
        && let Some(rest) = p.strip_prefix(ws)
    {
        p = rest.trim_start_matches('/').to_string();
    }
    p.trim_start_matches("./").to_string()
}

/// Checks whether a lease pattern (exact relative path or glob) matches `rel_path`.
pub fn pattern_matches_path(pattern: &str, rel_path: &str) -> bool {
    let pat = pattern.trim().trim_start_matches("./");
    let rel = rel_path.trim().trim_start_matches("./");
    if pat.is_empty() || rel.is_empty() {
        return false;
    }
    if pat == rel {
        return true;
    }
    if let Some(dir) = pat.strip_suffix("/**")
        && (rel == dir || rel.starts_with(&format!("{dir}/")))
    {
        return true;
    }
    if let Ok(glob) = globset::GlobBuilder::new(pat)
        .literal_separator(true)
        .build()
        && glob.compile_matcher().is_match(rel)
    {
        return true;
    }
    false
}

/// Prunes expired file leases and leases owned by launches that have already exited.
pub fn prune_stale_leases(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM agent_leases
         WHERE expires_ns <= ?1
            OR EXISTS (
                SELECT 1 FROM launches l
                WHERE l.id = agent_leases.owner_launch
                  AND l.ended_ns IS NOT NULL
            )",
        params![now],
    )
}

/// Acquires cooperative file leases for `owner_launch`. Fails atomically if any
/// requested pattern conflicts with an active lease owned by another peer.
pub fn acquire_leases(
    conn: &Connection,
    workspace: &str,
    owner_launch: &str,
    task_id: Option<&str>,
    patterns: &[String],
    ttl_ns: i64,
    now: i64,
) -> Result<Vec<FileLease>, String> {
    let _ = prune_stale_leases(conn, now);
    let active = list_leases(conn, workspace, now).map_err(|e| e.to_string())?;
    let mut normalized = Vec::new();
    for raw in patterns {
        let pat = normalize_rel_path(workspace, raw);
        if pat.is_empty() {
            continue;
        }
        for existing in &active {
            if existing.owner_launch != owner_launch
                && (existing.pattern == pat
                    || pattern_matches_path(&existing.pattern, &pat)
                    || pattern_matches_path(&pat, &existing.pattern))
            {
                return Err(format!(
                    "path '{pat}' conflicts with active lease '{}' owned by peer {}{}",
                    existing.pattern,
                    existing.owner_launch,
                    existing
                        .task_id
                        .as_deref()
                        .map(|t| format!(" (task {t})"))
                        .unwrap_or_default()
                ));
            }
        }
        if !normalized.contains(&pat) {
            normalized.push(pat);
        }
    }

    let expires_ns = now.saturating_add(ttl_ns.max(1_000_000_000));
    let mut out = Vec::new();
    for pat in normalized {
        conn.execute(
            "INSERT INTO agent_leases (workspace, pattern, owner_launch, task_id, acquired_ns, expires_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace, pattern) DO UPDATE SET
               owner_launch = excluded.owner_launch,
               task_id = COALESCE(excluded.task_id, agent_leases.task_id),
               acquired_ns = excluded.acquired_ns,
               expires_ns = excluded.expires_ns",
            params![workspace, pat, owner_launch, task_id, now, expires_ns],
        )
        .map_err(|e| e.to_string())?;
        out.push(FileLease {
            workspace: workspace.to_string(),
            pattern: pat,
            owner_launch: owner_launch.to_string(),
            task_id: task_id.map(str::to_string),
            acquired_ns: now,
            expires_ns,
        });
    }
    Ok(out)
}

/// Releases leases owned by `owner_launch` (either specific patterns, a task's
/// leases, or all leases for that owner when both are empty/`None`).
pub fn release_leases(
    conn: &Connection,
    workspace: &str,
    owner_launch: &str,
    task_id: Option<&str>,
    patterns: &[String],
) -> rusqlite::Result<usize> {
    if let Some(tid) = task_id {
        return conn.execute(
            "DELETE FROM agent_leases WHERE workspace = ?1 AND task_id = ?2",
            params![workspace, tid],
        );
    }
    if patterns.is_empty() {
        return conn.execute(
            "DELETE FROM agent_leases WHERE workspace = ?1 AND owner_launch = ?2",
            params![workspace, owner_launch],
        );
    }
    let mut removed = 0;
    for raw in patterns {
        let pat = normalize_rel_path(workspace, raw);
        removed += conn.execute(
            "DELETE FROM agent_leases WHERE workspace = ?1 AND owner_launch = ?2 AND pattern = ?3",
            params![workspace, owner_launch, pat],
        )?;
    }
    Ok(removed)
}

/// Lists active (non-expired) file leases in `workspace`.
pub fn list_leases(
    conn: &Connection,
    workspace: &str,
    now: i64,
) -> rusqlite::Result<Vec<FileLease>> {
    let mut stmt = conn.prepare(
        "SELECT workspace, pattern, owner_launch, task_id, acquired_ns, expires_ns
         FROM agent_leases
         WHERE workspace = ?1 AND expires_ns > ?2
           AND NOT EXISTS (
               SELECT 1 FROM launches l
               WHERE l.id = agent_leases.owner_launch AND l.ended_ns IS NOT NULL
           )
         ORDER BY acquired_ns ASC, pattern ASC",
    )?;
    let rows = stmt.query_map(params![workspace, now], |r| {
        Ok(FileLease {
            workspace: r.get(0)?,
            pattern: r.get(1)?,
            owner_launch: r.get(2)?,
            task_id: r.get(3)?,
            acquired_ns: r.get(4)?,
            expires_ns: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Checks whether any path in `paths` is leased by a peer other than `launch_id`.
/// Returns `Some((rel_path, lease))` on the first conflict.
pub fn check_lease_conflict(
    conn: &Connection,
    workspace: &str,
    launch_id: &str,
    paths: &[String],
    now: i64,
) -> Option<(String, FileLease)> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_leases'",
            [],
            |_| Ok(true),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(false);
    if !exists {
        return None;
    }
    let leases = list_leases(conn, workspace, now).ok()?;
    if leases.is_empty() {
        return None;
    }
    for raw in paths {
        let rel = normalize_rel_path(workspace, raw);
        if rel.is_empty() {
            continue;
        }
        for lease in &leases {
            if lease.owner_launch != launch_id && pattern_matches_path(&lease.pattern, &rel) {
                return Some((rel, lease.clone()));
            }
        }
    }
    None
}

/// Lists active and recent peer launches in `workspace`.
pub fn list_peers(
    conn: &Connection,
    workspace: &str,
    caller_launch: Option<&str>,
    include_exited: bool,
    now: i64,
) -> rusqlite::Result<Vec<PeerInfo>> {
    let snapshot_dir = default_snapshot_dir();
    let live_map: HashMap<String, LiveSession> = read_snapshots(&snapshot_dir, now)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|s| s.sessions)
        .map(|ls| (ls.launch_id.clone(), ls))
        .collect();

    let leases = list_leases(conn, workspace, now).unwrap_or_default();
    let tasks = list_tasks(conn, workspace, None, None, None).unwrap_or_default();

    let mut stmt = conn.prepare(
        "SELECT id, agent_mux_session, profile, provider, cwd, started_ns, ended_ns
         FROM launches
         WHERE cwd = ?1
         ORDER BY started_ns DESC
         LIMIT 50",
    )?;
    let rows = stmt.query_map(params![workspace], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;

    let mut peers = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in rows {
        let (launch_id, mux_session, profile, provider, cwd, started_ns, ended_ns) = row?;
        let live = live_map.get(&launch_id);
        let state = if let Some(l) = live {
            match l.state {
                crate::tracing::analysis::RuntimeState::Working => "working",
                crate::tracing::analysis::RuntimeState::Idle => "idle",
                crate::tracing::analysis::RuntimeState::WaitingForUser => "waiting_for_user",
                crate::tracing::analysis::RuntimeState::Disconnected => "disconnected",
                crate::tracing::analysis::RuntimeState::Exited => "exited",
                crate::tracing::analysis::RuntimeState::Unknown => "running",
            }
        } else if ended_ns.is_some() {
            "exited"
        } else {
            "running"
        };
        if !include_exited && state == "exited" && caller_launch != Some(launch_id.as_str()) {
            continue;
        }
        seen.insert(launch_id.clone());
        let active_tasks: Vec<String> = tasks
            .iter()
            .filter(|t| {
                t.assigned_to.as_deref() == Some(launch_id.as_str()) && !t.status.is_terminal()
            })
            .map(|t| format!("{}: {} ({})", t.id, t.title, t.status.as_str()))
            .collect();
        let leased_files: Vec<String> = leases
            .iter()
            .filter(|l| l.owner_launch == launch_id)
            .map(|l| l.pattern.clone())
            .collect();
        let unread_messages = count_unread_messages(conn, workspace, &launch_id).unwrap_or(0);
        peers.push(PeerInfo {
            is_caller: caller_launch == Some(launch_id.as_str()),
            session_id: live
                .map(|l| l.session_id)
                .or_else(|| mux_session.and_then(|n| usize::try_from(n).ok())),
            launch_id,
            provider,
            profile,
            workspace: cwd,
            state: state.to_string(),
            active_tasks,
            leased_files,
            unread_messages,
            started_ns,
        });
    }

    // Also include live snapshot sessions in this workspace that don't have a launch row yet.
    for (launch_id, l) in live_map {
        if seen.contains(&launch_id) {
            continue;
        }
        let l_ws = normalize_workspace(&l.cwd);
        if l_ws != workspace {
            continue;
        }
        let state = match l.state {
            crate::tracing::analysis::RuntimeState::Working => "working",
            crate::tracing::analysis::RuntimeState::Idle => "idle",
            crate::tracing::analysis::RuntimeState::WaitingForUser => "waiting_for_user",
            crate::tracing::analysis::RuntimeState::Disconnected => "disconnected",
            crate::tracing::analysis::RuntimeState::Exited => "exited",
            crate::tracing::analysis::RuntimeState::Unknown => "running",
        };
        if !include_exited && state == "exited" {
            continue;
        }
        peers.push(PeerInfo {
            is_caller: caller_launch == Some(launch_id.as_str()),
            session_id: Some(l.session_id),
            launch_id: launch_id.clone(),
            provider: l.provider.unwrap_or_else(|| "unknown".into()),
            profile: "live".into(),
            workspace: l_ws,
            state: state.to_string(),
            active_tasks: Vec::new(),
            leased_files: Vec::new(),
            unread_messages: count_unread_messages(conn, workspace, &launch_id).unwrap_or(0),
            started_ns: l.updated_at_ns,
        });
    }

    Ok(peers)
}

/// Counts unread messages addressed to `to_launch` (or broadcast in `workspace`).
pub fn count_unread_messages(
    conn: &Connection,
    workspace: &str,
    to_launch: &str,
) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_messages
         WHERE workspace = ?1
           AND (to_launch = ?2 OR (to_launch IS NULL AND from_launch != ?2))
           AND acked_ns IS NULL",
        params![workspace, to_launch],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as usize)
}

/// Sends a message to a specific peer (`to_launch = Some(...)`) or broadcasts
/// to the workspace (`to_launch = None`).
#[allow(clippy::too_many_arguments)]
pub fn send_message(
    conn: &Connection,
    db_path: Option<&Path>,
    workspace: &str,
    from_launch: &str,
    to_launch: Option<&str>,
    topic: &str,
    body: &str,
    reply_to: Option<&str>,
    now: i64,
) -> rusqlite::Result<BusMessage> {
    let id = format!("msg-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let topic = if topic.trim().is_empty() {
        "message"
    } else {
        topic.trim()
    };
    let to_clean = to_launch
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "*");
    let reply_clean = reply_to.map(str::trim).filter(|s| !s.is_empty());
    conn.execute(
        "INSERT INTO agent_messages (id, workspace, from_launch, to_launch, topic, body, reply_to, created_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            workspace,
            from_launch,
            to_clean,
            topic,
            body,
            reply_clean,
            now
        ],
    )?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    Ok(BusMessage {
        id,
        workspace: workspace.to_string(),
        from_launch: from_launch.to_string(),
        to_launch: to_clean.map(str::to_string),
        topic: topic.to_string(),
        body: body.to_string(),
        reply_to: reply_clean.map(str::to_string),
        created_ns: now,
        delivered_ns: None,
        acked_ns: None,
    })
}

/// Reads messages addressed to `to_launch` (plus workspace broadcasts from other
/// peers). When `unread_only` is true, returns only un-acknowledged messages;
/// when `ack` is true, marks returned messages as delivered and acknowledged.
pub fn recv_messages(
    conn: &Connection,
    workspace: &str,
    to_launch: &str,
    unread_only: bool,
    ack: bool,
    limit: usize,
    now: i64,
) -> rusqlite::Result<Vec<BusMessage>> {
    let limit = limit.clamp(1, 200) as i64;
    let sql = if unread_only {
        "SELECT id, workspace, from_launch, to_launch, topic, body, reply_to, created_ns, delivered_ns, acked_ns
         FROM agent_messages
         WHERE workspace = ?1
           AND (to_launch = ?2 OR (to_launch IS NULL AND from_launch != ?2))
           AND acked_ns IS NULL
         ORDER BY created_ns ASC
         LIMIT ?3"
    } else {
        "SELECT id, workspace, from_launch, to_launch, topic, body, reply_to, created_ns, delivered_ns, acked_ns
         FROM agent_messages
         WHERE workspace = ?1
           AND (to_launch = ?2 OR (to_launch IS NULL AND from_launch != ?2))
         ORDER BY created_ns DESC
         LIMIT ?3"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![workspace, to_launch, limit], |r| {
        Ok(BusMessage {
            id: r.get(0)?,
            workspace: r.get(1)?,
            from_launch: r.get(2)?,
            to_launch: r.get(3)?,
            topic: r.get(4)?,
            body: r.get(5)?,
            reply_to: r.get(6)?,
            created_ns: r.get(7)?,
            delivered_ns: r.get(8)?,
            acked_ns: r.get(9)?,
        })
    })?;
    let mut msgs = Vec::new();
    for r in rows {
        msgs.push(r?);
    }
    if ack && !msgs.is_empty() {
        for m in &mut msgs {
            // Only mark direct messages acked in place; broadcasts addressed to NULL
            // are marked acked if addressed directly or if there is a single recipient.
            conn.execute(
                "UPDATE agent_messages
                 SET delivered_ns = COALESCE(delivered_ns, ?1),
                     acked_ns = COALESCE(acked_ns, ?1)
                 WHERE id = ?2",
                params![now, m.id],
            )?;
            m.delivered_ns = Some(m.delivered_ns.unwrap_or(now));
            m.acked_ns = Some(m.acked_ns.unwrap_or(now));
        }
    }
    Ok(msgs)
}

/// Long-polls for unread messages up to `wait_ms` (bounded by `MAX_WAIT_MS`).
pub fn recv_messages_wait(
    db_path: &Path,
    workspace: &str,
    to_launch: &str,
    unread_only: bool,
    ack: bool,
    limit: usize,
    wait_ms: u64,
) -> Result<Vec<BusMessage>, String> {
    let wait_ms = wait_ms.min(MAX_WAIT_MS);
    let start = Instant::now();
    let deadline = Duration::from_millis(wait_ms);
    loop {
        let conn = crate::tracing::store::open_aux(db_path)?;
        let msgs = recv_messages(
            &conn,
            workspace,
            to_launch,
            unread_only,
            ack,
            limit,
            now_ns(),
        )
        .map_err(|e| e.to_string())?;
        if !msgs.is_empty() || start.elapsed() >= deadline {
            return Ok(msgs);
        }
        drop(conn);
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Drains un-delivered urgent messages for `launch_id` during a `PreToolUse` hook
/// so they can be injected into `hookSpecificOutput.additionalContext`.
pub fn drain_urgent_for_hook(conn: &Connection, launch_id: &str, now: i64) -> Option<String> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_messages'",
            [],
            |_| Ok(true),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(false);
    if !exists {
        return None;
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, from_launch, topic, body
             FROM agent_messages
             WHERE to_launch = ?1
               AND delivered_ns IS NULL
               AND acked_ns IS NULL
               AND topic IN ('urgent', 'question', 'task-update')
             ORDER BY created_ns ASC
             LIMIT 5",
        )
        .ok()?;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map(params![launch_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()?
        .filter_map(Result::ok)
        .collect();
    if rows.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for (id, from, topic, body) in rows {
        let _ = conn.execute(
            "UPDATE agent_messages SET delivered_ns = ?1 WHERE id = ?2",
            params![now, id],
        );
        lines.push(format!("[agent-mux bus ({topic}) from {from}]: {body}"));
    }
    Some(lines.join("\n"))
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<BusTask> {
    let files_str: String = r.get(9)?;
    let isolation_str: String = r.get(10)?;
    let status_str: String = r.get(11)?;
    let result_str: Option<String> = r.get(12)?;
    let files: Vec<String> = serde_json::from_str(&files_str).unwrap_or_default();
    let result: Option<Value> = result_str.and_then(|s| serde_json::from_str(&s).ok());
    Ok(BusTask {
        id: r.get(0)?,
        workspace: r.get(1)?,
        created_by: r.get(2)?,
        parent_task_id: r.get(3)?,
        assigned_to: r.get(4)?,
        target_harness: r.get(5)?,
        target_agent: r.get(6)?,
        title: r.get(7)?,
        instructions: r.get(8)?,
        files,
        isolation: TaskIsolation::parse(&isolation_str).unwrap_or_default(),
        status: TaskStatus::parse(&status_str).unwrap_or(TaskStatus::Open),
        result,
        error: r.get(13)?,
        worktree: r.get(14)?,
        branch: r.get(15)?,
        created_ns: r.get(16)?,
        updated_ns: r.get(17)?,
        ended_ns: r.get(18)?,
    })
}

const TASK_COLS: &str = "id, workspace, created_by, parent_task_id, assigned_to, target_harness, \
    target_agent, title, instructions, files_json, isolation, status, result_json, error, \
    worktree, branch, created_ns, updated_ns, ended_ns";

/// Gets a single task by ID.
pub fn get_task(conn: &Connection, task_id: &str) -> rusqlite::Result<Option<BusTask>> {
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM agent_tasks WHERE id = ?1"),
        params![task_id],
        row_to_task,
    )
    .optional()
}

/// Lists tasks in `workspace`, optionally filtered by status, assignee, or parent task.
pub fn list_tasks(
    conn: &Connection,
    workspace: &str,
    status: Option<TaskStatus>,
    assigned_to: Option<&str>,
    parent_task_id: Option<&str>,
) -> rusqlite::Result<Vec<BusTask>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM agent_tasks
         WHERE workspace = ?1
           AND (?2 IS NULL OR status = ?2)
           AND (?3 IS NULL OR assigned_to = ?3)
           AND (?4 IS NULL OR parent_task_id = ?4)
         ORDER BY created_ns ASC"
    ))?;
    let status_str = status.map(|s| s.as_str().to_string());
    let rows = stmt.query_map(
        params![workspace, status_str, assigned_to, parent_task_id],
        row_to_task,
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Creates one or more tasks on the workspace board. If a task specifies
/// `assigned_to` and `files` in `shared` isolation, acquires file leases for
/// that assignee immediately.
pub fn split_tasks(
    conn: &Connection,
    db_path: Option<&Path>,
    workspace: &str,
    created_by: &str,
    specs: &[NewTaskSpec],
    now: i64,
) -> Result<Vec<BusTask>, String> {
    if specs.is_empty() {
        return Err("at least one task spec is required".into());
    }
    let _ = prune_stale_leases(conn, now);
    let mut created = Vec::with_capacity(specs.len());
    for spec in specs {
        let title = spec.title.trim();
        if title.is_empty() {
            return Err("task title cannot be empty".into());
        }
        let id = format!("task-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let files: Vec<String> = spec
            .files
            .iter()
            .map(|f| normalize_rel_path(workspace, f))
            .filter(|f| !f.is_empty())
            .collect();
        let assigned = spec
            .assigned_to
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let status = if assigned.is_some() {
            TaskStatus::Claimed
        } else {
            TaskStatus::Open
        };
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT INTO agent_tasks (
                id, workspace, created_by, parent_task_id, assigned_to,
                target_harness, target_agent, title, instructions, files_json,
                isolation, status, created_ns, updated_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                id,
                workspace,
                created_by,
                spec.parent_task_id.as_deref(),
                assigned,
                spec.target_harness.as_deref(),
                spec.target_agent.as_deref(),
                title,
                spec.instructions.trim(),
                files_json,
                spec.isolation.as_str(),
                status.as_str(),
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        if let Some(owner) = assigned
            && spec.isolation == TaskIsolation::Shared
            && !files.is_empty()
            && let Err(e) = acquire_leases(
                conn,
                workspace,
                owner,
                Some(&id),
                &files,
                DEFAULT_LEASE_TTL_NS,
                now,
            )
        {
            let _ = conn.execute("DELETE FROM agent_tasks WHERE id = ?1", params![id]);
            return Err(e);
        }

        created.push(BusTask {
            id,
            workspace: workspace.to_string(),
            created_by: created_by.to_string(),
            parent_task_id: spec.parent_task_id.clone(),
            assigned_to: assigned.map(str::to_string),
            target_harness: spec.target_harness.clone(),
            target_agent: spec.target_agent.clone(),
            title: title.to_string(),
            instructions: spec.instructions.trim().to_string(),
            files,
            isolation: spec.isolation,
            status,
            result: None,
            error: None,
            worktree: None,
            branch: None,
            created_ns: now,
            updated_ns: now,
            ended_ns: None,
        });
    }
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    Ok(created)
}

/// Atomically claims an `open` task (or a task already pre-assigned to `claimer_launch`)
/// and acquires its cooperative file leases when `isolation == shared`.
pub fn claim_task(
    conn: &Connection,
    db_path: Option<&Path>,
    task_id: &str,
    claimer_launch: &str,
    now: i64,
) -> Result<BusTask, String> {
    let Some(task) = get_task(conn, task_id).map_err(|e| e.to_string())? else {
        return Err(format!("no task '{task_id}'"));
    };
    if task.status != TaskStatus::Open
        && !(task.status == TaskStatus::Claimed
            && task.assigned_to.as_deref() == Some(claimer_launch))
    {
        return Err(format!(
            "task '{task_id}' cannot be claimed (status is {}, assigned to {:?})",
            task.status.as_str(),
            task.assigned_to
        ));
    }
    if task.isolation == TaskIsolation::Shared && !task.files.is_empty() {
        acquire_leases(
            conn,
            &task.workspace,
            claimer_launch,
            Some(&task.id),
            &task.files,
            DEFAULT_LEASE_TTL_NS,
            now,
        )?;
    }
    let updated = conn
        .execute(
            "UPDATE agent_tasks
             SET status = 'running',
                 assigned_to = ?1,
                 updated_ns = ?2
             WHERE id = ?3
               AND (status = 'open' OR (status = 'claimed' AND assigned_to = ?1))",
            params![claimer_launch, now, task_id],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        let _ = release_leases(conn, &task.workspace, claimer_launch, Some(task_id), &[]);
        return Err(format!(
            "task '{task_id}' was already claimed by another peer"
        ));
    }
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    get_task(conn, task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task '{task_id}' disappeared"))
}

/// Updates a task's status, result, error, or worktree/branch metadata.
/// Automatically releases the task's file leases when transitioning to a terminal state
/// (`done`, `failed`, `cancelled`) and posts a `task-update` message to the creator.
#[allow(clippy::too_many_arguments)]
pub fn update_task(
    conn: &Connection,
    db_path: Option<&Path>,
    task_id: &str,
    actor_launch: &str,
    status: TaskStatus,
    result: Option<Value>,
    error: Option<String>,
    worktree: Option<String>,
    branch: Option<String>,
    now: i64,
) -> Result<BusTask, String> {
    let Some(before) = get_task(conn, task_id).map_err(|e| e.to_string())? else {
        return Err(format!("no task '{task_id}'"));
    };
    let ended_ns = status.is_terminal().then_some(now);
    let result_str = result.as_ref().map(Value::to_string);
    conn.execute(
        "UPDATE agent_tasks
         SET status = ?1,
             assigned_to = COALESCE(assigned_to, ?2),
             result_json = COALESCE(?3, result_json),
             error = COALESCE(?4, error),
             worktree = COALESCE(?5, worktree),
             branch = COALESCE(?6, branch),
             updated_ns = ?7,
             ended_ns = COALESCE(?8, ended_ns)
         WHERE id = ?9",
        params![
            status.as_str(),
            actor_launch,
            result_str,
            error,
            worktree,
            branch,
            now,
            ended_ns,
            task_id,
        ],
    )
    .map_err(|e| e.to_string())?;

    if status.is_terminal() {
        let _ = release_leases(conn, &before.workspace, actor_launch, Some(task_id), &[]);
        if before.created_by != actor_launch {
            let summary = match status {
                TaskStatus::Done => format!(
                    "Task '{}' ({}) finished: {}",
                    before.title,
                    task_id,
                    result_str.as_deref().unwrap_or("ok")
                ),
                TaskStatus::Failed => format!(
                    "Task '{}' ({}) failed: {}",
                    before.title,
                    task_id,
                    error.as_deref().unwrap_or("unknown error")
                ),
                TaskStatus::Cancelled => {
                    format!("Task '{}' ({}) was cancelled.", before.title, task_id)
                }
                _ => String::new(),
            };
            if !summary.is_empty() {
                let _ = send_message(
                    conn,
                    None,
                    &before.workspace,
                    actor_launch,
                    Some(&before.created_by),
                    "task-update",
                    &summary,
                    None,
                    now,
                );
            }
        }
    }
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    get_task(conn, task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task '{task_id}' disappeared"))
}

/// Lists the most recent `limit` messages across the entire workspace in
/// chronological order (oldest first among the recent window).
pub fn list_workspace_messages(
    conn: &Connection,
    workspace: &str,
    limit: usize,
) -> rusqlite::Result<Vec<BusMessage>> {
    let limit = limit.clamp(1, 500) as i64;
    let mut stmt = conn.prepare(
        "SELECT id, workspace, from_launch, to_launch, topic, body, reply_to, created_ns, delivered_ns, acked_ns
         FROM (
             SELECT id, workspace, from_launch, to_launch, topic, body, reply_to, created_ns, delivered_ns, acked_ns
             FROM agent_messages
             WHERE workspace = ?1
             ORDER BY created_ns DESC
             LIMIT ?2
         )
         ORDER BY created_ns ASC",
    )?;
    let rows = stmt.query_map(params![workspace, limit], |r| {
        Ok(BusMessage {
            id: r.get(0)?,
            workspace: r.get(1)?,
            from_launch: r.get(2)?,
            to_launch: r.get(3)?,
            topic: r.get(4)?,
            body: r.get(5)?,
            reply_to: r.get(6)?,
            created_ns: r.get(7)?,
            delivered_ns: r.get(8)?,
            acked_ns: r.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// The swarm a workspace is running: who leads it and what it was started with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwarmState {
    pub workspace: String,
    pub coordinator_launch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolation>,
    pub started_ns: i64,
    pub updated_ns: i64,
}

/// Makes `coordinator_launch` the Swarm Coordinator of `workspace`. A `goal`
/// (and `max_workers`, `isolation`) given here replaces what was recorded;
/// `None` keeps it, so promoting another session keeps the swarm's goal.
#[allow(clippy::too_many_arguments)]
pub fn set_coordinator(
    conn: &Connection,
    db_path: Option<&Path>,
    workspace: &str,
    coordinator_launch: &str,
    goal: Option<&str>,
    max_workers: Option<usize>,
    isolation: Option<TaskIsolation>,
    now: i64,
) -> rusqlite::Result<SwarmState> {
    let goal = goal.map(str::trim).filter(|g| !g.is_empty());
    conn.execute(
        "INSERT INTO swarm_state (workspace, coordinator_launch, goal, max_workers, isolation, started_ns, updated_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (workspace) DO UPDATE SET
             coordinator_launch = excluded.coordinator_launch,
             goal = COALESCE(excluded.goal, swarm_state.goal),
             max_workers = COALESCE(excluded.max_workers, swarm_state.max_workers),
             isolation = COALESCE(excluded.isolation, swarm_state.isolation),
             updated_ns = excluded.updated_ns",
        params![
            workspace,
            coordinator_launch,
            goal,
            max_workers.map(|n| n as i64),
            isolation.map(TaskIsolation::as_str),
            now
        ],
    )?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    Ok(swarm_state(conn, workspace)?.expect("row written above"))
}

/// The swarm recorded for `workspace`, if one was started or a coordinator promoted.
pub fn swarm_state(conn: &Connection, workspace: &str) -> rusqlite::Result<Option<SwarmState>> {
    conn.query_row(
        "SELECT workspace, coordinator_launch, goal, max_workers, isolation, started_ns, updated_ns
         FROM swarm_state WHERE workspace = ?1",
        params![workspace],
        |r| {
            Ok(SwarmState {
                workspace: r.get(0)?,
                coordinator_launch: r.get(1)?,
                goal: r.get(2)?,
                max_workers: r.get::<_, Option<i64>>(3)?.map(|n| n.max(0) as usize),
                isolation: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| TaskIsolation::parse(&s)),
                started_ns: r.get(5)?,
                updated_ns: r.get(6)?,
            })
        },
    )
    .optional()
}

/// Forgets the swarm recorded for `workspace` (`agent-mux swarm stop`).
pub fn clear_swarm(
    conn: &Connection,
    db_path: Option<&Path>,
    workspace: &str,
) -> rusqlite::Result<usize> {
    let n = conn.execute(
        "DELETE FROM swarm_state WHERE workspace = ?1",
        params![workspace],
    )?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    Ok(n)
}

/// Puts a failed or abandoned task back on the board for another try: open,
/// nobody's, no result or error, its leases released. Its worktree and
/// branch stay, so a retry continues the work already there.
pub fn reopen_task(
    conn: &Connection,
    db_path: Option<&Path>,
    task_id: &str,
    now: i64,
) -> Result<BusTask, String> {
    let Some(before) = get_task(conn, task_id).map_err(|e| e.to_string())? else {
        return Err(format!("no task '{task_id}'"));
    };
    if before.status == TaskStatus::Done {
        return Err(format!("task '{}' is done; nothing to retry", before.title));
    }
    conn.execute(
        "UPDATE agent_tasks
         SET status = 'open',
             assigned_to = NULL,
             result_json = NULL,
             error = NULL,
             ended_ns = NULL,
             updated_ns = ?1
         WHERE id = ?2",
        params![now, task_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM agent_leases WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    get_task(conn, task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task '{task_id}' disappeared"))
}

/// Updates a task's title, instructions, and scoped file patterns.
pub fn update_task_spec(
    conn: &Connection,
    db_path: Option<&Path>,
    task_id: &str,
    title: &str,
    instructions: &str,
    files: &[String],
    now: i64,
) -> Result<BusTask, String> {
    let Some(before) = get_task(conn, task_id).map_err(|e| e.to_string())? else {
        return Err(format!("no task '{task_id}'"));
    };
    let clean_title = title.trim();
    if clean_title.is_empty() {
        return Err("task title cannot be empty".into());
    }
    let norm_files: Vec<String> = files
        .iter()
        .map(|f| normalize_rel_path(&before.workspace, f))
        .filter(|f| !f.is_empty())
        .collect();
    let files_json = serde_json::to_string(&norm_files).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "UPDATE agent_tasks
         SET title = ?1,
             instructions = ?2,
             files_json = ?3,
             updated_ns = ?4
         WHERE id = ?5",
        params![clean_title, instructions.trim(), files_json, now, task_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    get_task(conn, task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task '{task_id}' disappeared"))
}

/// Force-releases a single file lease by pattern in `workspace`, regardless of owner.
pub fn force_release_lease(
    conn: &Connection,
    db_path: Option<&Path>,
    workspace: &str,
    pattern: &str,
) -> rusqlite::Result<usize> {
    let pat = normalize_rel_path(workspace, pattern);
    let n = conn.execute(
        "DELETE FROM agent_leases WHERE workspace = ?1 AND pattern = ?2",
        params![workspace, pat],
    )?;
    if let Some(p) = db_path {
        crate::tracing::store::poke_wake(p);
    }
    Ok(n)
}

/// Waits until all specified `task_ids` reach a terminal state (`done`, `failed`, `cancelled`)
/// or `wait_ms` elapses.
pub fn wait_tasks(
    db_path: &Path,
    task_ids: &[String],
    wait_ms: u64,
) -> Result<Vec<BusTask>, String> {
    let wait_ms = wait_ms.min(MAX_WAIT_MS);
    let start = Instant::now();
    let deadline = Duration::from_millis(wait_ms);
    loop {
        let conn = crate::tracing::store::open_aux(db_path)?;
        let mut tasks = Vec::with_capacity(task_ids.len());
        let mut all_done = true;
        for id in task_ids {
            match get_task(&conn, id).map_err(|e| e.to_string())? {
                Some(t) => {
                    if !t.status.is_terminal() {
                        all_done = false;
                    }
                    tasks.push(t);
                }
                None => return Err(format!("no task '{id}'")),
            }
        }
        if all_done || start.elapsed() >= deadline {
            return Ok(tasks);
        }
        drop(conn);
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    fn test_db(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-bus".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        db
    }

    #[test]
    fn point_to_point_and_broadcast_messages_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let db = test_db(temp.path());
        let conn = open_aux(&db).unwrap();

        // Direct message from peer-a to peer-b
        let m1 = send_message(
            &conn,
            Some(&db),
            "/ws",
            "peer-a",
            Some("peer-b"),
            "urgent",
            "Auth schema updated",
            None,
            100,
        )
        .unwrap();
        assert_eq!(m1.to_launch.as_deref(), Some("peer-b"));

        // Broadcast from peer-a to workspace
        let _m2 = send_message(
            &conn,
            Some(&db),
            "/ws",
            "peer-a",
            None,
            "message",
            "Running integration tests",
            None,
            200,
        )
        .unwrap();

        assert_eq!(count_unread_messages(&conn, "/ws", "peer-b").unwrap(), 2);
        assert_eq!(
            count_unread_messages(&conn, "/ws", "peer-a").unwrap(),
            0,
            "sender does not see its own broadcast as unread"
        );

        // PreToolUse hook drains urgent message first
        let hook_ctx = drain_urgent_for_hook(&conn, "peer-b", 250).unwrap();
        assert!(hook_ctx.contains("Auth schema updated"), "{hook_ctx}");
        assert!(
            drain_urgent_for_hook(&conn, "peer-b", 260).is_none(),
            "delivered urgent message is not drained twice"
        );

        // Recv & ack
        let received = recv_messages(&conn, "/ws", "peer-b", true, true, 50, 300).unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(count_unread_messages(&conn, "/ws", "peer-b").unwrap(), 0);
    }

    #[test]
    fn task_splitting_claiming_and_cooperative_file_leases() {
        let temp = tempfile::tempdir().unwrap();
        let db = test_db(temp.path());
        let conn = open_aux(&db).unwrap();

        let created = split_tasks(
            &conn,
            Some(&db),
            "/ws",
            "lead-1",
            &[
                NewTaskSpec {
                    title: "Implement API".into(),
                    instructions: "Add endpoints in src/api/".into(),
                    files: vec!["src/api/**".into()],
                    ..Default::default()
                },
                NewTaskSpec {
                    title: "Write UI".into(),
                    instructions: "Update src/ui/".into(),
                    files: vec!["src/ui/mod.rs".into()],
                    ..Default::default()
                },
            ],
            1_000,
        )
        .unwrap();
        assert_eq!(created.len(), 2);
        let t1 = &created[0].id;

        // Peer-1 claims task 1 -> acquires lease on src/api/**
        let claimed = claim_task(&conn, Some(&db), t1, "worker-1", 2_000).unwrap();
        assert_eq!(claimed.status, TaskStatus::Running);
        assert_eq!(claimed.assigned_to.as_deref(), Some("worker-1"));

        // Peer-2 tries to claim the same task -> rejected
        assert!(claim_task(&conn, Some(&db), t1, "worker-2", 2_100).is_err());

        // Lease conflict check: worker-2 blocked on src/api/routes.rs, worker-1 permitted
        let conflict = check_lease_conflict(
            &conn,
            "/ws",
            "worker-2",
            &["/ws/src/api/routes.rs".into()],
            2_500,
        );
        assert!(conflict.is_some());
        let (rel, lease) = conflict.unwrap();
        assert_eq!(rel, "src/api/routes.rs");
        assert_eq!(lease.owner_launch, "worker-1");

        assert!(
            check_lease_conflict(
                &conn,
                "/ws",
                "worker-1",
                &["/ws/src/api/routes.rs".into()],
                2_500,
            )
            .is_none()
        );

        // Completing task 1 releases its lease and notifies lead-1
        let done = update_task(
            &conn,
            Some(&db),
            t1,
            "worker-1",
            TaskStatus::Done,
            Some(serde_json::json!({"endpoints": 3})),
            None,
            None,
            None,
            3_000,
        )
        .unwrap();
        assert_eq!(done.status, TaskStatus::Done);
        assert!(
            check_lease_conflict(
                &conn,
                "/ws",
                "worker-2",
                &["/ws/src/api/routes.rs".into()],
                3_100,
            )
            .is_none(),
            "lease released on task completion"
        );
        let lead_inbox = recv_messages(&conn, "/ws", "lead-1", true, true, 10, 3_200).unwrap();
        assert_eq!(lead_inbox.len(), 1);
        assert_eq!(lead_inbox[0].topic, "task-update");
    }
}
