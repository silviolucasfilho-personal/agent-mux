//! Query engine for factual session briefings, recaps, and scope rollups.

use super::evidence::{extract_command, extract_target_file, snippet};
use super::model::{
    AnalysisError, Briefing, Evidence, EvidenceSource, LiveSession, RuntimeState, SessionCard,
    TaskOutcome, ToolCountSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::path::Path;

/// What the briefing should scan. A card costs about seven queries, so the
/// filters and the cap belong in SQL: building every card and then dropping
/// most of them is what made a briefing over a busy day miss its deadline.
#[derive(Debug, Clone, Default)]
pub struct BriefingScan {
    /// Only sessions of this provider.
    pub provider: Option<String>,
    /// Only this session key: one card, for a detail request.
    pub session_key: Option<String>,
    /// At most this many historical cards, most recent first. 0 is no cap.
    pub max_cards: usize,
}

impl BriefingScan {
    /// Every card in the window, as the briefing did before it was capped.
    pub fn all() -> BriefingScan {
        BriefingScan::default()
    }

    pub fn with_max(max_cards: usize) -> BriefingScan {
        BriefingScan {
            max_cards,
            ..BriefingScan::default()
        }
    }

    fn matches_live(&self, s: &LiveSession) -> bool {
        if let Some(p) = &self.provider
            && s.provider.as_deref() != Some(p.as_str())
        {
            return false;
        }
        if let Some(k) = &self.session_key
            && s.session_key.as_deref() != Some(k.as_str())
        {
            return false;
        }
        true
    }
}

/// The workspace rollup, read from `session_stats` in one query so it stays
/// exact however few cards the caller asked for.
#[derive(Debug, Clone, Default)]
struct Rollup {
    sessions: i64,
    turns: i64,
    tools: i64,
    tokens: Option<i64>,
    cost_usd: Option<f64>,
}

fn rollup(
    conn: &Connection,
    ws: &str,
    since_ns: i64,
    until_ns: i64,
    scan: &BriefingScan,
) -> rusqlite::Result<Rollup> {
    conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(turn_count), 0),
                COALESCE(SUM(tool_count), 0),
                SUM(total_tokens),
                SUM(total_cost_usd)
         FROM session_stats
         WHERE (last_seen_ns >= ?1 AND first_seen_ns < ?2)
           AND (cwd IS NULL OR cwd = '' OR cwd = ?3)
           AND (?4 IS NULL OR provider = ?4)
           AND (?5 IS NULL OR key = ?5)",
        params![since_ns, until_ns, ws, scan.provider, scan.session_key],
        |r| {
            Ok(Rollup {
                sessions: r.get(0)?,
                turns: r.get(1)?,
                tools: r.get(2)?,
                tokens: r.get(3)?,
                cost_usd: r.get(4)?,
            })
        },
    )
}

