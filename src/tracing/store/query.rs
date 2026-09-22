//! Read queries shared by the CLI and the TUI browser. Every function takes
//! a plain `Connection` so callers can use a read-only one.
//!
//! `trace_stats` and `session_stats` are aggregate views over the whole
//! `traces ⋈ observations` join, and SQLite has no push-down into an
//! aggregate subquery: `SELECT * FROM trace_stats WHERE session_key = ?1`
//! groups every observation in the store before the predicate is applied,
//! so it costs the size of the store rather than the size of the answer.
//! The browser runs that query on every cursor move in the Sessions pane,
//! so the hot queries here (`list_sessions`, `list_traces`, `find_trace`)
//! rebuild the views' columns with their filter applied to `sessions` or
//! `traces` first, leaving only the surviving rows to be joined and
//! grouped. The views themselves stay for ad-hoc SQL and the colder
//! callers.
//!
//! The column lists must stay in step with the views in `schema.rs`; the
//! tests at the foot of this file compare the queries against the views
//! row by row, so a change to one that skips the other fails there.

use rusqlite::{Connection, OptionalExtension, Row, params};

/// Everything `trace_stats` adds to `traces`, over `t` (a trace) left
/// joined to `o` (its observations) and grouped by `t.rid`.
const TRACE_AGGREGATES: &str = "\
       datetime(t.start_ns / 1000000000, 'unixepoch', 'localtime') AS started_at,
       (MAX(COALESCE(t.end_ns, t.start_ns), COALESCE(MAX(COALESCE(o.end_ns, o.start_ns)), t.start_ns), t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
       COUNT(o.rid) AS observation_count,
       COALESCE(SUM(o.type = 'generation'), 0) AS generation_count,
       COALESCE(SUM(o.type IN ('tool','agent')), 0) AS tool_count,
       COALESCE(SUM(o.level = 'ERROR'), 0) AS error_count,
       COALESCE(SUM(o.end_ns IS NULL), 0) AS open_count,
       SUM(o.input_tokens) AS input_tokens,
       SUM(o.output_tokens) AS output_tokens,
       SUM(o.cache_read_tokens) AS cache_read_tokens,
       SUM(o.cache_write_tokens) AS cache_write_tokens,
       SUM(o.total_tokens) AS total_tokens,
       SUM(o.total_cost_usd) AS total_cost_usd,
       COALESCE(SUM(o.type = 'generation' AND o.usage_details IS NOT NULL AND o.total_cost_usd IS NULL), 0) AS unpriced_generations,
       GROUP_CONCAT(DISTINCT o.model) AS models,
       COALESCE(SUM(o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''), 0)
         - COUNT(DISTINCT CASE WHEN o.type = 'tool' AND trim(COALESCE(o.input, '')) <> '' THEN o.name || char(0) || o.input END) AS retries,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined";

/// `trace_stats` over the traces `where_clause` keeps, with `tail` (an
/// `ORDER BY`, a `LIMIT`) appended.
fn trace_stats_sql(where_clause: &str, tail: &str) -> String {
    format!(
        "SELECT t.*, {TRACE_AGGREGATES}
         FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
         WHERE {where_clause}
         GROUP BY t.rid {tail}"
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionStat {
    pub key: String,
    pub provider: String,
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub project_slug: Option<String>,
    pub transcript_path: Option<String>,
    pub first_seen_ns: i64,
    pub last_seen_ns: i64,
    pub turn_count: i64,
    pub open_turns: i64,
    pub duration_ms: Option<i64>,
    pub observation_count: i64,
    pub tool_count: i64,
    pub error_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub reported_cost_usd: Option<f64>,
    pub unpriced_generations: i64,
}

fn session_from_row(r: &Row) -> rusqlite::Result<SessionStat> {
    Ok(SessionStat {
        key: r.get("key")?,
        provider: r.get("provider")?,
        session_id: r.get("session_id")?,
        title: r.get("title")?,
        cwd: r.get("cwd")?,
        project_slug: r.get("project_slug")?,
        transcript_path: r.get("transcript_path")?,
        first_seen_ns: r.get("first_seen_ns")?,
        last_seen_ns: r.get("last_seen_ns")?,
        turn_count: r.get("turn_count")?,
        open_turns: r.get("open_turns")?,
        duration_ms: r.get("duration_ms")?,
        observation_count: r.get("observation_count")?,
        tool_count: r.get("tool_count")?,
        error_count: r.get("error_count")?,
        input_tokens: r.get("input_tokens")?,
        output_tokens: r.get("output_tokens")?,
        cache_read_tokens: r.get("cache_read_tokens")?,
        cache_write_tokens: r.get("cache_write_tokens")?,
        total_tokens: r.get("total_tokens")?,
        total_cost_usd: r.get("total_cost_usd")?,
        reported_cost_usd: r.get("reported_cost_usd")?,
        unpriced_generations: r.get("unpriced_generations")?,
    })
}

#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub project_slug: Option<String>,
    pub since_ns: Option<i64>,
    pub limit: usize,
}

pub fn list_sessions(
    conn: &Connection,
    filter: &SessionFilter,
) -> rusqlite::Result<Vec<SessionStat>> {
    // `picked` narrows to the listed page off `sessions_last_seen` /
    // `sessions_project` first, so the aggregation below touches only the
    // turns of the sessions actually shown.
    let mut stmt = conn.prepare(
        "WITH picked AS (
             SELECT * FROM sessions
             WHERE (?1 IS NULL OR project_slug = ?1) AND (?2 IS NULL OR last_seen_ns >= ?2)
             ORDER BY last_seen_ns DESC LIMIT ?3
         ),
         turns AS (
             SELECT t.rid                                   AS rid,
                    t.session_key                           AS session_key,
                    t.status                                AS status,
                    t.start_ns                              AS start_ns,
                    t.end_ns                                AS end_ns,
                    COUNT(o.rid)                            AS observation_count,
                    COALESCE(SUM(o.type IN ('tool','agent')), 0) AS tool_count,
                    COALESCE(SUM(o.level = 'ERROR'), 0)     AS error_count,
                    SUM(o.input_tokens)                     AS input_tokens,
                    SUM(o.output_tokens)                    AS output_tokens,
                    SUM(o.cache_read_tokens)                AS cache_read_tokens,
                    SUM(o.cache_write_tokens)               AS cache_write_tokens,
                    SUM(o.total_tokens)                     AS total_tokens,
                    SUM(o.total_cost_usd)                   AS total_cost_usd,
                    COALESCE(SUM(o.type = 'generation' AND o.usage_details IS NOT NULL
                                 AND o.total_cost_usd IS NULL), 0) AS unpriced_generations
             FROM traces t
             JOIN picked p ON p.key = t.session_key
             LEFT JOIN observations o ON o.trace_id = t.id
             GROUP BY t.rid
         )
         SELECT s.*,
                datetime(s.last_seen_ns / 1000000000, 'unixepoch', 'localtime') AS last_seen_at,
                COUNT(ts.rid)                             AS turn_count,
                COALESCE(SUM(ts.status = 'open'), 0)      AS open_turns,
                MIN(ts.start_ns)                          AS first_turn_ns,
                MAX(COALESCE(ts.end_ns, ts.start_ns))     AS last_turn_ns,
                (MAX(COALESCE(ts.end_ns, ts.start_ns)) - MIN(ts.start_ns)) / 1000000 AS duration_ms,
                COALESCE(SUM(ts.observation_count), 0)    AS observation_count,
                COALESCE(SUM(ts.tool_count), 0)           AS tool_count,
                COALESCE(SUM(ts.error_count), 0)          AS error_count,
                SUM(ts.input_tokens)                      AS input_tokens,
                SUM(ts.output_tokens)                     AS output_tokens,
                SUM(ts.cache_read_tokens)                 AS cache_read_tokens,
                SUM(ts.cache_write_tokens)                AS cache_write_tokens,
                SUM(ts.total_tokens)                      AS total_tokens,
                SUM(ts.total_cost_usd)                    AS total_cost_usd,
                COALESCE(SUM(ts.unpriced_generations), 0) AS unpriced_generations,
                (SELECT MAX(reported_cost_usd) FROM launches l WHERE l.session_key = s.key) AS reported_cost_usd
         FROM picked s LEFT JOIN turns ts ON ts.session_key = s.key
         GROUP BY s.key
         ORDER BY s.last_seen_ns DESC",
    )?;
    let rows = stmt.query_map(
        params![
            filter.project_slug,
            filter.since_ns,
            filter.limit.max(1) as i64
        ],
        session_from_row,
    )?;
    rows.collect()
}

/// The loop or workflow run each traced session belongs to, read from the
/// launch rows' metadata (`loop_id`, `workflow_run_id`, written by
/// `tracing::start_session`). The browser groups its Sessions pane by it,
/// so a workflow's steps and a loop's runs stop arriving as unrelated
/// rows. Sessions with no parent are simply absent from the map.
///
/// Newest launch wins: a session resumed under a different parent is
/// listed under the one it last ran for.
pub fn session_groups(
    conn: &Connection,
    limit: usize,
) -> rusqlite::Result<std::collections::HashMap<String, crate::tree::GroupRef>> {
    use crate::tree::{GroupKind, GroupRef};
    let mut stmt = conn.prepare(
        "SELECT session_key,
                json_extract(metadata, '$.loop_id')         AS loop_id,
                json_extract(metadata, '$.loop_run_id')     AS loop_run_id,
                json_extract(metadata, '$.loop_pattern')    AS loop_pattern,
                json_extract(metadata, '$.loop_level')      AS loop_level,
                json_extract(metadata, '$.workflow_run_id') AS wf_run,
                json_extract(metadata, '$.workflow')        AS wf_name,
                json_extract(metadata, '$.workflow_step')   AS wf_step
         FROM launches
         WHERE session_key IS NOT NULL
           AND (json_extract(metadata, '$.loop_id') IS NOT NULL
                OR json_extract(metadata, '$.workflow_run_id') IS NOT NULL)
         ORDER BY started_ns DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit.max(1) as i64], |r| {
        let key: String = r.get("session_key")?;
        let loop_id: Option<String> = r.get("loop_id")?;
        let group = match loop_id {
            Some(loop_id) => {
                let run: Option<String> = r.get("loop_run_id")?;
                let level: Option<String> = r.get("loop_level")?;
                GroupRef {
                    kind: GroupKind::Loop,
                    id: loop_id.clone(),
                    title: loop_id,
                    detail: r
                        .get::<_, Option<String>>("loop_pattern")?
                        .unwrap_or_default(),
                    label: match (run, level) {
                        (Some(run), Some(level)) => {
                            format!("{} {level}", run.chars().take(8).collect::<String>())
                        }
                        (Some(run), None) => run.chars().take(8).collect(),
                        _ => "run".to_string(),
                    },
                }
            }
            None => {
                let run: String = r.get::<_, Option<String>>("wf_run")?.unwrap_or_default();
                GroupRef {
                    kind: GroupKind::Workflow,
                    detail: format!("#{}", run.chars().take(8).collect::<String>()),
                    id: run,
                    title: r.get::<_, Option<String>>("wf_name")?.unwrap_or_default(),
                    label: r.get::<_, Option<String>>("wf_step")?.unwrap_or_default(),
                }
            }
        };
        Ok((key, group))
    })?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (key, group) = row?;
        out.entry(key).or_insert(group);
    }
    Ok(out)
}

