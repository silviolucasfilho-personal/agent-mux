//! `/api/trace/`: the trace store through `TraceService`.
//!
//! `POST /api/trace/{tool}` with a JSON body of the tool's arguments, where
//! `tool` is one of `briefing`, `list_sessions`, `get_session`,
//! `get_timeline`, `search_traces`, `analyze_skills`, `compare_runs`,
//! `get_health`; the answer is the service envelope, the same JSON the MCP
//! server returns as `structuredContent`. An unknown tool is a 404 JSON
//! error; a service error keeps its `[CODE] message` text.
//!
//! The session viewer reads two more routes, straight from the store
//! (`open_ro`) and under the same scope `get_session` applies:
//! `GET /api/session/{key}/transcript` (`Transcript`: the header facts,
//! the turns with their observations in full, the files touched) and
//! `GET /api/export/session/{key}.html`, the same transcript as one
//! self-contained HTML file (inline CSS, no script).

use super::{ApiError, ApiResult, WebState};
use crate::tracing::analysis::Request;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path as FsPath;
use std::sync::Arc;

/// The tools `/api/trace/{tool}` accepts, without their `agent_mux_` prefix.
pub const TOOLS: &[&str] = &[
    "briefing",
    "list_sessions",
    "get_session",
    "get_timeline",
    "search_traces",
    "analyze_skills",
    "compare_runs",
    "get_health",
];

/// The MCP tool name of a web tool name.
pub fn tool_name(tool: &str) -> Option<String> {
    if !TOOLS.contains(&tool) {
        return None;
    }
    Some(match tool {
        "briefing" => "agent_mux_get_briefing".to_string(),
        other => format!("agent_mux_{other}"),
    })
}

pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/trace/{tool}", post(call))
        .route("/api/session/{key}/transcript", get(transcript))
        .route("/api/export/session/{file}", get(export_session))
}

/// The JSON body, if any; an empty body counts as `{}`.
async fn body_json(body: axum::body::Bytes) -> Result<Value, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

async fn call(
    State(state): State<Arc<WebState>>,
    Path(tool): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    let name =
        tool_name(&tool).ok_or_else(|| ApiError::not_found(format!("unknown tool: {tool}")))?;
    let arguments = body_json(body).await?;
    let request = Request::from_tool_call(&name, arguments)?;
    let envelope = tokio::task::spawn_blocking(move || state.trace.execute(request))
        .await
        .map_err(|e| ApiError::internal(format!("trace query panicked: {e}")))??;
    let value = serde_json::to_value(&envelope).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(axum::Json(value))
}

// ---- session transcript ------------------------------------------------------

/// Turns `/api/session/{key}/transcript` answers by default, and at most.
pub const TRANSCRIPT_PAGE: usize = 200;
pub const TRANSCRIPT_PAGE_MAX: usize = 2000;
/// Characters kept of one text field in the JSON transcript. A longer text
/// is cut and named in the item's `truncated` list, with its full length in
/// `lengths`. The export keeps every turn and every character.
pub const TRANSCRIPT_MAX_CHARS: usize = 32_000;
/// Turns the export reads at a time: it writes each page and drops it, so
/// only the HTML and one page are in memory.
pub const EXPORT_PAGE: usize = 200;

/// The header and turns of one session (or one launch of it).
#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub ok: bool,
    pub session_key: String,
    pub launch_id: Option<String>,
    pub title: Option<String>,
    pub provider: Option<String>,
    pub cwd: Option<String>,
    /// `full`, `metadata` (prompts, outputs and tool inputs were not
    /// stored), `mixed` (launches differ) or `unknown`.
    pub content_mode: String,
    pub launches: Vec<LaunchInfo>,
    pub models: Vec<String>,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub totals: Totals,
    pub files: Vec<FileTouch>,
    pub total_turns: usize,
    /// The index (0-based, in start order) of `turns[0]`.
    pub from_turn: usize,
    /// The index of the turn the `turn` query named, when it is one of
    /// this session's (a deep link loads up to it).
    pub turn_index: Option<usize>,
    pub turns: Vec<Turn>,
}

/// A launch of the session and what it belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct LaunchInfo {
    pub id: String,
    pub provider: String,
    pub profile: String,
    pub content_mode: String,
    pub started_ns: i64,
    pub ended_ns: Option<i64>,
    pub termination: Option<String>,
    pub exit_code: Option<i64>,
    pub cost_usd: Option<f64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    /// `{id, role, task, task_key}` from `swarm_*` metadata.
    pub swarm: Option<Value>,
    /// `{run_id, loop_id, pattern}` from `loop_*` metadata.
    pub loop_run: Option<Value>,
    /// `{run_id, name, step, phase}` from `workflow*` metadata.
    pub workflow: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub generations: i64,
    pub tool_calls: i64,
    pub errors: i64,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
}

/// One path the session's observations name (`observations.path`).
#[derive(Debug, Clone, Serialize)]
pub struct FileTouch {
    pub path: String,
    pub count: i64,
    pub tools: Vec<String>,
    pub errors: i64,
    pub first_ns: i64,
    pub last_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub index: usize,
    pub id: String,
    pub ordinal: i64,
    pub launch_id: Option<String>,
    pub status: String,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    pub skills: Vec<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub errors: i64,
    pub truncated: Vec<&'static str>,
    pub lengths: BTreeMap<&'static str, usize>,
    pub observations: Vec<Obs>,
}