/// Generates an executive briefing across active and historical sessions within a time window.
///
/// Merges live sessions with historical sessions, deduplicating history already represented
/// by a live launch. The rollup totals come from one aggregate query, so they stay exact
/// whatever `scan.max_cards` caps the cards at; a cap that bites is reported in `warnings`.
pub fn briefing(
    conn: &Connection,
    workspace: &Path,
    since_ns: i64,
    until_ns: i64,
    live: &[LiveSession],
    scan: &BriefingScan,
) -> Result<Briefing, AnalysisError> {
    let mut cards = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_session_keys = HashSet::new();
    let mut seen_launch_ids = HashSet::new();
    let ws_str = workspace.to_string_lossy().to_string();

    // 1. Process Live Sessions first
    for s in live {
        if !s.cwd.as_os_str().is_empty() && s.cwd != workspace {
            // Out of workspace scope
            continue;
        }
        if !scan.matches_live(s) {
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
    let live_cards = cards.len();

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

    let mut totals = Rollup::default();
    if has_sessions_table {
        totals = rollup(conn, &ws_str, since_ns, until_ns, scan).unwrap_or_default();

        // The cap is pushed into SQL: the most recent sessions are what a
        // briefing reads. The key breaks ties, so "the most recent n" is the
        // same set on every build of the same store.
        let limit = if scan.max_cards == 0 {
            -1
        } else {
            scan.max_cards as i64
        };
        let mut stmt = conn.prepare(
            "SELECT key, provider, cwd, first_seen_ns, last_seen_ns
             FROM sessions
             WHERE (last_seen_ns >= ?1 AND first_seen_ns < ?2)
               AND (cwd IS NULL OR cwd = '' OR cwd = ?3)
               AND (?4 IS NULL OR provider = ?4)
               AND (?5 IS NULL OR key = ?5)
             ORDER BY last_seen_ns DESC, key DESC LIMIT ?6",
        )?;

        let rows = stmt.query_map(
            params![
                since_ns,
                until_ns,
                ws_str,
                scan.provider,
                scan.session_key,
                limit
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;

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

    // 3. Roll up workspace scope totals. A live session the store has not
    // seen yet is not in the aggregate, so its card is added to it.
    let mut total_sessions = totals.sessions.max(0) as usize;
    let mut total_turns = totals.turns;
    let mut total_tools = totals.tools;
    let mut total_tokens_sum: i64 = totals.tokens.unwrap_or(0);
    let mut has_tokens = totals.tokens.is_some();
    let mut total_cost_sum: f64 = totals.cost_usd.unwrap_or(0.0);
    let mut has_cost = totals.cost_usd.is_some();

    for card in cards.iter().take(live_cards) {
        let counted = card
            .session_key
            .as_deref()
            .is_some_and(|k| session_in_store(conn, k, since_ns, until_ns, &ws_str, scan));
        if counted {
            continue;
        }
        total_sessions += 1;
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
    if total_sessions < cards.len() {
        total_sessions = cards.len();
    }
    if scan.max_cards > 0 && total_sessions > cards.len() {
        warnings.push(format!(
            "{total_sessions} sessions in scope; cards built for the {} most recent",
            cards.len()
        ));
    }

    Ok(Briefing {
        cards,
        scope_workspace: workspace.to_path_buf(),
        since_ns,
        until_ns,
        total_sessions,
        total_turns,
        total_tools,
        total_tokens: if has_tokens {
            Some(total_tokens_sum)
        } else {
            None
        },
        total_cost_usd: if has_cost { Some(total_cost_sum) } else { None },
        warnings,
    })
}

/// Whether the aggregate already counted this live session.
fn session_in_store(
    conn: &Connection,
    key: &str,
    since_ns: i64,
    until_ns: i64,
    ws: &str,
    scan: &BriefingScan,
) -> bool {
    conn.query_row(
        "SELECT 1 FROM session_stats
         WHERE key = ?1
           AND (last_seen_ns >= ?2 AND first_seen_ns < ?3)
           AND (cwd IS NULL OR cwd = '' OR cwd = ?4)
           AND (?5 IS NULL OR provider = ?5)",
        params![key, since_ns, until_ns, ws, scan.provider],
        |_| Ok(true),
    )
    .optional()
    .unwrap_or(None)
    .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
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
    // Check if traces table exists
    let has_traces: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'traces'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    let mut initial_goal =
        Evidence::missing(EvidenceSource::Transcript, "No initial goal recorded");
    let mut completed_turns = 0;
    let mut open_turns = 0;
    let mut last_assistant_output = None;
    let mut last_active_ns = 0;
    let mut duration_ms = 0;
    let mut total_tokens = None;
    let mut total_cost_usd = None;

    let (filter_sql, t_filter_sql, filter_params): (String, String, Vec<String>) =
        match (session_key, launch_id) {
            (Some(sk), Some(lid)) if !sk.is_empty() && !lid.is_empty() => (
                "(session_key = ?1 OR launch_id = ?2)".into(),
                "(t.session_key = ?1 OR t.launch_id = ?2)".into(),
                vec![sk.to_string(), lid.to_string()],
            ),
            (Some(sk), _) if !sk.is_empty() => (
                "session_key = ?1".into(),
                "t.session_key = ?1".into(),
                vec![sk.to_string()],
            ),
            (_, Some(lid)) if !lid.is_empty() => (
                "launch_id = ?1".into(),
                "t.launch_id = ?1".into(),
                vec![lid.to_string()],
            ),
            _ => (String::new(), String::new(), vec![]),
        };

    if has_traces && !filter_sql.is_empty() {
        // Initial goal: first input
        let sql = format!(
            "SELECT input FROM traces
             WHERE {filter_sql}
               AND input IS NOT NULL AND trim(input) != ''
             ORDER BY start_ns ASC LIMIT 1"
        );
        if let Ok(Some(first_in)) = conn
            .query_row(
                &sql,
                rusqlite::params_from_iter(filter_params.iter()),
                |r| r.get::<_, String>(0),
            )
            .optional()
        {
            initial_goal =
                Evidence::observed(snippet(&first_in, 200), EvidenceSource::Transcript, None);
        }

        // Turns count
        let sql = format!("SELECT COUNT(*) FROM traces WHERE {filter_sql}");
        if let Ok(total) = conn.query_row(
            &sql,
            rusqlite::params_from_iter(filter_params.iter()),
            |r| r.get::<_, i64>(0),
        ) {
            completed_turns = total;
        }

        // Open turns count
        let sql = format!("SELECT COUNT(*) FROM traces WHERE {filter_sql} AND end_ns IS NULL");
        if let Ok(open) = conn.query_row(
            &sql,
            rusqlite::params_from_iter(filter_params.iter()),
            |r| r.get::<_, i64>(0),
        ) {
            open_turns = open;
            completed_turns = completed_turns.saturating_sub(open);
        }

        // Latest assistant output
        let sql = format!(
            "SELECT output FROM traces
             WHERE {filter_sql}
               AND output IS NOT NULL AND trim(output) != ''
             ORDER BY start_ns DESC LIMIT 1"
        );
        if let Ok(Some(last_out)) = conn
            .query_row(
                &sql,
                rusqlite::params_from_iter(filter_params.iter()),
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
            let sql = format!(
                "SELECT SUM(total_tokens), SUM(total_cost_usd), MIN(start_ns), MAX(COALESCE(end_ns, start_ns))
                 FROM trace_stats
                 WHERE {filter_sql}"
            );
            if let Ok(row) = conn.query_row(
                &sql,
                rusqlite::params_from_iter(filter_params.iter()),
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?,
                        r.get::<_, Option<f64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
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

    if has_obs && !t_filter_sql.is_empty() {
        // Tool count and breakdown
        let sql = format!(
            "SELECT o.name, COUNT(*)
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE {t_filter_sql}
               AND o.type = 'tool'
             GROUP BY o.name
             ORDER BY COUNT(*) DESC"
        );
        if let Ok(mut stmt) = conn.prepare(&sql)
            && let Ok(rows) =
                stmt.query_map(rusqlite::params_from_iter(filter_params.iter()), |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?
                            .unwrap_or_else(|| "unknown".into()),
                        r.get::<_, i64>(1)?,
                    ))
                })
        {
            for (name, count) in rows.flatten() {
                total_tools += count;
                tool_counts.push(ToolCountSummary { name, count });
            }
        }

        // Files modified
        let sql = format!(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE {t_filter_sql}
               AND o.type = 'tool'
             ORDER BY o.start_ns DESC LIMIT 50"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let mut seen_files = HashSet::new();
            if let Ok(rows) =
                stmt.query_map(rusqlite::params_from_iter(filter_params.iter()), |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })
            {
                for (name, input) in rows.flatten() {
                    if let Some(file_path) = extract_target_file(&name, &input)
                        && seen_files.insert(file_path.clone())
                    {
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

        // Recent commands
        let sql = format!(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE {t_filter_sql}
               AND o.type = 'tool'
             ORDER BY o.start_ns DESC LIMIT 50"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let mut seen_cmds = HashSet::new();
            if let Ok(rows) =
                stmt.query_map(rusqlite::params_from_iter(filter_params.iter()), |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })
            {
                for (name, input) in rows.flatten() {
                    if let Some(cmd) = extract_command(&name, &input)
                        && seen_cmds.insert(cmd.clone())
                    {
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

        // Open observations (in-flight tools)
        let sql = format!(
            "SELECT o.name, o.input
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE {t_filter_sql}
               AND o.end_ns IS NULL AND o.type IN ('tool', 'agent')
             ORDER BY o.start_ns DESC LIMIT 10"
        );
        if let Ok(mut stmt) = conn.prepare(&sql)
            && let Ok(rows) =
                stmt.query_map(rusqlite::params_from_iter(filter_params.iter()), |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })
        {
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
            RuntimeState::Working => {
                Evidence::derived("Working / Generating".into(), EvidenceSource::LiveProcess)
            }
            RuntimeState::WaitingForUser => Evidence::derived(
                "Waiting for user response / approval".into(),
                EvidenceSource::LiveProcess,
            ),
            RuntimeState::Idle => Evidence::derived("Idle".into(), EvidenceSource::LiveProcess),
            RuntimeState::Exited => {
                Evidence::derived("Session exited".into(), EvidenceSource::StoreRollup)
            }
            RuntimeState::Disconnected => {
                Evidence::derived("Disconnected".into(), EvidenceSource::LiveProcess)
            }
            RuntimeState::Unknown => {
                Evidence::missing(EvidenceSource::LiveProcess, "State unknown")
            }
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