/// Exact key / session id, or a session-id prefix.
pub fn find_session(conn: &Connection, needle: &str) -> rusqlite::Result<Option<SessionStat>> {
    conn.query_row(
        "SELECT * FROM session_stats
         WHERE key = ?1 OR session_id = ?1 OR session_id LIKE ?1 || '%'
         ORDER BY (key = ?1 OR session_id = ?1) DESC, last_seen_ns DESC LIMIT 1",
        params![needle],
        session_from_row,
    )
    .optional()
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceStat {
    pub id: String,
    pub session_key: String,
    pub launch_id: Option<String>,
    pub ordinal: i64,
    pub name: String,
    pub status: String,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub latency_ms: i64,
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    pub skills: String,
    pub reported_duration_ms: Option<i64>,
    pub session_cost_usd: Option<f64>,
    pub closed_by: Option<String>,
    pub observation_count: i64,
    pub generation_count: i64,
    pub tool_count: i64,
    pub error_count: i64,
    pub open_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub unpriced_generations: i64,
    pub models: Option<String>,
    /// The turn's own metadata JSON (compaction, interruption, hook facts).
    pub metadata: String,
    /// Tool calls repeated with identical input inside the turn.
    pub retries: i64,
    /// Tool calls the user declined.
    pub declined: i64,
}

fn trace_from_row(r: &Row) -> rusqlite::Result<TraceStat> {
    Ok(TraceStat {
        id: r.get("id")?,
        session_key: r.get("session_key")?,
        launch_id: r.get("launch_id")?,
        ordinal: r.get("ordinal")?,
        name: r.get("name")?,
        status: r.get("status")?,
        start_ns: r.get("start_ns")?,
        end_ns: r.get("end_ns")?,
        latency_ms: r.get("latency_ms")?,
        input: r.get("input")?,
        output: r.get("output")?,
        thinking: r.get("thinking")?,
        skills: r.get("skills")?,
        reported_duration_ms: r.get("reported_duration_ms")?,
        session_cost_usd: r.get("session_cost_usd")?,
        closed_by: r.get("closed_by")?,
        observation_count: r.get("observation_count")?,
        generation_count: r.get("generation_count")?,
        tool_count: r.get("tool_count")?,
        error_count: r.get("error_count")?,
        open_count: r.get("open_count")?,
        input_tokens: r.get("input_tokens")?,
        output_tokens: r.get("output_tokens")?,
        cache_read_tokens: r.get("cache_read_tokens")?,
        cache_write_tokens: r.get("cache_write_tokens")?,
        total_tokens: r.get("total_tokens")?,
        total_cost_usd: r.get("total_cost_usd")?,
        unpriced_generations: r.get("unpriced_generations")?,
        models: r.get("models")?,
        metadata: r.get("metadata")?,
        retries: r.get("retries")?,
        declined: r.get("declined")?,
    })
}

pub fn list_traces(conn: &Connection, session_key: &str) -> rusqlite::Result<Vec<TraceStat>> {
    let mut stmt = conn.prepare(&trace_stats_sql(
        "t.session_key = ?1",
        "ORDER BY t.ordinal, t.start_ns",
    ))?;
    let rows = stmt.query_map(params![session_key], trace_from_row)?;
    rows.collect()
}

pub fn find_trace(conn: &Connection, needle: &str) -> rusqlite::Result<Option<TraceStat>> {
    conn.query_row(
        &trace_stats_sql(
            "t.id = ?1 OR t.id LIKE ?1 || '%'",
            "ORDER BY (t.id = ?1) DESC LIMIT 1",
        ),
        params![needle],
        trace_from_row,
    )
    .optional()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationView {
    pub id: String,
    pub trace_id: String,
    pub parent_id: Option<String>,
    /// Nesting depth in the tree returned by `list_observations` (0 for a
    /// top-level row).
    pub depth: usize,
    pub obs_type: String,
    pub name: String,
    pub kind: Option<String>,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub level: String,
    pub status_message: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    pub usage: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub tool_id: Option<String>,
    pub tool_name: Option<String>,
    pub skill: Option<String>,
    pub mcp_server: Option<String>,
    pub path: Option<String>,
    pub metadata: String,
}

fn observation_from_row(r: &Row) -> rusqlite::Result<ObservationView> {
    Ok(ObservationView {
        id: r.get("id")?,
        trace_id: r.get("trace_id")?,
        parent_id: r.get("parent_id")?,
        depth: 0,
        obs_type: r.get("type")?,
        name: r.get("name")?,
        kind: r.get("kind")?,
        start_ns: r.get("start_ns")?,
        end_ns: r.get("end_ns")?,
        level: r.get("level")?,
        status_message: r.get("status_message")?,
        model: r.get("model")?,
        model_id: r.get("model_id")?,
        input: r.get("input")?,
        output: r.get("output")?,
        thinking: r.get("thinking")?,
        usage: r.get("usage")?,
        input_tokens: r.get("input_tokens")?,
        output_tokens: r.get("output_tokens")?,
        cache_read_tokens: r.get("cache_read_tokens")?,
        cache_write_tokens: r.get("cache_write_tokens")?,
        reasoning_tokens: r.get("reasoning_tokens")?,
        total_tokens: r.get("total_tokens")?,
        total_cost_usd: r.get("total_cost_usd")?,
        tool_id: r.get("tool_id")?,
        tool_name: r.get("tool_name")?,
        skill: r.get("skill")?,
        mcp_server: r.get("mcp_server")?,
        path: r.get("path")?,
        metadata: r.get("metadata")?,
    })
}

/// A turn's observations in chronological order. This is the canonical event
/// sequence for lists and timelines; hierarchy is derived only for tree views.
pub fn list_observations(
    conn: &Connection,
    trace_id: &str,
) -> rusqlite::Result<Vec<ObservationView>> {
    let mut stmt =
        conn.prepare("SELECT * FROM observations WHERE trace_id = ?1 ORDER BY start_ns, rid")?;
    let rows = stmt.query_map(params![trace_id], observation_from_row)?;
    rows.collect()
}

/// A turn's observations in parent-first depth-first order, for hierarchy
/// renderers only. Do not use this for a time-ordered event list.
pub fn list_observations_tree(
    conn: &Connection,
    trace_id: &str,
) -> rusqlite::Result<Vec<ObservationView>> {
    Ok(nest_observations(list_observations(conn, trace_id)?))
}

/// Reorders rows so children follow their parent, setting `depth`. A
/// parent that is not in the list (or a cycle) leaves the row at the top
/// level.
pub fn nest_observations(rows: Vec<ObservationView>) -> Vec<ObservationView> {
    use std::collections::{HashMap, HashSet};
    let ids: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        match r.parent_id.as_deref() {
            Some(p) if ids.contains(p) && p != r.id => children.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }
    let mut order: Vec<(usize, usize)> = Vec::with_capacity(rows.len());
    let mut placed = vec![false; rows.len()];
    fn walk(
        i: usize,
        depth: usize,
        rows: &[ObservationView],
        children: &HashMap<&str, Vec<usize>>,
        placed: &mut [bool],
        order: &mut Vec<(usize, usize)>,
    ) {
        if placed[i] {
            return;
        }
        placed[i] = true;
        order.push((i, depth));
        if let Some(kids) = children.get(rows[i].id.as_str()) {
            for &k in kids {
                walk(k, depth + 1, rows, children, placed, order);
            }
        }
    }
    for &i in &roots {
        walk(i, 0, &rows, &children, &mut placed, &mut order);
    }
    // rows only reachable through a cycle
    for i in 0..rows.len() {
        walk(i, 0, &rows, &children, &mut placed, &mut order);
    }
    let mut out: Vec<Option<ObservationView>> = rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|(i, depth)| {
            let mut r = out[i].take().expect("each row once");
            r.depth = depth;
            r
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub trace_id: String,
    pub observation_id: Option<String>,
    pub name: String,
    pub start_ns: i64,
    pub snippet: String,
}

/// FTS5 over turn and observation content. Invalid query syntax surfaces
/// as `Err`.
pub fn search(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<Vec<SearchHit>> {
    let limit = limit.max(1) as i64;
    let mut hits = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, t.start_ns, snippet(traces_fts, -1, '[', ']', '…', 12)
             FROM traces_fts f JOIN traces t ON t.rid = f.rowid
             WHERE traces_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok(SearchHit {
                trace_id: r.get(0)?,
                observation_id: None,
                name: r.get(1)?,
                start_ns: r.get(2)?,
                snippet: r.get(3)?,
            })
        })?;
        for row in rows {
            hits.push(row?);
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT o.trace_id, o.id, o.name, o.start_ns, snippet(observations_fts, -1, '[', ']', '…', 12)
             FROM observations_fts f JOIN observations o ON o.rid = f.rowid
             WHERE observations_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok(SearchHit {
                trace_id: r.get(0)?,
                observation_id: Some(r.get(1)?),
                name: r.get(2)?,
                start_ns: r.get(3)?,
                snippet: r.get(4)?,
            })
        })?;
        for row in rows {
            hits.push(row?);
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.start_ns));
    hits.truncate(limit as usize);
    Ok(hits)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub sessions: i64,
    pub launches: i64,
    pub traces: i64,
    pub observations: i64,
    pub open_traces: i64,
    pub live_runs: i64,
}