/// One observation; `parent_id` nests tool calls under their generation
/// and a sub-agent's turns under its invocation.
#[derive(Debug, Clone, Serialize)]
pub struct Obs {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(rename = "type")]
    pub obs_type: String,
    pub kind: Option<String>,
    pub name: String,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error: bool,
    pub level: String,
    pub status_message: Option<String>,
    pub model: Option<String>,
    /// Not kept for a generation: its input is the whole context.
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub tool_name: Option<String>,
    pub skill: Option<String>,
    pub mcp_server: Option<String>,
    pub path: Option<String>,
    pub truncated: Vec<&'static str>,
    pub lengths: BTreeMap<&'static str, usize>,
}

/// Options of one transcript read.
#[derive(Debug, Clone, Copy)]
pub struct TranscriptOpts<'a> {
    pub from_turn: usize,
    /// A trace id whose index to report in `turn_index`.
    pub turn: Option<&'a str>,
    pub limit: usize,
    pub max_chars: usize,
}

/// `text` cut to `max` characters; records the cut in `truncated`/`lengths`.
fn clip(
    text: Option<String>,
    field: &'static str,
    max: usize,
    truncated: &mut Vec<&'static str>,
    lengths: &mut BTreeMap<&'static str, usize>,
) -> Option<String> {
    let text = text?;
    let n = text.chars().count();
    if n <= max {
        return Some(text);
    }
    truncated.push(field);
    lengths.insert(field, n);
    Some(text.chars().take(max).collect())
}

fn meta_str(meta: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    meta.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The session's cwd, checked against `scope`, and the launch filter
/// checked against the session's traces, as `get_session` does.
pub fn check_session(
    conn: &Connection,
    scope: &crate::tracing::analysis::Scope,
    key: &str,
    launch_id: Option<&str>,
) -> Result<(), ApiError> {
    let db = |e: rusqlite::Error| ApiError::internal(e.to_string());
    let cwd: Option<Option<String>> = conn
        .query_row("SELECT cwd FROM sessions WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db)?;
    let cwd = match cwd {
        Some(c) => c,
        None => conn
            .query_row(
                "SELECT l.cwd FROM traces t LEFT JOIN launches l ON l.id = t.launch_id
                 WHERE t.session_key = ?1 LIMIT 1",
                [key],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(db)?
            .ok_or_else(|| ApiError::not_found(format!("session '{key}' not found")))?,
    };
    let cwd = cwd.unwrap_or_default();
    if !scope.allows_session_cwd(FsPath::new(&cwd)) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("session '{key}' is outside authorized workspace scope"),
        ));
    }
    if let Some(lid) = launch_id {
        let belongs = conn
            .query_row(
                "SELECT 1 FROM traces WHERE session_key = ?1 AND launch_id = ?2 LIMIT 1",
                params![key, lid],
                |_| Ok(()),
            )
            .optional()
            .map_err(db)?
            .is_some();
        if !belongs {
            return Err(ApiError::bad_request(format!(
                "launch_id '{lid}' does not belong to session '{key}'"
            )));
        }
    }
    Ok(())
}

