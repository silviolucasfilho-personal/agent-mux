//! Read queries shared by the CLI and the TUI browser. Every function takes
//! a plain `Connection` so callers can use a read-only one.

use rusqlite::{Connection, OptionalExtension, Row, params};

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
    let mut stmt = conn.prepare(
        "SELECT * FROM session_stats
         WHERE (?1 IS NULL OR project_slug = ?1) AND (?2 IS NULL OR last_seen_ns >= ?2)
         ORDER BY last_seen_ns DESC LIMIT ?3",
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
    let mut stmt = conn
        .prepare("SELECT * FROM trace_stats WHERE session_key = ?1 ORDER BY ordinal, start_ns")?;
    let rows = stmt.query_map(params![session_key], trace_from_row)?;
    rows.collect()
}

pub fn find_trace(conn: &Connection, needle: &str) -> rusqlite::Result<Option<TraceStat>> {
    conn.query_row(
        "SELECT * FROM trace_stats WHERE id = ?1 OR id LIKE ?1 || '%' ORDER BY (id = ?1) DESC LIMIT 1",
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
    pub is_error: bool,
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
        is_error: r.get::<_, i64>("is_error")? != 0,
        metadata: r.get("metadata")?,
    })
}

/// A turn's observations in tree order: parents first, each followed by
/// its children (recursively), siblings by start time.
pub fn list_observations(
    conn: &Connection,
    trace_id: &str,
) -> rusqlite::Result<Vec<ObservationView>> {
    let mut stmt =
        conn.prepare("SELECT * FROM observations WHERE trace_id = ?1 ORDER BY start_ns, rid")?;
    let rows = stmt.query_map(params![trace_id], observation_from_row)?;
    Ok(nest_observations(rows.collect::<Result<Vec<_>, _>>()?))
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
            is_error: false,
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