pub fn counts(conn: &Connection) -> rusqlite::Result<Counts> {
    let one = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };
    Ok(Counts {
        sessions: one("SELECT COUNT(*) FROM sessions")?,
        launches: one("SELECT COUNT(*) FROM launches")?,
        traces: one("SELECT COUNT(*) FROM traces")?,
        observations: one("SELECT COUNT(*) FROM observations")?,
        open_traces: one("SELECT COUNT(*) FROM traces WHERE status = 'open'")?,
        live_runs: one("SELECT COUNT(*) FROM runs WHERE ended_ns IS NULL")?,
    })
}

/// Distinct generation models with usage but no matched price, with counts.
pub fn unpriced_models(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT model, COUNT(*) FROM observations
         WHERE type = 'generation' AND usage IS NOT NULL AND model_id IS NULL AND model IS NOT NULL AND model NOT LIKE '<%'
         GROUP BY model ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// Live per-launch rollup for the TUI badges.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchStats {
    pub turns: i64,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub running_tool: Option<String>,
}

pub fn launch_stats(conn: &Connection, launch_id: &str) -> rusqlite::Result<LaunchStats> {
    let (turns, total_tokens, cost_usd) = conn.query_row(
        "SELECT COUNT(*), SUM(total_tokens), SUM(total_cost_usd) FROM trace_stats WHERE launch_id = ?1",
        params![launch_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<f64>>(2)?)),
    )?;
    let running_tool = conn
        .query_row(
            "SELECT o.name FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE t.launch_id = ?1 AND o.end_ns IS NULL AND o.type IN ('tool','agent')
             ORDER BY o.start_ns DESC LIMIT 1",
            params![launch_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(LaunchStats {
        turns,
        total_tokens,
        cost_usd,
        running_tool,
    })
}

/// Distinct project slugs known to the store (browser scope toggle).
pub fn session_project_slugs(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT project_slug FROM sessions WHERE project_slug IS NOT NULL")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod nest_tests {
    use super::*;

    fn view(id: &str, parent: Option<&str>, start: i64) -> ObservationView {
        ObservationView {
            id: id.into(),
            trace_id: "t".into(),
            parent_id: parent.map(str::to_string),
            depth: 0,
            obs_type: "tool".into(),
            name: id.into(),
            kind: None,
            start_ns: start,
            end_ns: None,
            level: "DEFAULT".into(),
            status_message: None,
            model: None,
            model_id: None,
            input: None,
            output: None,
            thinking: None,
            usage: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            metadata: "{}".into(),
        }
    }

    #[test]
    fn children_follow_their_parent_with_depth() {
        // start-time order: gen, task, agent(child of task), grep(child of agent), later
        let rows = vec![
            view("gen", None, 1),
            view("grep", Some("agent"), 2),
            view("task", None, 3),
            view("agent", Some("task"), 4),
            view("later", None, 5),
            view("orphan", Some("missing"), 6),
            view("loop", Some("loop"), 7),
        ];
        let nested = nest_observations(rows);
        let order: Vec<(&str, usize)> = nested.iter().map(|r| (r.id.as_str(), r.depth)).collect();
        assert_eq!(
            order,
            vec![
                ("gen", 0),
                ("task", 0),
                ("agent", 1),
                ("grep", 2),
                ("later", 0),
                ("orphan", 0),
                ("loop", 0),
            ]
        );
    }
}

/// A turn's prompt with the skills it loaded, for trigger matching.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRow {
    pub trace_id: String,
    pub input: String,
    pub skills: Vec<String>,
}