/// Reads the transcript of `key` (only `launch_id`'s turns when given).
/// The caller checks the scope first (`check_session`).
pub fn load_transcript(
    conn: &Connection,
    key: &str,
    launch_id: Option<&str>,
    opts: TranscriptOpts<'_>,
) -> rusqlite::Result<Transcript> {
    let lid = launch_id.unwrap_or("");
    let session: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT provider, cwd, title FROM sessions WHERE key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (mut provider, mut cwd, title) = session.unwrap_or((None, None, None));

    // launches: the session's own rows and the ones its traces name
    let mut launches = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, provider, profile, content_mode, started_ns, ended_ns, termination,
                    exit_code, reported_cost_usd, reported_lines_added, reported_lines_removed,
                    metadata, cwd
             FROM launches
             WHERE (session_key = ?1
                    OR id IN (SELECT DISTINCT launch_id FROM traces
                              WHERE session_key = ?1 AND launch_id IS NOT NULL))
               AND (?2 = '' OR id = ?2)
             ORDER BY started_ns ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![key, lid], |r| {
            let meta: String = r.get(11)?;
            let meta: serde_json::Map<String, Value> =
                serde_json::from_str(&meta).unwrap_or_default();
            let swarm = meta_str(&meta, "swarm_id").map(|id| {
                serde_json::json!({
                    "id": id,
                    "role": meta_str(&meta, "swarm_role"),
                    "task": meta_str(&meta, "swarm_task"),
                    "task_key": meta_str(&meta, "swarm_task_key"),
                })
            });
            let loop_run = meta_str(&meta, "loop_run_id").map(|id| {
                serde_json::json!({
                    "run_id": id,
                    "loop_id": meta_str(&meta, "loop_id"),
                    "pattern": meta_str(&meta, "loop_pattern"),
                })
            });
            let workflow = meta_str(&meta, "workflow_run_id").map(|id| {
                serde_json::json!({
                    "run_id": id,
                    "name": meta_str(&meta, "workflow"),
                    "step": meta_str(&meta, "workflow_step"),
                    "phase": meta_str(&meta, "workflow_phase"),
                })
            });
            Ok((
                LaunchInfo {
                    id: r.get(0)?,
                    provider: r.get(1)?,
                    profile: r.get(2)?,
                    content_mode: r.get(3)?,
                    started_ns: r.get(4)?,
                    ended_ns: r.get(5)?,
                    termination: r.get(6)?,
                    exit_code: r.get(7)?,
                    cost_usd: r.get(8)?,
                    lines_added: r.get(9)?,
                    lines_removed: r.get(10)?,
                    swarm,
                    loop_run,
                    workflow,
                },
                r.get::<_, String>(12)?,
            ))
        })?;
        for row in rows {
            let (l, lcwd) = row?;
            if provider.is_none() {
                provider = Some(l.provider.clone());
            }
            if cwd.is_none() {
                cwd = Some(lcwd);
            }
            launches.push(l);
        }
    }

    let total_turns: usize = conn.query_row(
        "SELECT COUNT(*) FROM traces WHERE session_key = ?1 AND (?2 = '' OR launch_id = ?2)",
        params![key, lid],
        |r| r.get::<_, i64>(0),
    )? as usize;
    let (started_ns, ended_ns, any_text): (Option<i64>, Option<i64>, i64) = conn.query_row(
        "SELECT MIN(start_ns), MAX(COALESCE(end_ns, start_ns)),
                COALESCE(SUM(input IS NOT NULL OR output IS NOT NULL), 0)
         FROM traces WHERE session_key = ?1 AND (?2 = '' OR launch_id = ?2)",
        params![key, lid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;

    let content_mode = {
        let modes: BTreeSet<&str> = launches.iter().map(|l| l.content_mode.as_str()).collect();
        match modes.len() {
            0 if any_text > 0 => "full",
            0 if total_turns > 0 => "metadata",
            0 => "unknown",
            1 => modes.into_iter().next().unwrap_or("unknown"),
            _ => "mixed",
        }
        .to_string()
    };

    let mut totals: Totals = conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN o.type = 'generation' THEN o.input_tokens END), 0),
                COALESCE(SUM(CASE WHEN o.type = 'generation' THEN o.output_tokens END), 0),
                COALESCE(SUM(CASE WHEN o.type = 'generation' THEN o.cache_read_tokens END), 0),
                COALESCE(SUM(CASE WHEN o.type = 'generation' THEN o.cache_write_tokens END), 0),
                COALESCE(SUM(CASE WHEN o.type = 'generation' THEN o.total_tokens END), 0),
                COALESCE(SUM(COALESCE(o.total_cost_usd, json_extract(o.provided_cost, '$.total'))), 0.0),
                COALESCE(SUM(o.type = 'generation'), 0),
                COALESCE(SUM(o.type IN ('tool','agent') AND COALESCE(o.kind, '') <> 'subagent_turn'), 0),
                COALESCE(SUM(o.level = 'ERROR'), 0)
         FROM observations o JOIN traces t ON t.id = o.trace_id
         WHERE t.session_key = ?1 AND (?2 = '' OR t.launch_id = ?2)",
        params![key, lid],
        |r| {
            Ok(Totals {
                input_tokens: r.get(0)?,
                output_tokens: r.get(1)?,
                cache_read_tokens: r.get(2)?,
                cache_write_tokens: r.get(3)?,
                total_tokens: r.get(4)?,
                cost_usd: r.get(5)?,
                generations: r.get(6)?,
                tool_calls: r.get(7)?,
                errors: r.get(8)?,
                lines_added: None,
                lines_removed: None,
            })
        },
    )?;
    let sum_opt = |f: fn(&LaunchInfo) -> Option<i64>| {
        launches
            .iter()
            .filter_map(f)
            .fold(None, |a: Option<i64>, v| Some(a.unwrap_or(0) + v))
    };
    totals.lines_added = sum_opt(|l| l.lines_added);
    totals.lines_removed = sum_opt(|l| l.lines_removed);
    // a launch's reported cost when the observations carry none
    if totals.cost_usd == 0.0 {
        let reported: f64 = launches.iter().filter_map(|l| l.cost_usd).sum();
        totals.cost_usd = reported;
    }

    let mut models = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT o.model, COUNT(*) FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE t.session_key = ?1 AND (?2 = '' OR t.launch_id = ?2)
               AND o.type = 'generation' AND o.model IS NOT NULL AND o.model <> ''
             GROUP BY o.model ORDER BY COUNT(*) DESC, o.model",
        )?;
        for m in stmt.query_map(params![key, lid], |r| r.get::<_, String>(0))? {
            models.push(m?);
        }
    }

    let mut files = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT o.path, COUNT(*), GROUP_CONCAT(DISTINCT COALESCE(o.tool_name, o.name)),
                    COALESCE(SUM(o.level = 'ERROR'), 0), MIN(o.start_ns), MAX(o.start_ns)
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE t.session_key = ?1 AND (?2 = '' OR t.launch_id = ?2)
               AND o.path IS NOT NULL AND o.path <> ''
             GROUP BY o.path ORDER BY MAX(o.start_ns) DESC, o.path",
        )?;
        let rows = stmt.query_map(params![key, lid], |r| {
            let tools: Option<String> = r.get(2)?;
            let mut tools: Vec<String> = tools
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            tools.sort();
            Ok(FileTouch {
                path: r.get(0)?,
                count: r.get(1)?,
                tools,
                errors: r.get(3)?,
                first_ns: r.get(4)?,
                last_ns: r.get(5)?,
            })
        })?;
        for f in rows {
            files.push(f?);
        }
    }

    let from_turn = opts.from_turn.min(total_turns);
    let turn_index = match opts.turn {
        Some(tid) => conn
            .query_row(
                "SELECT COUNT(*) FROM traces t, traces x
                 WHERE x.id = ?3 AND x.session_key = ?1 AND (?2 = '' OR x.launch_id = ?2)
                   AND t.session_key = ?1 AND (?2 = '' OR t.launch_id = ?2)
                   AND (t.start_ns < x.start_ns OR (t.start_ns = x.start_ns AND t.id < x.id))
                 GROUP BY x.id",
                params![key, lid, tid],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|n| n as usize),
        None => None,
    };
    let turns = load_turns(conn, key, lid, from_turn, opts.limit, opts.max_chars)?;

    Ok(Transcript {
        ok: true,
        session_key: key.to_string(),
        launch_id: launch_id.map(str::to_string),
        title,
        provider,
        cwd,
        content_mode,
        launches,
        models,
        started_ns,
        ended_ns,
        duration_ms: match (started_ns, ended_ns) {
            (Some(s), Some(e)) => Some((e - s).max(0) / 1_000_000),
            _ => None,
        },
        totals,
        files,
        total_turns,
        from_turn,
        turn_index,
        turns,
    })
}

