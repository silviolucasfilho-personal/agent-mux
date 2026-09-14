//! Query engine for factual session briefings, recaps, and scope rollups.

use super::evidence::{extract_command, extract_target_file, snippet};
use super::model::{
    AnalysisError, Briefing, Evidence, EvidenceSource, LiveSession, RuntimeState,
    SessionCard, TaskOutcome, ToolCountSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::path::Path;

/// Generates an executive briefing across active and historical sessions within a time window.
///
/// Merges live sessions with historical sessions, deduplicating history already represented
/// by a live launch. Computes uncapped totals before applying preview display limits.
pub fn briefing(
    conn: &Connection,
    workspace: &Path,
    since_ns: i64,
    until_ns: i64,
    live: &[LiveSession],
) -> Result<Briefing, AnalysisError> {
    let mut cards = Vec::new();
    let mut seen_session_keys = HashSet::new();
    let mut seen_launch_ids = HashSet::new();
    let ws_str = workspace.to_string_lossy().to_string();

    // 1. Process Live Sessions first
    for s in live {
        if !s.cwd.as_os_str().is_empty() && s.cwd != workspace {
            // Out of workspace scope
            continue;
        }

        if let Some(ref k) = s.session_key {
            seen_session_keys.insert(k.clone());
        }
        seen_launch_ids.insert(s.launch_id.clone());

        let card = build_session_card(
            conn,
            s.session_key.as_deref(),
            Some(&s.launch_id),
            s.provider.as_deref(),
            &s.cwd,
            s.state,
            TaskOutcome::Unknown,
            &s.active_tools,
        )?;
        cards.push(card);
    }

    // 2. Query Historical Sessions from SQLite
    // Check if sessions or traces table exists
    let has_sessions_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'sessions'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    if has_sessions_table {
        let mut stmt = conn.prepare(
            "SELECT key, provider, cwd, first_seen_ns, last_seen_ns
             FROM sessions
             WHERE (last_seen_ns >= ?1 AND first_seen_ns < ?2)
               AND (cwd IS NULL OR cwd = '' OR cwd = ?3)
             ORDER BY last_seen_ns DESC",
        )?;

        let rows = stmt.query_map(params![since_ns, until_ns, ws_str], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;

        for row in rows.flatten() {
            let (key, provider, cwd_str, _first_seen, _last_seen) = row;
            if seen_session_keys.contains(&key) {
                continue;
            }
            seen_session_keys.insert(key.clone());

            let card_cwd = cwd_str
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());

            let card = build_session_card(
                conn,
                Some(&key),
                None,
                provider.as_deref(),
                &card_cwd,
                RuntimeState::Exited,
                TaskOutcome::Unknown,
                &[],
            )?;
            cards.push(card);
        }
    }

    // 3. Roll up workspace scope totals
    let total_sessions = cards.len();
    let mut total_turns = 0;
    let mut total_tools = 0;
    let mut total_tokens_sum: i64 = 0;
    let mut has_tokens = false;
    let mut total_cost_sum: f64 = 0.0;
    let mut has_cost = false;

    for card in &cards {
        total_turns += card.completed_turns + card.open_turns;
        total_tools += card.total_tools;
        if let Some(tok) = card.total_tokens {
            total_tokens_sum += tok;
            has_tokens = true;
        }
        if let Some(cost) = card.total_cost_usd {
            total_cost_sum += cost;
            has_cost = true;
        }
    }

    Ok(Briefing {
        cards,
        scope_workspace: workspace.to_path_buf(),
        since_ns,
        until_ns,
        total_sessions,
        total_turns,
        total_tools,
        total_tokens: if has_tokens { Some(total_tokens_sum) } else { None },
        total_cost_usd: if has_cost { Some(total_cost_sum) } else { None },
        warnings: Vec::new(),
    })
}

fn build_session_card(
    conn: &Connection,
    session_key: Option<&str>,
    launch_id: Option<&str>,
    provider: Option<&str>,
    cwd: &Path,
    runtime_state: RuntimeState,
    task_outcome: TaskOutcome,
    live_active_tools: &[String],
) -> Result<SessionCard, AnalysisError> {
    let skey = session_key.unwrap_or("");
    let lid = launch_id.unwrap_or("");

    // Check if traces table exists
    let has_traces: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'traces'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    let mut initial_goal = Evidence::missing(EvidenceSource::Transcript, "No initial goal recorded");
    let mut completed_turns = 0;
    let mut open_turns = 0;
    let mut last_assistant_output = None;
    let mut last_active_ns = 0;
    let mut duration_ms = 0;
    let mut total_tokens = None;
    let mut total_cost_usd = None;

    if has_traces && (!skey.is_empty() || !lid.is_empty()) {
        // Initial goal: first input
        if let Ok(Some(first_in)) = conn
            .query_row(
                "SELECT input FROM traces
                 WHERE ((?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2))
                   AND input IS NOT NULL AND trim(input) != ''
                 ORDER BY start_ns ASC LIMIT 1",
                params![skey, lid],
                |r| r.get::<_, String>(0),
            )
            .optional()
        {
            initial_goal = Evidence::observed(
                snippet(&first_in, 200),
                EvidenceSource::Transcript,
                None,
            );
        }

        // Turns count
        if let Ok(total) = conn.query_row(
            "SELECT COUNT(*) FROM traces WHERE (?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2)",
            params![skey, lid],
            |r| r.get::<_, i64>(0),
        ) {
            completed_turns = total;
        }

        // Open turns count
        if let Ok(open) = conn.query_row(
            "SELECT COUNT(*) FROM traces WHERE ((?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2)) AND end_ns IS NULL",
            params![skey, lid],
            |r| r.get::<_, i64>(0),
        ) {
            open_turns = open;
            completed_turns = completed_turns.saturating_sub(open);
        }

        // Latest assistant output
        if let Ok(Some(last_out)) = conn
            .query_row(
                "SELECT output FROM traces
                 WHERE ((?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2))
                   AND output IS NOT NULL AND trim(output) != ''
                 ORDER BY start_ns DESC LIMIT 1",
                params![skey, lid],
                |r| r.get::<_, String>(0),
            )
            .optional()
        {
            last_assistant_output = Some(Evidence::observed(
                snippet(&last_out, 300),
                EvidenceSource::Transcript,
                None,
            ));
        }

        // Timing & Resources (if trace_stats view or columns exist)
        let has_trace_stats: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'trace_stats'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        if has_trace_stats {
            if let Ok(row) = conn.query_row(
                "SELECT SUM(total_tokens), SUM(total_cost_usd), MIN(start_ns), MAX(COALESCE(end_ns, start_ns))
                 FROM trace_stats
                 WHERE (?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2)",
                params![skey, lid],
                |r| Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                )),
            ) {
                total_tokens = row.0;
                total_cost_usd = row.1;
                if let (Some(start), Some(end)) = (row.2, row.3) {
                    last_active_ns = end;
                    duration_ms = ((end.saturating_sub(start)) / 1_000_000).max(0) as u64;
                }
            }
        }
    }

    // Observations queries (tools, modified files, commands)
    let has_obs: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'observations'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    let mut total_tools = 0;
    let mut tool_counts = Vec::new();
    let mut files_modified = Vec::new();
    let mut recent_commands = Vec::new();
    let mut db_active_tools = Vec::new();

    if has_obs && (!skey.is_empty() || !lid.is_empty()) {
        // Tool count and breakdown
        if let Ok(mut stmt) = conn.prepare(
            "SELECT o.name, COUNT(*)
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
               AND o.type = 'tool'
             GROUP BY o.name
             ORDER BY COUNT(*) DESC",
        ) {
            if let Ok(rows) = stmt.query_map(params![skey, lid], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "unknown".into()), r.get::<_, i64>(1)?))
            }) {
                for (name, count) in rows.flatten() {
                    total_tools += count;
                    tool_counts.push(ToolCountSummary { name, count });
                }
            }
        }

        // Files modified
        if let Ok(mut stmt) = conn.prepare(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
               AND o.type = 'tool'
             ORDER BY o.start_ns DESC",
        ) {
            let mut seen_files = HashSet::new();
            if let Ok(rows) = stmt.query_map(params![skey, lid], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            }) {
                for (name, input) in rows.flatten() {
                    if let Some(file_path) = extract_target_file(&name, &input) {
                        if seen_files.insert(file_path.clone()) {
                            files_modified.push(Evidence::observed(
                                file_path,
                                EvidenceSource::Hook,
                                None,
                            ));
                            if files_modified.len() >= 10 {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Recent commands
        if let Ok(mut stmt) = conn.prepare(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
               AND o.type = 'tool'
             ORDER BY o.start_ns DESC",
        ) {
            let mut seen_cmds = HashSet::new();
            if let Ok(rows) = stmt.query_map(params![skey, lid], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            }) {
                for (name, input) in rows.flatten() {
                    if let Some(cmd) = extract_command(&name, &input) {
                        if seen_cmds.insert(cmd.clone()) {
                            recent_commands.push(Evidence::observed(
                                snippet(&cmd, 60),
                                EvidenceSource::Hook,
                                None,
                            ));
                            if recent_commands.len() >= 5 {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Open observations (in-flight tools)
        if let Ok(mut stmt) = conn.prepare(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
               AND o.end_ns IS NULL AND o.type IN ('tool', 'agent')
             ORDER BY o.start_ns DESC",
        ) {
            if let Ok(rows) = stmt.query_map(params![skey, lid], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            }) {
                for (name, input) in rows.flatten() {
                    let clue = if let Some(cmd) = extract_command(&name, &input) {
                        format!("{name}: {}", snippet(&cmd, 30))
                    } else if let Some(path) = extract_target_file(&name, &input) {
                        format!("{name}: {path}")
                    } else {
                        name
                    };
                    db_active_tools.push(clue);
                }
            }
        }
    }

    // Active tools preview
    let mut all_active_tools = live_active_tools.to_vec();
    for tool in db_active_tools {
        if !all_active_tools.contains(&tool) {
            all_active_tools.push(tool);
        }
    }

    // Current activity clue
    let current_activity = if !all_active_tools.is_empty() {
        Evidence::observed(
            format!("Running: {}", all_active_tools.join(", ")),
            EvidenceSource::LiveProcess,
            None,
        )
    } else {
        match runtime_state {
            RuntimeState::Working => Evidence::derived("Working / Generating".into(), EvidenceSource::LiveProcess),
            RuntimeState::WaitingForUser => {
                Evidence::derived("Waiting for user response / approval".into(), EvidenceSource::LiveProcess)
            }
            RuntimeState::Idle => Evidence::derived("Idle".into(), EvidenceSource::LiveProcess),
            RuntimeState::Exited => Evidence::derived("Session exited".into(), EvidenceSource::StoreRollup),
            RuntimeState::Disconnected => Evidence::derived("Disconnected".into(), EvidenceSource::LiveProcess),
            RuntimeState::Unknown => Evidence::missing(EvidenceSource::LiveProcess, "State unknown"),
        }
    };

    Ok(SessionCard {
        session_key: session_key.map(|s| s.to_string()),
        launch_id: launch_id.map(|s| s.to_string()),
        provider: provider.map(|s| s.to_string()),
        cwd: cwd.to_path_buf(),
        runtime_state,
        task_outcome,
        initial_goal,
        current_activity,
        completed_turns,
        open_turns,
        total_tools,
        tool_counts,
        files_modified,
        recent_commands,
        last_assistant_output,
        duration_ms,
        last_active_ns,
        total_tokens,
        total_cost_usd,
        active_tools: all_active_tools,
    })
}