/// The newest `limit` prompts that were stored (metadata mode keeps none).
pub fn prompt_rows(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<PromptRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, input, skills FROM traces
         WHERE input IS NOT NULL AND input != ''
         ORDER BY start_ns DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| {
        let skills: Option<String> = r.get(2)?;
        Ok(PromptRow {
            trace_id: r.get(0)?,
            input: r.get(1)?,
            skills: skills
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default(),
        })
    })?;
    rows.collect()
}

/// Turns that loaded a skill, newest first.
pub fn traces_with_skill(
    conn: &Connection,
    skill: &str,
    limit: usize,
) -> rusqlite::Result<Vec<TraceStat>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM trace_stats
         WHERE EXISTS (SELECT 1 FROM json_each(trace_stats.skills) j WHERE j.value = ?1)
         ORDER BY start_ns DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![skill, limit as i64], trace_from_row)?;
    rows.collect()
}

/// Every tool name the store has seen a provider call.
pub fn tool_names(conn: &Connection, provider: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT o.name FROM observations o
         JOIN traces t ON t.id = o.trace_id
         JOIN sessions s ON s.key = t.session_key
         WHERE s.provider = ?1 AND o.type = 'tool' AND o.name NOT LIKE 'skill: %'
         ORDER BY o.name",
    )?;
    let rows = stmt.query_map(params![provider], |r| r.get(0))?;
    rows.collect()
}