/// Turns `from_turn..from_turn+limit` of `key` (only launch `lid`'s when
/// not empty), each text cut to `max_chars`, with their observations.
pub fn load_turns(
    conn: &Connection,
    key: &str,
    lid: &str,
    from_turn: usize,
    limit: usize,
    max_chars: usize,
) -> rusqlite::Result<Vec<Turn>> {
    let mut turns = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, ordinal, launch_id, status, start_ns, end_ns, input, output, thinking,
                    skills, reported_duration_ms
             FROM traces WHERE session_key = ?1 AND (?2 = '' OR launch_id = ?2)
             ORDER BY start_ns ASC, id ASC LIMIT ?3 OFFSET ?4",
        )?;
        let rows = stmt.query_map(
            params![
                key,
                lid,
                i64::try_from(limit).unwrap_or(-1),
                from_turn as i64
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                ))
            },
        )?;
        for (i, row) in rows.enumerate() {
            let (
                id,
                ordinal,
                launch,
                status,
                start_ns,
                end_ns,
                input,
                output,
                thinking,
                skills,
                rep,
            ) = row?;
            let mut truncated = Vec::new();
            let mut lengths = BTreeMap::new();
            let max = max_chars;
            let input = clip(input, "input", max, &mut truncated, &mut lengths);
            let output = clip(output, "output", max, &mut truncated, &mut lengths);
            let thinking = clip(thinking, "thinking", max, &mut truncated, &mut lengths);
            let skills: Vec<String> = serde_json::from_str(&skills).unwrap_or_default();
            turns.push(Turn {
                index: from_turn + i,
                ordinal,
                launch_id: launch,
                status,
                start_ns,
                end_ns,
                duration_ms: rep.or(end_ns.map(|e| (e - start_ns).max(0) / 1_000_000)),
                input,
                output,
                thinking,
                skills,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
                errors: 0,
                truncated,
                lengths,
                observations: Vec::new(),
                id,
            });
        }
    }

    let mut obs_stmt = conn.prepare(
        "SELECT id, parent_id, type, kind, name, start_ns, end_ns, level, status_message, model,
                input, output, thinking, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, total_tokens,
                -- a provider's own cost when the store left the price empty
                COALESCE(total_cost_usd, json_extract(provided_cost, '$.total')),
                tool_name, skill, mcp_server, path
         FROM observations WHERE trace_id = ?1 ORDER BY start_ns ASC, id ASC",
    )?;
    for turn in &mut turns {
        let rows = obs_stmt.query_map([turn.id.as_str()], |r| {
            let obs_type: String = r.get(2)?;
            let level: String = r.get(7)?;
            let start_ns: i64 = r.get(5)?;
            let end_ns: Option<i64> = r.get(6)?;
            Ok(Obs {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                kind: r.get(3)?,
                name: r.get(4)?,
                start_ns,
                end_ns,
                duration_ms: end_ns.map(|e| (e - start_ns).max(0) / 1_000_000),
                error: level == "ERROR",
                level,
                status_message: r.get(8)?,
                model: r.get(9)?,
                input: if obs_type == "generation" {
                    None
                } else {
                    r.get(10)?
                },
                output: r.get(11)?,
                thinking: r.get(12)?,
                input_tokens: r.get(13)?,
                output_tokens: r.get(14)?,
                cache_read_tokens: r.get(15)?,
                cache_write_tokens: r.get(16)?,
                total_tokens: r.get(17)?,
                cost_usd: r.get(18)?,
                tool_name: r.get(19)?,
                skill: r.get(20)?,
                mcp_server: r.get(21)?,
                path: r.get(22)?,
                truncated: Vec::new(),
                lengths: BTreeMap::new(),
                obs_type,
            })
        })?;
        for o in rows {
            let mut o = o?;
            let max = max_chars;
            o.input = clip(
                o.input.take(),
                "input",
                max,
                &mut o.truncated,
                &mut o.lengths,
            );
            o.output = clip(
                o.output.take(),
                "output",
                max,
                &mut o.truncated,
                &mut o.lengths,
            );
            o.thinking = clip(
                o.thinking.take(),
                "thinking",
                max,
                &mut o.truncated,
                &mut o.lengths,
            );
            if o.obs_type == "generation" {
                turn.input_tokens += o.input_tokens.unwrap_or(0);
                turn.output_tokens += o.output_tokens.unwrap_or(0);
                turn.cache_read_tokens += o.cache_read_tokens.unwrap_or(0);
                turn.cache_write_tokens += o.cache_write_tokens.unwrap_or(0);
                turn.total_tokens += o.total_tokens.unwrap_or(0);
            }
            turn.cost_usd += o.cost_usd.unwrap_or(0.0);
            if o.error {
                turn.errors += 1;
            }
            turn.observations.push(o);
        }
    }
    Ok(turns)
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TranscriptQuery {
    launch_id: Option<String>,
    from_turn: Option<usize>,
    limit: Option<usize>,
    turn: Option<String>,
    download: Option<String>,
}

type QueryResult =
    Result<axum::extract::Query<TranscriptQuery>, axum::extract::rejection::QueryRejection>;

/// Opens the store, checks the scope and reads the transcript, on a
/// blocking thread.
async fn read_transcript(
    state: Arc<WebState>,
    key: String,
    launch_id: Option<String>,
    turn: Option<String>,
    opts: TranscriptOpts<'static>,
) -> Result<Transcript, ApiError> {
    tokio::task::spawn_blocking(move || {
        let opts = TranscriptOpts {
            turn: turn.as_deref(),
            ..opts
        };
        let conn = crate::tracing::store::open_ro(&state.db)
            .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let lid = launch_id.as_deref().filter(|s| !s.is_empty());
        check_session(&conn, &state.trace.config().scope, &key, lid)?;
        load_transcript(&conn, &key, lid, opts).map_err(|e| ApiError::internal(e.to_string()))
    })
    .await
    .map_err(|e| ApiError::internal(format!("transcript read panicked: {e}")))?
}

/// `GET /api/session/{key}/transcript?launch_id=&from_turn=&limit=&turn=`:
/// the `Transcript` of a session, turns `from_turn..from_turn+limit`;
/// `turn` (a trace id) is answered with its index in `turn_index`.
async fn transcript(
    State(state): State<Arc<WebState>>,
    Path(key): Path<String>,
    query: QueryResult,
) -> ApiResult<Transcript> {
    let axum::extract::Query(q) = query.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let opts = TranscriptOpts {
        from_turn: q.from_turn.unwrap_or(0),
        turn: None,
        limit: q
            .limit
            .unwrap_or(TRANSCRIPT_PAGE)
            .clamp(1, TRANSCRIPT_PAGE_MAX),
        max_chars: TRANSCRIPT_MAX_CHARS,
    };
    let t = read_transcript(state, key, q.launch_id, q.turn, opts).await?;
    Ok(axum::Json(t))
}

/// `GET /api/export/session/{key}.html[?launch_id=&download=1]`: the whole
/// transcript, every turn and every stored character, as one self-contained
/// HTML file; `download=1` asks the browser to save it.
async fn export_session(
    State(state): State<Arc<WebState>>,
    Path(file): Path<String>,
    query: QueryResult,
) -> Result<Response, ApiError> {
    let axum::extract::Query(q) = query.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let key = file
        .strip_suffix(".html")
        .filter(|k| !k.is_empty())
        .ok_or_else(|| ApiError::not_found("export a session as <key>.html"))?
        .to_string();
    let launch_id = q.launch_id;
    let (session_key, html) = tokio::task::spawn_blocking(move || {
        let conn = crate::tracing::store::open_ro(&state.db)
            .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let lid = launch_id.as_deref().filter(|s| !s.is_empty());
        check_session(&conn, &state.trace.config().scope, &key, lid)?;
        let html = export_session_html(&conn, &key, lid, EXPORT_PAGE)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok::<_, ApiError>((key, html))
    })
    .await
    .map_err(|e| ApiError::internal(format!("export panicked: {e}")))??;
    let disposition = format!(
        "{}; filename=\"{}.html\"",
        if q.download.as_deref().is_some_and(|d| d != "0") {
            "attachment"
        } else {
            "inline"
        },
        file_stem(&session_key)
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'".to_string(),
            ),
        ],
        html,
    )
        .into_response())
}

/// A file name for a session key: `[A-Za-z0-9._-]`, the rest `_`.
pub fn file_stem(key: &str) -> String {
    let s: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("session-{s}")
}

/// HTML-escapes text for an element body or a quoted attribute.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn fmt_ns(ns: Option<i64>) -> String {
    let Some(ns) = ns else {
        return "–".into();
    };
    time::OffsetDateTime::from_unix_timestamp_nanos(ns as i128)
        .ok()
        .and_then(|d| {
            d.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(|s| {
            s.split('.')
                .next()
                .unwrap_or(&s)
                .trim_end_matches('Z')
                .replace('T', " ")
                + " UTC"
        })
        .unwrap_or_else(|| "–".into())
}

fn fmt_ms(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "–".into();
    };
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{:02}s", ms / 60_000, (ms / 1000) % 60)
    } else {
        format!("{}h{:02}m", ms / 3_600_000, (ms / 60_000) % 60)
    }
}