/// One row of the `skill_stats` view: what a skill did across the store.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillStat {
    pub skill: String,
    pub turns_loaded: i64,
    pub generations: i64,
    pub tools: i64,
    pub tokens: Option<i64>,
    pub cost: Option<f64>,
    /// Turns where the skill was loaded and nothing was attributed to it.
    pub turns_unused: i64,
    pub first_ns: i64,
    pub last_ns: i64,
}

pub fn skill_stats(conn: &Connection) -> rusqlite::Result<Vec<SkillStat>> {
    let mut stmt = conn.prepare(
        "SELECT skill, turns_loaded, generations, tools, tokens, cost, turns_unused, first_ns, last_ns
         FROM skill_stats ORDER BY turns_loaded DESC, skill",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SkillStat {
            skill: r.get(0)?,
            turns_loaded: r.get(1)?,
            generations: r.get(2)?,
            tools: r.get(3)?,
            tokens: r.get(4)?,
            cost: r.get(5)?,
            turns_unused: r.get(6)?,
            first_ns: r.get(7)?,
            last_ns: r.get(8)?,
        })
    })?;
    rows.collect()
}

/// One agent type across the store. `p90_ms` is computed here from the
/// per-invocation durations, since the view carries mean and max only.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentStat {
    pub agent_type: String,
    pub invocations: i64,
    pub mean_ms: f64,
    pub p90_ms: i64,
    pub max_ms: i64,
    pub tokens: i64,
    pub cost: f64,
    /// Invocations where the agent row or any child failed.
    pub failures: i64,
}

pub fn agent_stats(conn: &Connection) -> rusqlite::Result<Vec<AgentStat>> {
    let mut stmt = conn.prepare(
        "SELECT agent_type, invocations, mean_ms, max_ms, tokens, cost, failures
         FROM agent_stats ORDER BY invocations DESC, agent_type",
    )?;
    let mut stats: Vec<AgentStat> = stmt
        .query_map([], |r| {
            Ok(AgentStat {
                agent_type: r.get(0)?,
                invocations: r.get(1)?,
                mean_ms: r.get::<_, f64>(2)?,
                p90_ms: 0,
                max_ms: r.get(3)?,
                tokens: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                cost: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                failures: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut durations = conn.prepare(
        "SELECT COALESCE(json_extract(metadata, '$.agent_type'), name) AS agent_type,
                (COALESCE(end_ns, start_ns) - start_ns) / 1000000 AS dur_ms
         FROM observations WHERE type = 'agent' ORDER BY agent_type, dur_ms",
    )?;
    let mut by_type: std::collections::HashMap<String, Vec<i64>> = std::collections::HashMap::new();
    for row in durations.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (t, d) = row?;
        by_type.entry(t).or_default().push(d);
    }
    for stat in &mut stats {
        if let Some(d) = by_type.get(&stat.agent_type) {
            // nearest-rank p90 over the sorted durations
            let rank = ((d.len() as f64) * 0.9).ceil() as usize;
            stat.p90_ms = d[rank.clamp(1, d.len()) - 1];
        }
    }
    Ok(stats)
}

/// One launch that ran a skill: the launch row joined to its turn count,
/// priced cost, and whether the owning run is still alive.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillLaunch {
    pub id: String,
    pub provider: String,
    pub profile: String,
    pub cwd: String,
    pub started_ns: i64,
    pub ended_ns: Option<i64>,
    pub termination: Option<String>,
    pub exit_code: Option<i64>,
    pub session_key: Option<String>,
    /// Matched through `metadata.skill_id`; `false` means the row predates
    /// that key and was matched by its session name.
    pub by_id: bool,
    pub turns: i64,
    pub total_cost_usd: Option<f64>,
    /// The launch has not ended and its run is still heartbeating.
    pub live: bool,
}

/// Launches of `skill_id`, newest first. Rows without `metadata.skill_id`
/// (captured before it was recorded) match when their profile name equals
/// `session_name`, the `<name> (<harness>)` the skill launcher uses.
/// `provider` narrows to one harness; `None` lists every harness.
pub fn skill_launches(
    conn: &Connection,
    skill_id: &str,
    session_name: &str,
    provider: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<SkillLaunch>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.provider, l.profile, l.cwd, l.started_ns, l.ended_ns, l.termination, l.exit_code,
                l.session_key,
                json_extract(l.metadata, '$.skill_id') IS NOT NULL AS by_id,
                (SELECT COUNT(*) FROM traces t WHERE t.launch_id = l.id) AS turns,
                (SELECT SUM(ts.total_cost_usd) FROM trace_stats ts WHERE ts.launch_id = l.id) AS total_cost_usd,
                (l.ended_ns IS NULL AND EXISTS (SELECT 1 FROM runs r WHERE r.id = l.run_id AND r.ended_ns IS NULL)) AS live
         FROM launches l
         WHERE (json_extract(l.metadata, '$.skill_id') = ?1
                OR (json_extract(l.metadata, '$.skill_id') IS NULL AND l.profile = ?2))
           AND (?3 IS NULL OR l.provider = ?3)
         ORDER BY l.started_ns DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![skill_id, session_name, provider, limit.max(1) as i64],
        |r| {
            Ok(SkillLaunch {
                id: r.get(0)?,
                provider: r.get(1)?,
                profile: r.get(2)?,
                cwd: r.get(3)?,
                started_ns: r.get(4)?,
                ended_ns: r.get(5)?,
                termination: r.get(6)?,
                exit_code: r.get(7)?,
                session_key: r.get(8)?,
                by_id: r.get::<_, i64>(9)? != 0,
                turns: r.get(10)?,
                total_cost_usd: r.get(11)?,
                live: r.get::<_, i64>(12)? != 0,
            })
        },
    )?;
    rows.collect()
}

/// A turn that loaded a skill, with whether any observation in it was
/// attributed to that skill.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillTurn {
    pub stat: TraceStat,
    pub attributed: bool,
}