fn fmt_tokens(n: i64) -> String {
    let a = n.abs() as f64;
    if a >= 1e6 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn fmt_cost(usd: f64) -> String {
    if usd == 0.0 {
        "$0".into()
    } else if usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

const EXPORT_CSS: &str = r#"
:root{--bg:#fff;--fg:#1d2330;--muted:#667085;--line:#e4e7ec;--panel:#f7f8fa;--accent:#3b5bdb;--err:#c92a2a;--errbg:#fff0f0;--skill:#7048e8;--code:#f1f3f5}
@media (prefers-color-scheme:dark){:root{--bg:#14161b;--fg:#e6e8ec;--muted:#98a2b3;--line:#2b2f38;--panel:#1b1e25;--accent:#7b93ff;--err:#ff8787;--errbg:#2a1517;--skill:#b197fc;--code:#20242c}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
main{max-width:980px;margin:0 auto;padding:24px 16px 64px}h1{font-size:20px;margin:0 0 4px}h2{font-size:16px;margin:28px 0 8px}
.muted{color:var(--muted)}.facts{display:grid;grid-template-columns:max-content 1fr;gap:2px 16px;margin:12px 0;font-size:13px}.facts dt{color:var(--muted)}.facts dd{margin:0;word-break:break-all}
.warn{background:var(--errbg);border:1px solid var(--err);color:var(--err);padding:8px 12px;border-radius:6px;margin:12px 0}
.turn{border:1px solid var(--line);border-radius:8px;margin:16px 0;overflow:hidden}.turn>header{background:var(--panel);padding:6px 12px;font-size:12px;color:var(--muted);display:flex;gap:12px;flex-wrap:wrap}
.turn>header b{color:var(--fg)}.msg{padding:8px 12px;border-top:1px solid var(--line)}.who{font-size:11px;text-transform:uppercase;letter-spacing:.04em;color:var(--muted);margin-bottom:2px}
pre{margin:0;white-space:pre-wrap;word-break:break-word;font:12.5px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code);padding:8px;border-radius:6px;max-height:none}
.text{white-space:pre-wrap;word-break:break-word}details{margin:4px 0}summary{cursor:pointer;font-size:13px}details.tool,details.thinking{border:1px solid var(--line);border-radius:6px;padding:4px 8px}
details.error{border-color:var(--err);background:var(--errbg)}.badge{display:inline-block;font-size:11px;padding:0 6px;border-radius:9px;border:1px solid var(--line);margin-left:6px}
.badge.err{color:var(--err);border-color:var(--err)}.badge.skill{color:var(--skill);border-color:var(--skill)}.agent{border-left:3px solid var(--accent);margin:6px 0 6px 4px;padding:2px 0 2px 10px}
.cut{color:var(--muted);font-size:12px;font-style:italic}table{border-collapse:collapse;font-size:13px;width:100%}td,th{text-align:left;padding:3px 8px;border-bottom:1px solid var(--line)}
footer{margin-top:40px;font-size:12px;color:var(--muted)}
"#;

/// The transcript as one self-contained HTML document: inline CSS, no
/// script, every stored text escaped.
pub fn export_html(t: &Transcript) -> String {
    let mut o = String::with_capacity(16 * 1024);
    export_head(&mut o, t);
    if t.turns.len() < t.total_turns {
        let _ = write!(
            o,
            "<p class=\"cut\">The first {} of {} turns.</p>",
            t.turns.len(),
            t.total_turns
        );
    }
    for turn in &t.turns {
        export_turn(&mut o, turn);
    }
    export_foot(&mut o);
    o
}

/// The export of `key` (only `launch_id`'s turns when given): every turn,
/// read `page` turns at a time, and every stored character. The caller
/// checks the scope first (`check_session`).
pub fn export_session_html(
    conn: &Connection,
    key: &str,
    launch_id: Option<&str>,
    page: usize,
) -> rusqlite::Result<String> {
    let page = page.max(1);
    let head = load_transcript(
        conn,
        key,
        launch_id,
        TranscriptOpts {
            from_turn: 0,
            turn: None,
            limit: 0,
            max_chars: usize::MAX,
        },
    )?;
    let mut o = String::with_capacity(16 * 1024);
    export_head(&mut o, &head);
    let lid = launch_id.unwrap_or("");
    let mut from = 0;
    loop {
        let turns = load_turns(conn, key, lid, from, page, usize::MAX)?;
        for turn in &turns {
            export_turn(&mut o, turn);
        }
        if turns.len() < page {
            break;
        }
        from += turns.len();
    }
    export_foot(&mut o);
    Ok(o)
}

/// The document head, the header facts and the files table.
fn export_head(o: &mut String, t: &Transcript) {
    let e = escape_html;
    let title = t.title.clone().unwrap_or_else(|| t.session_key.clone());
    let _ = write!(
        o,
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{} · agent-mux session</title><style>{}</style></head><body><main>",
        e(&title),
        EXPORT_CSS
    );
    let _ = write!(
        o,
        "<h1>{}</h1><div class=\"muted\">{}</div>",
        e(&title),
        e(&t.session_key)
    );
    let mut facts: Vec<(&str, String)> = vec![
        ("harness", t.provider.clone().unwrap_or_else(|| "–".into())),
        (
            "models",
            if t.models.is_empty() {
                "–".into()
            } else {
                t.models.join(", ")
            },
        ),
        ("workspace", t.cwd.clone().unwrap_or_else(|| "–".into())),
        ("started", fmt_ns(t.started_ns)),
        ("duration", fmt_ms(t.duration_ms)),
        ("turns", t.total_turns.to_string()),
        (
            "tokens",
            format!(
                "{} (input {}, output {}, cache read {}, cache write {})",
                fmt_tokens(t.totals.total_tokens),
                fmt_tokens(t.totals.input_tokens),
                fmt_tokens(t.totals.output_tokens),
                fmt_tokens(t.totals.cache_read_tokens),
                fmt_tokens(t.totals.cache_write_tokens)
            ),
        ),
        ("cost", fmt_cost(t.totals.cost_usd)),
        ("content", t.content_mode.clone()),
    ];
    if t.totals.lines_added.is_some() || t.totals.lines_removed.is_some() {
        facts.push((
            "lines",
            format!(
                "+{} −{}",
                t.totals.lines_added.unwrap_or(0),
                t.totals.lines_removed.unwrap_or(0)
            ),
        ));
    }
    for l in &t.launches {
        let mut s = format!("{} ({}, {})", l.id, l.provider, fmt_ns(Some(l.started_ns)));
        if let Some(sw) = &l.swarm {
            let _ = write!(s, " · swarm {}", sw["id"].as_str().unwrap_or(""));
        }
        if let Some(lr) = &l.loop_run {
            let _ = write!(s, " · loop run {}", lr["run_id"].as_str().unwrap_or(""));
        }
        if let Some(w) = &l.workflow {
            let _ = write!(s, " · workflow run {}", w["run_id"].as_str().unwrap_or(""));
        }
        facts.push(("launch", s));
    }
    o.push_str("<dl class=\"facts\">");
    for (k, v) in &facts {
        let _ = write!(o, "<dt>{}</dt><dd>{}</dd>", e(k), e(v));
    }
    o.push_str("</dl>");
    if t.content_mode == "metadata" {
        o.push_str(
            "<div class=\"warn\">Metadata only: this session was traced with \
             <code>content_mode = metadata</code>, so prompts, outputs and tool \
             inputs were not stored.</div>",
        );
    } else if t.content_mode == "mixed" {
        o.push_str(
            "<div class=\"warn\">Some launches were traced metadata-only: their \
             prompts, outputs and tool inputs were not stored.</div>",
        );
    }
    if !t.files.is_empty() {
        o.push_str(
            "<h2>Files touched</h2><table><tr><th>path</th><th>calls</th><th>tools</th></tr>",
        );
        for f in &t.files {
            let _ = write!(
                o,
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                e(&f.path),
                f.count,
                e(&f.tools.join(", "))
            );
        }
        o.push_str("</table>");
    }
    o.push_str("<h2>Transcript</h2>");
}

fn export_foot(o: &mut String) {
    let _ = writeln!(
        o,
        "<footer>Exported by agent-mux from the trace store · {}</footer></main></body></html>",
        fmt_ns(Some(crate::tracing::store::now_ns()))
    );
}

fn cut_note(o: &mut String, lengths: &BTreeMap<&'static str, usize>, field: &str) {
    if let Some(n) = lengths.get(field) {
        let _ = write!(
            o,
            "<div class=\"cut\">cut: {n} characters in the store</div>"
        );
    }
}

fn export_turn(o: &mut String, turn: &Turn) {
    let e = escape_html;
    let _ = write!(
        o,
        "<section class=\"turn\" id=\"turn-{}\"><header><b>turn {}</b><span>{}</span>\
         <span>{}</span><span>{} tokens</span><span>{}</span><span>{}</span>",
        turn.index + 1,
        turn.index + 1,
        e(&fmt_ns(Some(turn.start_ns))),
        e(&fmt_ms(turn.duration_ms)),
        fmt_tokens(turn.total_tokens),
        fmt_cost(turn.cost_usd),
        e(&turn.status)
    );
    for s in &turn.skills {
        let _ = write!(o, "<span class=\"badge skill\">skill {}</span>", e(s));
    }
    if turn.errors > 0 {
        let _ = write!(
            o,
            "<span class=\"badge err\">{} error(s)</span>",
            turn.errors
        );
    }
    o.push_str("</header>");
    if let Some(input) = &turn.input {
        let _ = write!(
            o,
            "<div class=\"msg user\"><div class=\"who\">user</div><div class=\"text\">{}</div>",
            e(input)
        );
        cut_note(o, &turn.lengths, "input");
        o.push_str("</div>");
    }
    if let Some(th) = &turn.thinking {
        let _ = write!(
            o,
            "<div class=\"msg\"><details class=\"thinking\"><summary>thinking</summary><div class=\"text\">{}</div></details></div>",
            e(th)
        );
    }
    // observations as a tree: generations are transparent, agents nest
    let ids: BTreeSet<&str> = turn.observations.iter().map(|x| x.id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<&Obs>> = HashMap::new();
    for x in &turn.observations {
        let parent = x.parent_id.as_deref().filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(x);
    }
    let gen_text = turn.observations.iter().any(|x| {
        x.obs_type == "generation" && x.output.as_deref().is_some_and(|s| !s.trim().is_empty())
    });
    if !turn.observations.is_empty() {
        o.push_str("<div class=\"msg\">");
        export_children(o, &children, None);
        o.push_str("</div>");
    }
    if let Some(out) = &turn.output
        && !gen_text
    {
        let _ = write!(
            o,
            "<div class=\"msg assistant\"><div class=\"who\">assistant</div><div class=\"text\">{}</div>",
            e(out)
        );
        cut_note(o, &turn.lengths, "output");
        o.push_str("</div>");
    }
    o.push_str("</section>");
}

fn export_children(
    o: &mut String,
    children: &HashMap<Option<&str>, Vec<&Obs>>,
    parent: Option<&str>,
) {
    let e = escape_html;
    let Some(list) = children.get(&parent) else {
        return;
    };
    for x in list {
        match x.obs_type.as_str() {
            "generation" => {
                if let Some(th) = x.thinking.as_deref().filter(|s| !s.trim().is_empty()) {
                    let _ = write!(
                        o,
                        "<details class=\"thinking\"><summary>thinking</summary><div class=\"text\">{}</div></details>",
                        e(th)
                    );
                }
                if let Some(out) = x.output.as_deref().filter(|s| !s.trim().is_empty()) {
                    let _ = write!(
                        o,
                        "<div class=\"assistant\"><div class=\"who\">assistant{}</div><div class=\"text\">{}</div>",
                        x.model
                            .as_deref()
                            .map(|m| format!(" · {}", e(m)))
                            .unwrap_or_default(),
                        e(out)
                    );
                    cut_note(o, &x.lengths, "output");
                    o.push_str("</div>");
                }
                export_children(o, children, Some(&x.id));
            }
            "agent" => {
                let _ = write!(
                    o,
                    "<div class=\"agent\"><div class=\"who\">{}{}</div>",
                    e(&x.name),
                    if x.error {
                        " <span class=\"badge err\">error</span>"
                    } else {
                        ""
                    }
                );
                if let Some(input) = x.input.as_deref().filter(|s| !s.trim().is_empty()) {
                    let _ = write!(
                        o,
                        "<details class=\"tool\"><summary>prompt</summary><pre>{}</pre></details>",
                        e(input)
                    );
                }
                export_children(o, children, Some(&x.id));
                if let Some(out) = x.output.as_deref().filter(|s| !s.trim().is_empty()) {
                    let _ = write!(
                        o,
                        "<details class=\"tool\"><summary>result</summary><pre>{}</pre></details>",
                        e(out)
                    );
                }
                o.push_str("</div>");
            }
            "event" if x.input.is_none() && x.output.is_none() => {
                let _ = write!(o, "<div class=\"cut\">{}</div>", e(&x.name));
            }
            _ => {
                let skill = x.kind.as_deref() == Some("skill_load") || x.skill.is_some();
                let _ = write!(
                    o,
                    "<details class=\"tool{}\"><summary>{}{}{}{} <span class=\"muted\">{}</span></summary>",
                    if x.error { " error" } else { "" },
                    e(&x.name),
                    if skill {
                        format!(
                            "<span class=\"badge skill\">skill {}</span>",
                            e(x.skill.as_deref().unwrap_or(""))
                        )
                    } else {
                        String::new()
                    },
                    if x.error {
                        "<span class=\"badge err\">error</span>"
                    } else {
                        ""
                    },
                    x.path
                        .as_deref()
                        .map(|p| format!(" <code>{}</code>", e(p)))
                        .unwrap_or_default(),
                    e(&fmt_ms(x.duration_ms))
                );
                if let Some(m) = x.status_message.as_deref().filter(|_| x.error) {
                    let _ = write!(o, "<div class=\"warn\">{}</div>", e(m));
                }
                if let Some(input) = &x.input {
                    let _ = write!(o, "<div class=\"who\">input</div><pre>{}</pre>", e(input));
                    cut_note(o, &x.lengths, "input");
                }
                if let Some(out) = &x.output {
                    let _ = write!(o, "<div class=\"who\">output</div><pre>{}</pre>", e(out));
                    cut_note(o, &x.lengths, "output");
                }
                o.push_str("</details>");
                export_children(o, children, Some(&x.id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_tool_names_map_to_mcp_tool_names() {
        assert_eq!(
            tool_name("briefing").as_deref(),
            Some("agent_mux_get_briefing")
        );
        assert_eq!(
            tool_name("get_health").as_deref(),
            Some("agent_mux_get_health")
        );
        assert_eq!(
            tool_name("list_sessions").as_deref(),
            Some("agent_mux_list_sessions")
        );
        assert_eq!(tool_name("delegate"), None);
        for t in TOOLS {
            let name = tool_name(t).unwrap();
            // every web tool is a service tool: the service may reject the
            // empty arguments, but never the name
            match Request::from_tool_call(&name, serde_json::json!({})) {
                Ok(_) => {}
                Err(e) => assert!(!e.to_string().contains("unknown tool"), "{t}: {e}"),
            }
        }
    }
}