/// Like [`traces_with_skill`], newest first, plus the attribution flag.
pub fn traces_with_skill_detail(
    conn: &Connection,
    skill: &str,
    limit: usize,
) -> rusqlite::Result<Vec<SkillTurn>> {
    let mut stmt = conn.prepare(
        "SELECT *, EXISTS (SELECT 1 FROM observations o WHERE o.trace_id = trace_stats.id AND o.skill = ?1) AS attributed
         FROM trace_stats
         WHERE EXISTS (SELECT 1 FROM json_each(trace_stats.skills) j WHERE j.value = ?1)
         ORDER BY start_ns DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![skill, limit as i64], |r| {
        Ok(SkillTurn {
            stat: trace_from_row(r)?,
            attributed: r.get::<_, i64>("attributed")? != 0,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::store::schema;

    /// A store with `sessions` sessions, each with two turns of three
    /// observations, varied enough to exercise every aggregate the views
    /// build: errors, open rows, retried tools, declined tools, unpriced
    /// generations, several models.
    fn seeded(sessions: usize) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        for (i, sql) in schema::MIGRATIONS.iter().enumerate() {
            conn.execute_batch(&format!(
                "BEGIN;\n{sql}\nPRAGMA user_version = {};\nCOMMIT;",
                i + 1
            ))
            .unwrap();
        }
        for s in 0..sessions {
            let key = format!("claude:s{s}");
            let ns = 1_000_000_000i64 * (s as i64 + 1);
            conn.execute(
                "INSERT INTO sessions (key, provider, session_id, cwd, project_slug,
                     first_seen_ns, last_seen_ns)
                 VALUES (?1, 'claude', ?2, '/proj', '-proj', ?3, ?4)",
                params![key, format!("s{s}"), ns, ns + 500],
            )
            .unwrap();
            for t in 0..2i64 {
                let tid = format!("t{s}-{t}");
                let start = ns + t * 100;
                // one turn of each status, so `open_turns` is exercised
                let (status, end): (&str, Option<i64>) = if t == 0 {
                    ("closed", Some(start + 90))
                } else {
                    ("open", None)
                };
                conn.execute(
                    "INSERT INTO traces (id, session_key, ordinal, name, status, start_ns,
                         end_ns, input, skills)
                     VALUES (?1, ?2, ?3, 'turn', ?4, ?5, ?6, 'hello', '[]')",
                    params![tid, key, t, status, start, end],
                )
                .unwrap();
                for o in 0..3i64 {
                    let (otype, level, tokens, cost, model, status_msg, input) = match o {
                        0 => (
                            "generation",
                            "DEFAULT",
                            Some(100),
                            Some(0.01),
                            Some("opus"),
                            None,
                            None,
                        ),
                        1 => (
                            "tool",
                            "ERROR",
                            None,
                            None,
                            None,
                            Some("declined by the user"),
                            Some("{}"),
                        ),
                        // repeated input: feeds `retries`
                        _ => ("tool", "DEFAULT", None, None, None, None, Some("{}")),
                    };
                    let oend = if o == 2 && t == 1 {
                        None
                    } else {
                        Some(start + o + 1)
                    };
                    conn.execute(
                        "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns,
                             level, status_message, model, input, total_tokens, total_cost_usd,
                             usage_details)
                         VALUES (?1, ?2, ?3, 'op', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            format!("o{s}-{t}-{o}"),
                            tid,
                            otype,
                            start + o,
                            oend,
                            level,
                            status_msg,
                            model,
                            input,
                            tokens,
                            cost,
                            // an unpriced generation: usage without a cost
                            if o == 0 && t == 1 { Some("{}") } else { None },
                        ],
                    )
                    .unwrap();
                }
            }
        }
        conn
    }

    /// The view, read through the same mapper, for whatever the filtered
    /// query is expected to reproduce.
    fn traces_from_view(conn: &Connection, session_key: &str) -> Vec<TraceStat> {
        let mut stmt = conn
            .prepare("SELECT * FROM trace_stats WHERE session_key = ?1 ORDER BY ordinal, start_ns")
            .unwrap();
        stmt.query_map(params![session_key], trace_from_row)
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn sessions_from_view(conn: &Connection, limit: i64) -> Vec<SessionStat> {
        let mut stmt = conn
            .prepare("SELECT * FROM session_stats ORDER BY last_seen_ns DESC LIMIT ?1")
            .unwrap();
        stmt.query_map(params![limit], session_from_row)
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn list_traces_matches_the_trace_stats_view() {
        let conn = seeded(3);
        for s in 0..3 {
            let key = format!("claude:s{s}");
            assert_eq!(
                list_traces(&conn, &key).unwrap(),
                traces_from_view(&conn, &key),
                "filtered query and view disagree for {key}"
            );
        }
        assert_eq!(list_traces(&conn, "claude:nope").unwrap(), Vec::new());
    }

    #[test]
    fn find_trace_matches_the_trace_stats_view() {
        let conn = seeded(2);
        let full = find_trace(&conn, "t1-0").unwrap().unwrap();
        assert_eq!(full, traces_from_view(&conn, "claude:s1")[0]);
        // a prefix resolves to the same turn
        assert_eq!(find_trace(&conn, "t1-").unwrap().unwrap(), full);
        assert_eq!(find_trace(&conn, "missing").unwrap(), None);
    }

    #[test]
    fn list_sessions_matches_the_session_stats_view() {
        let conn = seeded(4);
        let filter = SessionFilter {
            project_slug: None,
            since_ns: None,
            limit: 500,
        };
        assert_eq!(
            list_sessions(&conn, &filter).unwrap(),
            sessions_from_view(&conn, 500)
        );
        // the page the limit keeps is the same page, aggregates included
        let paged = SessionFilter {
            limit: 2,
            ..filter.clone()
        };
        assert_eq!(
            list_sessions(&conn, &paged).unwrap(),
            sessions_from_view(&conn, 2)
        );
        // a session with no turns still lists, with empty aggregates
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns)
             VALUES ('claude:empty', 'claude', 'empty', 9000000000, 9000000000)",
            [],
        )
        .unwrap();
        let rows = list_sessions(&conn, &filter).unwrap();
        assert_eq!(rows, sessions_from_view(&conn, 500));
        assert_eq!(rows[0].key, "claude:empty");
        assert_eq!(rows[0].turn_count, 0);
    }

    #[test]
    fn list_sessions_honours_the_project_filter() {
        let conn = seeded(2);
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, project_slug, first_seen_ns, last_seen_ns)
             VALUES ('claude:other', 'claude', 'other', '-other', 9000000000, 9000000000)",
            [],
        )
        .unwrap();
        let rows = list_sessions(
            &conn,
            &SessionFilter {
                project_slug: Some("-proj".into()),
                since_ns: None,
                limit: 500,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| r.project_slug.as_deref() == Some("-proj"))
        );
    }

    /// The browser re-runs `list_traces` on every cursor move, so its cost
    /// has to follow the session, not the store: reading through
    /// `trace_stats` grouped every observation in the store first.
    #[test]
    fn list_traces_reads_only_the_session_it_asks_for() {
        let conn = seeded(1);
        let plan: Vec<String> = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                trace_stats_sql("t.session_key = ?1", "ORDER BY t.ordinal, t.start_ns")
            ))
            .unwrap()
            .query_map(params!["claude:s0"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let plan = plan.join("\n");
        assert!(
            plan.contains("SEARCH t USING INDEX") && !plan.contains("SCAN t"),
            "the turns of one session come off a session_key index, never a \
             full pass over traces:\n{plan}"
        );
    }
}
