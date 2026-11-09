//! `/api/usage/`: token, cost and activity aggregates over the trace store
//! (`web/usage.js`: the Overview and Usage pages).
//!
//! Every route is `POST` with one closed JSON body (`UsageArgs`):
//!
//! | Field | Meaning |
//! |---|---|
//! | `since`, `until` | RFC 3339, or a relative `24h` / `7d` / `90m` / `2w` before now. Default: the 7 days before now. |
//! | `provider` | Only this harness (`sessions.provider`). |
//! | `project` (alias `workspace`) | A case-insensitive fragment of the session's `cwd` or `project_slug`. |
//! | `tz_offset_min` | Minutes east of UTC that day, hour and weekday buckets use (the browser's `-getTimezoneOffset()`). Default 0. |
//! | `limit` | Rows of a ranking (`sessions`, the top lists). |
//!
//! | Route | Answer |
//! |---|---|
//! | `overview` | `totals`, `daily`, `calendar`, `top_projects`, `top_models`, `top_harnesses`, `live` |
//! | `daily` | `days`, cost and tokens per day stacked by model |
//! | `breakdown` | per-model, per-project and per-harness tables |
//! | `cache` | cache hit ratio and the savings estimate, in total and per model |
//! | `sessions` | sessions ranked by their cost in the window |
//! | `distributions` | session archetypes and the duration, turns and tools-per-turn histograms |
//! | `activity` | turns, cost and tokens by weekday × hour |
//!
//! **Windowing.** Tokens and cost are summed from `generation` observations
//! by their own `start_ns`, so only what was spent inside the window counts
//! (a session that spent $10 last week and $0.10 today adds $0.10 to today).
//! Turns are `traces` by `start_ns`; a session counts when it has a turn or
//! a generation in the window; launches by `started_ns`.
//!
//! **Cost.** `cost_usd` is always the sum of the generations' own cost:
//! agent-mux's price for the tokens (`total_cost_usd`, from the `models`
//! table), or, for a call whose transcript carries the provider's cost,
//! that figure (`provided_cost.total`; the store then leaves the priced
//! columns empty). The
//! harness's own session total (`launches.reported_cost_usd`, e.g. Claude
//! Code's status line) is lifetime, not attributable to a day or a model, so
//! it is reported beside it as `reported_cost_usd` and never mixed in.
//!
//! **Scope.** `agent-mux web --workspace DIR` limits every aggregate to the
//! sessions (and launches) whose `cwd` the `TraceService` scope allows.
//!
//! Every request opens the store read-only (`tracing::store::open_ro`) on a
//! blocking thread; nothing here writes.

use super::{ApiError, ApiResult, WebState};
use crate::tracing::analysis::RuntimeState;
use crate::tracing::analysis::metrics::completed_percentiles;
use axum::Router;
use axum::extract::State;
use axum::routing::post;
use rusqlite::types::ToSql;
use rusqlite::{Connection, Statement};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Rows of a ranking when `limit` is absent.
pub const DEFAULT_LIMIT: usize = 50;
/// The largest `limit` a ranking answers.
pub const MAX_LIMIT: usize = 500;
/// Rows of each Overview top list.
pub const TOP_N: usize = 8;
/// Models drawn as their own series per day; the rest are `other`.
pub const DAILY_MODELS: usize = 8;
/// The Overview's activity calendar reaches at least this many days back.
pub const CALENDAR_MIN_DAYS: i64 = 12 * 7;

const NS: i64 = 1_000_000_000;
const DAY_NS: i64 = 86_400 * NS;

/// The usage routes, under `/api/usage/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/usage/overview", post(overview))
        .route("/api/usage/daily", post(daily))
        .route("/api/usage/breakdown", post(breakdown))
        .route("/api/usage/cache", post(cache))
        .route("/api/usage/sessions", post(sessions))
        .route("/api/usage/distributions", post(distributions))
        .route("/api/usage/activity", post(activity))
}

/// The body every usage route takes.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageArgs {
    pub since: Option<String>,
    pub until: Option<String>,
    pub provider: Option<String>,
    #[serde(alias = "workspace")]
    pub project: Option<String>,
    pub tz_offset_min: Option<i32>,
    pub limit: Option<usize>,
}

/// The resolved filters of one request.
#[derive(Debug, Clone)]
pub struct Filter {
    pub since_ns: i64,
    pub until_ns: i64,
    pub since: String,
    pub until: String,
    pub provider: Option<String>,
    /// Lower-cased project fragment.
    pub project: Option<String>,
    /// The `cwd`s the server's scope allows, as a JSON array; `None` when
    /// every workspace is allowed.
    pub scope: Option<String>,
    /// Seconds added to a timestamp before bucketing it.
    pub tz_secs: i64,
    pub tz_offset_min: i32,
    pub limit: usize,
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `24h`, `7d`, `90m`, `2w` before `now`, or an RFC 3339 instant.
fn parse_instant(field: &str, s: &str, now: OffsetDateTime) -> Result<OffsetDateTime, ApiError> {
    let s = s.trim();
    if let Some(unit) = s.chars().last()
        && let Ok(n) = s[..s.len() - unit.len_utf8()].trim().parse::<i64>()
    {
        let secs = match unit.to_ascii_lowercase() {
            'm' => Some(60),
            'h' => Some(3_600),
            'd' => Some(86_400),
            'w' => Some(7 * 86_400),
            _ => None,
        };
        if let Some(secs) = secs {
            return Ok(now - time::Duration::seconds(n.saturating_mul(secs)));
        }
    }
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| {
        ApiError::bad_request(format!(
            "invalid '{field}': '{s}' (RFC 3339 or 24h/7d/90m/2w)"
        ))
    })
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

impl UsageArgs {
    /// Resolves the window and filters; `scope` is the allowed `cwd` list.
    pub fn resolve(&self, now: OffsetDateTime, scope: Option<String>) -> Result<Filter, ApiError> {
        let until = match non_empty(&self.until) {
            Some(u) => parse_instant("until", &u, now)?,
            None => now,
        };
        let since = match non_empty(&self.since) {
            Some(s) => parse_instant("since", &s, now)?,
            None => until - time::Duration::days(7),
        };
        if since > until {
            return Err(ApiError::bad_request("'since' must not be after 'until'"));
        }
        let tz = self.tz_offset_min.unwrap_or(0);
        if !(-14 * 60..=14 * 60).contains(&tz) {
            return Err(ApiError::bad_request("'tz_offset_min' must be within ±840"));
        }
        Ok(Filter {
            since_ns: since.unix_timestamp_nanos() as i64,
            until_ns: until.unix_timestamp_nanos() as i64,
            since: rfc3339(since),
            until: rfc3339(until),
            provider: non_empty(&self.provider),
            project: non_empty(&self.project).map(|p| p.to_lowercase()),
            scope,
            tz_secs: i64::from(tz) * 60,
            tz_offset_min: tz,
            limit: self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        })
    }

    fn window_json(f: &Filter) -> Value {
        json!({
            "since": f.since, "until": f.until,
            "since_ns": f.since_ns, "until_ns": f.until_ns,
            "tz_offset_min": f.tz_offset_min,
        })
    }
}

// ---- SQL ---------------------------------------------------------------------

/// The session filters, over a `sessions` row aliased `s`.
const SESSION_FILTER: &str = "(:prov IS NULL OR s.provider = :prov)
  AND (:scope IS NULL OR s.cwd IN (SELECT value FROM json_each(:scope)))
  AND (:proj IS NULL OR instr(lower(COALESCE(s.cwd, '') || char(1) || COALESCE(s.project_slug, '')), :proj) > 0)";

/// The same filters over a `launches` row aliased `l`.
const LAUNCH_FILTER: &str = "(:prov IS NULL OR l.provider = :prov)
  AND (:scope IS NULL OR l.cwd IN (SELECT value FROM json_each(:scope)))
  AND (:proj IS NULL OR instr(lower(COALESCE(l.cwd, '') || char(1) || COALESCE(l.project_slug, '')), :proj) > 0)";

/// Generation observations in the window, joined to their trace `t` and
/// session `s`.
fn gen_from() -> String {
    format!(
        "FROM observations o JOIN traces t ON t.id = o.trace_id JOIN sessions s ON s.key = t.session_key
         WHERE o.type = 'generation' AND o.start_ns >= :since AND o.start_ns < :until AND {SESSION_FILTER}"
    )
}

/// Turns in the window, with their session `s`.
fn turn_from() -> String {
    format!(
        "FROM traces t JOIN sessions s ON s.key = t.session_key
         WHERE t.start_ns >= :since AND t.start_ns < :until AND {SESSION_FILTER}"
    )
}

/// The token and cost sums every grouping selects (13 columns). A cost
/// is the priced column, else the provider's own figure for the call
/// (`provided_cost`, which the store keeps instead of pricing it).
const SUMS: &str = "COUNT(*),
  COALESCE(SUM(o.input_tokens), 0), COALESCE(SUM(o.output_tokens), 0),
  COALESCE(SUM(o.cache_read_tokens), 0), COALESCE(SUM(o.cache_write_tokens), 0),
  COALESCE(SUM(o.reasoning_tokens), 0),
  COALESCE(SUM(COALESCE(o.total_cost_usd, json_extract(o.provided_cost, '$.total'))), 0.0),
  COALESCE(SUM(COALESCE(o.input_cost_usd, json_extract(o.provided_cost, '$.input'))), 0.0),
  COALESCE(SUM(COALESCE(o.output_cost_usd, json_extract(o.provided_cost, '$.output'))), 0.0),
  COALESCE(SUM(COALESCE(o.cache_read_cost_usd, json_extract(o.provided_cost, '$.cache_read_input_tokens'))), 0.0),
  COALESCE(SUM(COALESCE(o.cache_write_cost_usd, json_extract(o.provided_cost, '$.input_cache_creation_5m'))), 0.0),
  COALESCE(SUM(o.total_cost_usd IS NULL AND json_extract(o.provided_cost, '$.total') IS NULL
               AND (o.input_tokens IS NOT NULL OR o.output_tokens IS NOT NULL OR o.total_tokens IS NOT NULL)), 0),
  COUNT(DISTINCT t.session_key)";

/// One group's token and cost sums.
#[derive(Debug, Clone, Default)]
pub struct Sums {
    pub generations: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub cost: f64,
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub cache_write_cost: f64,
    pub unpriced: i64,
    pub sessions: i64,
}

impl Sums {
    fn read(r: &rusqlite::Row, at: usize) -> rusqlite::Result<Sums> {
        Ok(Sums {
            generations: r.get(at)?,
            input: r.get(at + 1)?,
            output: r.get(at + 2)?,
            cache_read: r.get(at + 3)?,
            cache_write: r.get(at + 4)?,
            reasoning: r.get(at + 5)?,
            cost: r.get(at + 6)?,
            input_cost: r.get(at + 7)?,
            output_cost: r.get(at + 8)?,
            cache_read_cost: r.get(at + 9)?,
            cache_write_cost: r.get(at + 10)?,
            unpriced: r.get(at + 11)?,
            sessions: r.get(at + 12)?,
        })
    }

    /// Input, output and cache tokens (reasoning is part of output).
    pub fn total_tokens(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn json(&self) -> Value {
        json!({
            "generations": self.generations,
            "sessions": self.sessions,
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cache_read_tokens": self.cache_read,
            "cache_write_tokens": self.cache_write,
            "reasoning_tokens": self.reasoning,
            "total_tokens": self.total_tokens(),
            "cost_usd": self.cost,
            "unpriced_generations": self.unpriced,
        })
    }
}

/// Binds the filter's named parameters a statement uses, plus `extra`.
fn bind(stmt: &mut Statement, f: &Filter, extra: &[(&str, &dyn ToSql)]) -> rusqlite::Result<()> {
    let base: [(&str, &dyn ToSql); 6] = [
        (":since", &f.since_ns),
        (":until", &f.until_ns),
        (":prov", &f.provider),
        (":scope", &f.scope),
        (":proj", &f.project),
        (":tz", &f.tz_secs),
    ];
    for (name, value) in base.iter().chain(extra.iter()) {
        if let Some(i) = stmt.parameter_index(name)? {
            stmt.raw_bind_parameter(i, *value)?;
        }
    }
    Ok(())
}

/// Runs `sql` with the filter bound and maps every row.
fn rows<T>(
    conn: &Connection,
    sql: &str,
    f: &Filter,
    extra: &[(&str, &dyn ToSql)],
    mut map: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    bind(&mut stmt, f, extra)?;
    let mut out = Vec::new();
    let mut q = stmt.raw_query();
    while let Some(r) = q.next()? {
        out.push(map(r)?);
    }
    Ok(out)
}

fn one<T>(
    conn: &Connection,
    sql: &str,
    f: &Filter,
    map: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    rows(conn, sql, f, &[], map)?
        .into_iter()
        .next()
        .ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// The local day (`YYYY-MM-DD`) of a nanosecond column, shifted by `:tz`.
fn day_of(col: &str) -> String {
    format!("strftime('%Y-%m-%d', {col} / 1000000000 + :tz, 'unixepoch')")
}

/// Every local day from `since` to `until` inclusive (at most 1000).
pub fn days_between(since_ns: i64, until_ns: i64, tz_secs: i64) -> Vec<String> {
    let day = |ns: i64| (ns.div_euclid(NS) + tz_secs).div_euclid(86_400);
    let (first, last) = (day(since_ns), day(until_ns.saturating_sub(1).max(since_ns)));
    (first..=last)
        .take(1000)
        .filter_map(|d| {
            OffsetDateTime::from_unix_timestamp(d * 86_400)
                .ok()
                .map(|t| format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day()))
        })
        .collect()
}

/// The project a session is grouped under: its `cwd`, else its slug.
const PROJECT_KEY: &str = "COALESCE(NULLIF(s.cwd, ''), NULLIF(s.project_slug, ''), '(none)')";

// ---- queries -----------------------------------------------------------------

/// Totals of the window.
pub fn totals(conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let gens = one(conn, &format!("SELECT {SUMS} {}", gen_from()), f, |r| {
        Sums::read(r, 0)
    })?;
    let turns: i64 = one(conn, &format!("SELECT COUNT(*) {}", turn_from()), f, |r| {
        r.get(0)
    })?;
    let sessions: i64 = one(
        conn,
        &format!(
            "SELECT COUNT(*) FROM (SELECT t.session_key {} UNION SELECT t.session_key {})",
            turn_from(),
            gen_from()
        ),
        f,
        |r| r.get(0),
    )?;
    let (launches, reported, added, removed): (i64, Option<f64>, i64, i64) = one(
        conn,
        &format!(
            "SELECT COUNT(*), SUM(l.reported_cost_usd), COALESCE(SUM(l.reported_lines_added), 0),
                    COALESCE(SUM(l.reported_lines_removed), 0)
             FROM launches l WHERE l.started_ns >= :since AND l.started_ns < :until AND {LAUNCH_FILTER}"
        ),
        f,
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut v = gens.json();
    v["sessions"] = json!(sessions);
    v["turns"] = json!(turns);
    v["launches"] = json!(launches);
    v["reported_cost_usd"] = json!(reported);
    v["lines_added"] = json!(added);
    v["lines_removed"] = json!(removed);
    Ok(v)
}

/// Cost, tokens, turns and sessions per local day (days with data only).
pub fn per_day(conn: &Connection, f: &Filter) -> rusqlite::Result<Vec<Value>> {
    let mut days: BTreeMap<String, (Sums, i64, i64)> = BTreeMap::new();
    for (day, s) in rows(
        conn,
        &format!(
            "SELECT {d}, {SUMS} {} GROUP BY 1",
            gen_from(),
            d = day_of("o.start_ns")
        ),
        f,
        &[],
        |r| Ok((r.get::<_, String>(0)?, Sums::read(r, 1)?)),
    )? {
        days.entry(day).or_default().0 = s;
    }
    for (day, turns) in rows(
        conn,
        &format!(
            "SELECT {d}, COUNT(*) {} GROUP BY 1",
            turn_from(),
            d = day_of("t.start_ns")
        ),
        f,
        &[],
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
    )? {
        days.entry(day).or_default().1 = turns;
    }
    // A session counts on every day it was active: a turn that started
    // that day, or a generation made that day (a turn may cross midnight).
    for (day, sessions) in rows(
        conn,
        &format!(
            "SELECT day, COUNT(DISTINCT key) FROM (
               SELECT {td} AS day, t.session_key AS key {}
               UNION SELECT {gd}, t.session_key {}
             ) GROUP BY 1",
            turn_from(),
            gen_from(),
            td = day_of("t.start_ns"),
            gd = day_of("o.start_ns")
        ),
        f,
        &[],
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
    )? {
        days.entry(day).or_default().2 = sessions;
    }
    Ok(days
        .into_iter()
        .map(|(day, (s, turns, sessions))| {
            json!({
                "day": day,
                "cost_usd": s.cost,
                "total_tokens": s.total_tokens(),
                "input_tokens": s.input,
                "output_tokens": s.output,
                "generations": s.generations,
                "turns": turns,
                "sessions": sessions,
            })
        })
        .collect())
}

/// Generation sums grouped by an SQL key expression, most expensive first.
pub fn grouped(conn: &Connection, f: &Filter, key: &str) -> rusqlite::Result<Vec<(String, Sums)>> {
    rows(
        conn,
        &format!(
            "SELECT {key}, {SUMS} {} GROUP BY 1 ORDER BY 8 DESC, 1 ASC",
            gen_from()
        ),
        f,
        &[],
        |r| Ok((r.get::<_, String>(0)?, Sums::read(r, 1)?)),
    )
}

const MODEL_KEY: &str = "COALESCE(NULLIF(o.model, ''), '(unknown)')";

fn group_json(name: &str, items: &[(String, Sums)], total_cost: f64) -> Vec<Value> {
    items
        .iter()
        .map(|(k, s)| {
            let mut v = s.json();
            v[name] = json!(k);
            v["share"] = json!(if total_cost > 0.0 {
                s.cost / total_cost
            } else {
                0.0
            });
            v
        })
        .collect()
}

/// Launches, reported cost, lines added and lines removed.
type LaunchSums = (i64, Option<f64>, i64, i64);

/// Per-harness launches, reported cost and lines changed in the window.
fn launch_by_provider(
    conn: &Connection,
    f: &Filter,
) -> rusqlite::Result<HashMap<String, LaunchSums>> {
    Ok(rows(
        conn,
        &format!(
            "SELECT l.provider, COUNT(*), SUM(l.reported_cost_usd),
                    COALESCE(SUM(l.reported_lines_added), 0), COALESCE(SUM(l.reported_lines_removed), 0)
             FROM launches l WHERE l.started_ns >= :since AND l.started_ns < :until AND {LAUNCH_FILTER}
             GROUP BY 1"
        ),
        f,
        &[],
        |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))),
    )?
    .into_iter()
    .collect())
}

fn turns_by(conn: &Connection, f: &Filter, key: &str) -> rusqlite::Result<HashMap<String, i64>> {
    Ok(rows(
        conn,
        &format!("SELECT {key}, COUNT(*) {} GROUP BY 1", turn_from()),
        f,
        &[],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .into_iter()
    .collect())
}

/// Distinct sessions per group, active in the window by a turn start or a
/// generation (as the totals count them). `key` reads the session `s` only.
fn sessions_by(conn: &Connection, f: &Filter, key: &str) -> rusqlite::Result<HashMap<String, i64>> {
    Ok(rows(
        conn,
        &format!(
            "SELECT g, COUNT(DISTINCT k) FROM (
               SELECT {key} AS g, t.session_key AS k {}
               UNION SELECT {key}, t.session_key {}
             ) GROUP BY 1",
            turn_from(),
            gen_from()
        ),
        f,
        &[],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .into_iter()
    .collect())
}

/// The harness rows: generation sums, turns, launches, reported cost, lines.
fn harnesses(conn: &Connection, f: &Filter, total_cost: f64) -> rusqlite::Result<Vec<Value>> {
    let gens = grouped(conn, f, "s.provider")?;
    let turns = turns_by(conn, f, "s.provider")?;
    let sessions = sessions_by(conn, f, "s.provider")?;
    let launches = launch_by_provider(conn, f)?;
    let mut keys: Vec<String> = gens.iter().map(|(k, _)| k.clone()).collect();
    for k in turns.keys().chain(launches.keys()) {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }
    let by: HashMap<&str, &Sums> = gens.iter().map(|(k, s)| (k.as_str(), s)).collect();
    let empty = Sums::default();
    Ok(keys
        .iter()
        .map(|k| {
            let s = by.get(k.as_str()).copied().unwrap_or(&empty);
            let mut v = group_json("provider", &[(k.clone(), s.clone())], total_cost).remove(0);
            v["turns"] = json!(turns.get(k).copied().unwrap_or(0));
            v["sessions"] = json!(sessions.get(k).copied().unwrap_or(0));
            let (n, reported, added, removed) = launches.get(k).cloned().unwrap_or_default();
            v["launches"] = json!(n);
            v["reported_cost_usd"] = json!(reported);
            v["lines_added"] = json!(added);
            v["lines_removed"] = json!(removed);
            v
        })
        .collect())
}

/// The project rows: generation sums and turns per project.
fn projects(conn: &Connection, f: &Filter, total_cost: f64) -> rusqlite::Result<Vec<Value>> {
    let gens = grouped(conn, f, PROJECT_KEY)?;
    let turns = turns_by(conn, f, PROJECT_KEY)?;
    let sessions = sessions_by(conn, f, PROJECT_KEY)?;
    let mut out = group_json("project", &gens, total_cost);
    let mut rest: Vec<&String> = turns
        .keys()
        .filter(|k| !gens.iter().any(|(g, _)| g == *k))
        .collect();
    rest.sort();
    for k in rest {
        let mut v = Sums::default().json();
        v["project"] = json!(k);
        v["share"] = json!(0.0);
        out.push(v);
    }
    for v in &mut out {
        let k = v["project"].as_str().unwrap_or_default().to_string();
        v["turns"] = json!(turns.get(&k).copied().unwrap_or(0));
        v["sessions"] = json!(sessions.get(&k).copied().unwrap_or(0));
    }
    Ok(out)
}

/// Turns per local day over the calendar's span (at least
/// `CALENDAR_MIN_DAYS` back from `until`).
pub fn calendar(conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let mut cf = f.clone();
    cf.since_ns = f.since_ns.min(f.until_ns - CALENDAR_MIN_DAYS * DAY_NS);
    let days = rows(
        conn,
        &format!(
            "SELECT {d}, COUNT(*) {} GROUP BY 1",
            turn_from(),
            d = day_of("t.start_ns")
        ),
        &cf,
        &[],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )?;
    let span = days_between(cf.since_ns, cf.until_ns, cf.tz_secs);
    Ok(json!({
        "from": span.first(),
        "to": span.last(),
        "turns": days.into_iter().collect::<BTreeMap<_, _>>(),
    }))
}

/// Live sessions of the snapshots that pass the filters (not exited).
fn live(state: &WebState, f: &Filter) -> Value {
    let now = crate::tracing::store::now_ns();
    let (sessions, available) = state.trace.load_live_sessions(now);
    let scope = &state.trace.config().scope;
    let list: Vec<Value> = sessions
        .into_iter()
        .filter(|s| {
            f.provider
                .as_deref()
                .is_none_or(|p| s.provider.as_deref() == Some(p))
        })
        .filter(|s| s.state != RuntimeState::Exited)
        .filter(|s| scope.allows_session_cwd(&s.cwd))
        .filter(|s| {
            f.project
                .as_deref()
                .is_none_or(|p| s.cwd.to_string_lossy().to_lowercase().contains(p))
        })
        .map(|s| {
            json!({
                "launch_id": s.launch_id,
                "session_key": s.session_key,
                "provider": s.provider,
                "cwd": s.cwd,
                "state": s.state.as_str(),
                "updated_at_ns": s.updated_at_ns,
                "active_tools": s.active_tools,
            })
        })
        .collect();
    json!({ "available": available, "sessions": list })
}

/// Per-day per-model cost and tokens: the top `DAILY_MODELS` by cost, the
/// rest summed as `other`.
pub fn daily_by_model(conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let days = days_between(f.since_ns, f.until_ns, f.tz_secs);
    let index: HashMap<&str, usize> = days
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let cells = rows(
        conn,
        &format!(
            "SELECT {d}, {MODEL_KEY}, {SUMS} {} GROUP BY 1, 2",
            gen_from(),
            d = day_of("o.start_ns")
        ),
        f,
        &[],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                Sums::read(r, 2)?,
            ))
        },
    )?;
    let mut by_model: HashMap<String, f64> = HashMap::new();
    for (_, m, s) in &cells {
        *by_model.entry(m.clone()).or_default() += s.cost + s.total_tokens() as f64 * 1e-12;
    }
    let mut models: Vec<(String, f64)> = by_model.into_iter().collect();
    models.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut names: Vec<String> = models
        .iter()
        .take(DAILY_MODELS)
        .map(|m| m.0.clone())
        .collect();
    if models.len() > DAILY_MODELS {
        names.push("other".into());
    }
    let slot = |m: &str| names.iter().position(|n| n == m).unwrap_or(names.len() - 1);
    let mut cost = vec![vec![0.0f64; days.len()]; names.len()];
    let mut tokens = vec![vec![0i64; days.len()]; names.len()];
    for (d, m, s) in &cells {
        if let Some(&i) = index.get(d.as_str()) {
            let k = slot(m);
            cost[k][i] += s.cost;
            tokens[k][i] += s.total_tokens();
        }
    }
    let series = |values: &dyn Fn(usize) -> Value| -> Vec<Value> {
        names
            .iter()
            .enumerate()
            .map(|(k, n)| json!({ "name": n, "values": values(k) }))
            .collect()
    };
    Ok(json!({
        "days": days,
        "models": names,
        "cost": series(&|k| json!(cost[k])),
        "tokens": series(&|k| json!(tokens[k])),
    }))
}

/// Cache economics of a group: hit ratio and the savings estimate.
pub fn cache_json(s: &Sums) -> Value {
    let prompt = s.input + s.cache_read + s.cache_write;
    let hit_ratio = (prompt > 0).then(|| s.cache_read as f64 / prompt as f64);
    // the observed uncached input price per token
    let rate = (s.input > 0 && s.input_cost > 0.0).then(|| s.input_cost / s.input as f64);
    let savings = rate.map(|r| s.cache_read as f64 * r - s.cache_read_cost);
    let write_premium = rate.map(|r| s.cache_write_cost - s.cache_write as f64 * r);
    let net = match (savings, write_premium) {
        (Some(a), Some(b)) => Some(a - b),
        _ => None,
    };
    json!({
        "input_tokens": s.input,
        "cache_read_tokens": s.cache_read,
        "cache_write_tokens": s.cache_write,
        "hit_ratio": hit_ratio,
        "input_cost_usd": s.input_cost,
        "cache_read_cost_usd": s.cache_read_cost,
        "cache_write_cost_usd": s.cache_write_cost,
        "input_rate_per_m": rate.map(|r| r * 1e6),
        "savings_usd": savings,
        "write_premium_usd": write_premium,
        "net_savings_usd": net,
    })
}

/// One session's facts within the window.
#[derive(Debug, Clone, Default)]
pub struct SessionFacts {
    pub key: String,
    pub provider: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub turns: i64,
    pub tools: i64,
    pub first_ns: Option<i64>,
    pub last_ns: Option<i64>,
    pub sums: Sums,
    pub reported_cost: Option<f64>,
}

impl SessionFacts {
    pub fn duration_ms(&self) -> Option<i64> {
        Some((self.last_ns? - self.first_ns?).max(0) / 1_000_000)
    }

    pub fn tools_per_turn(&self) -> f64 {
        if self.turns > 0 {
            self.tools as f64 / self.turns as f64
        } else {
            0.0
        }
    }
}

/// Every session with a turn or a generation in the window.
pub fn session_facts(conn: &Connection, f: &Filter) -> rusqlite::Result<Vec<SessionFacts>> {
    let mut by: BTreeMap<String, SessionFacts> = BTreeMap::new();
    for (key, turns, first, last, tools) in rows(
        conn,
        &format!(
            "SELECT t.session_key, COUNT(*), MIN(t.start_ns), MAX(COALESCE(t.end_ns, t.start_ns)),
                    (SELECT COUNT(*) FROM observations x JOIN traces y ON y.id = x.trace_id
                      WHERE y.session_key = t.session_key AND x.type IN ('tool', 'agent')
                        -- a sub-agent's turns are containers, not calls
                        AND COALESCE(x.kind, '') <> 'subagent_turn'
                        AND y.start_ns >= :since AND y.start_ns < :until)
             {} GROUP BY 1",
            turn_from()
        ),
        f,
        &[],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get(4)?,
            ))
        },
    )? {
        let e = by.entry(key.clone()).or_default();
        e.key = key;
        e.turns = turns;
        e.tools = tools;
        e.first_ns = first;
        e.last_ns = last;
    }
    for (key, s, first, last) in rows(
        conn,
        &format!(
            "SELECT t.session_key, {SUMS}, MIN(o.start_ns), MAX(COALESCE(o.end_ns, o.start_ns)) {} GROUP BY 1",
            gen_from()
        ),
        f,
        &[],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                Sums::read(r, 1)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, Option<i64>>(15)?,
            ))
        },
    )? {
        let e = by.entry(key.clone()).or_default();
        e.key = key;
        e.sums = s;
        e.first_ns = match (e.first_ns, first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        e.last_ns = match (e.last_ns, last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    let mut stmt = conn.prepare(
        "SELECT s.provider, s.cwd, s.title,
                (SELECT MAX(reported_cost_usd) FROM launches l WHERE l.session_key = s.key)
         FROM sessions s WHERE s.key = ?1",
    )?;
    for e in by.values_mut() {
        let (p, c, t, r) = stmt.query_row([&e.key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        e.provider = p;
        e.cwd = c;
        e.title = t;
        e.reported_cost = r;
    }
    Ok(by.into_values().collect())
}

fn session_json(s: &SessionFacts) -> Value {
    let mut v = s.sums.json();
    v["session_key"] = json!(s.key);
    v["provider"] = json!(s.provider);
    v["cwd"] = json!(s.cwd);
    v["title"] = json!(s.title);
    v["turns"] = json!(s.turns);
    v["tools"] = json!(s.tools);
    v["tools_per_turn"] = json!(s.tools_per_turn());
    v["first_ns"] = json!(s.first_ns);
    v["last_ns"] = json!(s.last_ns);
    v["duration_ms"] = json!(s.duration_ms());
    v["reported_cost_usd"] = json!(s.reported_cost);
    v.as_object_mut().map(|o| o.remove("sessions"));
    v
}

/// Archetypes, in the order they are tried; the first that fits wins.
pub const ARCHETYPES: &[(&str, &str)] = &[
    ("quick", "at most 2 turns and under one tool call per turn"),
    ("tool-heavy", "10 or more tool calls per turn"),
    (
        "long-running",
        "an hour or more between its first and last activity",
    ),
    (
        "conversation",
        "10 or more turns and under 3 tool calls per turn",
    ),
    ("task", "everything else"),
];

pub fn archetype(s: &SessionFacts) -> &'static str {
    let tpt = s.tools_per_turn();
    let dur = s.duration_ms().unwrap_or(0);
    if s.turns <= 2 && tpt < 1.0 {
        "quick"
    } else if tpt >= 10.0 {
        "tool-heavy"
    } else if dur >= 3_600_000 {
        "long-running"
    } else if s.turns >= 10 && tpt < 3.0 {
        "conversation"
    } else {
        "task"
    }
}

/// Histogram buckets: (label, exclusive upper bound).
const DURATION_BUCKETS: &[(&str, f64)] = &[
    ("<1m", 60_000.0),
    ("1–5m", 300_000.0),
    ("5–15m", 900_000.0),
    ("15–60m", 3_600_000.0),
    ("1–4h", 14_400_000.0),
    ("≥4h", f64::INFINITY),
];
const TURN_BUCKETS: &[(&str, f64)] = &[
    ("1", 2.0),
    ("2–5", 6.0),
    ("6–10", 11.0),
    ("11–25", 26.0),
    ("26–50", 51.0),
    (">50", f64::INFINITY),
];
const TPT_BUCKETS: &[(&str, f64)] = &[
    ("0", f64::MIN_POSITIVE),
    ("<1", 1.0),
    ("1–3", 3.0),
    ("3–10", 10.0),
    ("10–25", 25.0),
    ("≥25", f64::INFINITY),
];

fn histogram(values: &[f64], buckets: &[(&str, f64)]) -> Value {
    let mut counts = vec![0i64; buckets.len()];
    for v in values {
        let i = buckets
            .iter()
            .position(|(_, hi)| *v < *hi)
            .unwrap_or(buckets.len() - 1);
        counts[i] += 1;
    }
    json!({
        "labels": buckets.iter().map(|b| b.0).collect::<Vec<_>>(),
        "counts": counts,
    })
}

fn percentiles(values: &[u64]) -> Value {
    let opts: Vec<Option<u64>> = values.iter().map(|v| Some(*v)).collect();
    match completed_percentiles(&opts) {
        Some((p50, p95, max, n)) => json!({ "p50": p50, "p95": p95, "max": max, "count": n }),
        None => json!({ "p50": null, "p95": null, "max": null, "count": 0 }),
    }
}

// ---- handlers ----------------------------------------------------------------

/// The JSON body, if any; an empty body counts as `{}`.
fn parse_args(body: &[u8]) -> Result<UsageArgs, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(UsageArgs::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))
}

/// The `cwd`s the server's scope allows, as a JSON array, or `None` when
/// it allows every workspace.
fn scope_list(state: &WebState, conn: &Connection) -> Result<Option<String>, ApiError> {
    let scope = &state.trace.config().scope;
    if scope.is_all_workspaces() {
        return Ok(None);
    }
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT cwd FROM sessions WHERE cwd IS NOT NULL
             UNION SELECT DISTINCT cwd FROM launches",
        )
        .map_err(db_err)?;
    let cwds: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .filter_map(Result::ok)
        .filter(|c| scope.allows_session_cwd(std::path::Path::new(c)))
        .collect();
    Ok(Some(Value::from(cwds).to_string()))
}

fn db_err(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

/// Parses the body, opens the store read-only on a blocking thread and
/// answers `run`'s document with `ok` and `window` added.
async fn answer(
    state: Arc<WebState>,
    body: axum::body::Bytes,
    run: fn(&WebState, &Connection, &Filter) -> rusqlite::Result<Value>,
) -> ApiResult<Value> {
    let args = parse_args(&body)?;
    let doc = tokio::task::spawn_blocking(move || -> Result<Value, ApiError> {
        let conn = crate::tracing::store::open_ro(&state.db)
            .map_err(|e| ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e))?;
        let scope = scope_list(&state, &conn)?;
        let f = args.resolve(OffsetDateTime::now_utc(), scope)?;
        let mut doc = run(&state, &conn, &f).map_err(db_err)?;
        doc["ok"] = json!(true);
        doc["window"] = UsageArgs::window_json(&f);
        doc["filters"] = json!({ "provider": f.provider, "project": f.project, "limit": f.limit });
        Ok(doc)
    })
    .await
    .map_err(|e| ApiError::internal(format!("usage query panicked: {e}")))??;
    Ok(axum::Json(doc))
}

fn overview_doc(state: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let totals = totals(conn, f)?;
    let total_cost = totals["cost_usd"].as_f64().unwrap_or(0.0);
    let top = |mut v: Vec<Value>| {
        v.truncate(TOP_N.min(f.limit));
        v
    };
    Ok(json!({
        "totals": totals,
        "days": days_between(f.since_ns, f.until_ns, f.tz_secs),
        "daily": per_day(conn, f)?,
        "calendar": calendar(conn, f)?,
        "top_projects": top(projects(conn, f, total_cost)?),
        "top_models": top(group_json("model", &grouped(conn, f, MODEL_KEY)?, total_cost)),
        "top_harnesses": top(harnesses(conn, f, total_cost)?),
        "live": live(state, f),
    }))
}

fn daily_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    daily_by_model(conn, f)
}

fn breakdown_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let totals = totals(conn, f)?;
    let total_cost = totals["cost_usd"].as_f64().unwrap_or(0.0);
    Ok(json!({
        "totals": totals,
        "models": group_json("model", &grouped(conn, f, MODEL_KEY)?, total_cost),
        "projects": projects(conn, f, total_cost)?,
        "harnesses": harnesses(conn, f, total_cost)?,
    }))
}

fn cache_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let all = one(conn, &format!("SELECT {SUMS} {}", gen_from()), f, |r| {
        Sums::read(r, 0)
    })?;
    let models: Vec<Value> = grouped(conn, f, MODEL_KEY)?
        .iter()
        .map(|(m, s)| {
            let mut v = cache_json(s);
            v["model"] = json!(m);
            v
        })
        .collect();
    // the total savings is the sum of the models' own estimates, each at
    // its own input price
    let sum = |k: &str| -> Option<f64> {
        let vals: Vec<f64> = models.iter().filter_map(|m| m[k].as_f64()).collect();
        (!vals.is_empty()).then(|| vals.iter().sum())
    };
    let mut totals = cache_json(&all);
    totals["savings_usd"] = json!(sum("savings_usd"));
    totals["write_premium_usd"] = json!(sum("write_premium_usd"));
    totals["net_savings_usd"] = json!(sum("net_savings_usd"));
    totals["input_rate_per_m"] = Value::Null;
    Ok(json!({ "totals": totals, "models": models }))
}

fn sessions_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let mut all = session_facts(conn, f)?;
    let total = all.len();
    all.sort_by(|a, b| {
        b.sums
            .cost
            .total_cmp(&a.sums.cost)
            .then(b.sums.total_tokens().cmp(&a.sums.total_tokens()))
            .then(a.key.cmp(&b.key))
    });
    all.truncate(f.limit);
    Ok(json!({
        "total": total,
        "sessions": all.iter().map(session_json).collect::<Vec<_>>(),
    }))
}

fn distributions_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let all = session_facts(conn, f)?;
    let mut groups: BTreeMap<&str, (i64, f64, i64, i64)> = BTreeMap::new();
    for s in &all {
        let g = groups.entry(archetype(s)).or_default();
        g.0 += 1;
        g.1 += s.sums.cost;
        g.2 += s.sums.total_tokens();
        g.3 += s.turns;
    }
    let archetypes: Vec<Value> = ARCHETYPES
        .iter()
        .map(|(name, description)| {
            let (n, cost, tokens, turns) = groups.get(name).copied().unwrap_or_default();
            json!({
                "archetype": name,
                "description": description,
                "sessions": n,
                "cost_usd": cost,
                "total_tokens": tokens,
                "avg_turns": if n > 0 { turns as f64 / n as f64 } else { 0.0 },
            })
        })
        .collect();
    let durations: Vec<u64> = all
        .iter()
        .filter_map(|s| s.duration_ms())
        .map(|d| d as u64)
        .collect();
    let turns: Vec<u64> = all.iter().map(|s| s.turns as u64).collect();
    let tpt: Vec<f64> = all
        .iter()
        .filter(|s| s.turns > 0)
        .map(SessionFacts::tools_per_turn)
        .collect();
    let tpt_hundredths: Vec<u64> = tpt.iter().map(|v| (v * 100.0).round() as u64).collect();
    let tpt_pct = percentiles(&tpt_hundredths);
    let scaled = |k: &str| tpt_pct[k].as_u64().map(|v| v as f64 / 100.0);
    Ok(json!({
        "sessions": all.len(),
        "archetypes": archetypes,
        "duration_ms": {
            "histogram": histogram(&durations.iter().map(|v| *v as f64).collect::<Vec<_>>(), DURATION_BUCKETS),
            "percentiles": percentiles(&durations),
        },
        "turns": {
            "histogram": histogram(&turns.iter().map(|v| *v as f64).collect::<Vec<_>>(), TURN_BUCKETS),
            "percentiles": percentiles(&turns),
        },
        "tools_per_turn": {
            "histogram": histogram(&tpt, TPT_BUCKETS),
            "percentiles": { "p50": scaled("p50"), "p95": scaled("p95"), "max": scaled("max"), "count": tpt.len() },
        },
    }))
}

fn activity_doc(_: &WebState, conn: &Connection, f: &Filter) -> rusqlite::Result<Value> {
    let wd = |col: &str| {
        format!(
            "CAST(strftime('%w', {col} / 1000000000 + :tz, 'unixepoch') AS INTEGER),
             CAST(strftime('%H', {col} / 1000000000 + :tz, 'unixepoch') AS INTEGER)"
        )
    };
    let mut turns = vec![vec![0i64; 24]; 7];
    let mut cost = vec![vec![0.0f64; 24]; 7];
    let mut tokens = vec![vec![0i64; 24]; 7];
    for (w, h, n) in rows(
        conn,
        &format!(
            "SELECT {}, COUNT(*) {} GROUP BY 1, 2",
            wd("t.start_ns"),
            turn_from()
        ),
        f,
        &[],
        |r| {
            Ok((
                r.get::<_, i64>(0)? as usize,
                r.get::<_, i64>(1)? as usize,
                r.get::<_, i64>(2)?,
            ))
        },
    )? {
        turns[w % 7][h % 24] = n;
    }
    for (w, h, s) in rows(
        conn,
        &format!(
            "SELECT {}, {SUMS} {} GROUP BY 1, 2",
            wd("o.start_ns"),
            gen_from()
        ),
        f,
        &[],
        |r| {
            Ok((
                r.get::<_, i64>(0)? as usize,
                r.get::<_, i64>(1)? as usize,
                Sums::read(r, 2)?,
            ))
        },
    )? {
        cost[w % 7][h % 24] = s.cost;
        tokens[w % 7][h % 24] = s.total_tokens();
    }
    let by_hour = |m: &Vec<Vec<i64>>| {
        (0..24)
            .map(|h| m.iter().map(|r| r[h]).sum::<i64>())
            .collect::<Vec<_>>()
    };
    let by_day = |m: &Vec<Vec<i64>>| m.iter().map(|r| r.iter().sum::<i64>()).collect::<Vec<_>>();
    let cost_hour: Vec<f64> = (0..24).map(|h| cost.iter().map(|r| r[h]).sum()).collect();
    let cost_day: Vec<f64> = cost.iter().map(|r| r.iter().sum()).collect();
    Ok(json!({
        "weekdays": ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
        "turns": turns,
        "cost_usd": cost,
        "tokens": tokens,
        "turns_by_hour": by_hour(&turns),
        "turns_by_weekday": by_day(&turns),
        "cost_by_hour": cost_hour,
        "cost_by_weekday": cost_day,
        "tokens_by_hour": by_hour(&tokens),
    }))
}

async fn overview(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, overview_doc).await
}
async fn daily(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, daily_doc).await
}
async fn breakdown(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, breakdown_doc).await
}
async fn cache(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, cache_doc).await
}
async fn sessions(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, sessions_doc).await
}
async fn distributions(
    State(s): State<Arc<WebState>>,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    answer(s, body, distributions_doc).await
}
async fn activity(State(s): State<Arc<WebState>>, body: axum::body::Bytes) -> ApiResult<Value> {
    answer(s, body, activity_doc).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    #[test]
    fn the_window_defaults_to_seven_days_and_takes_relative_bounds() {
        let now = at("2026-11-09T12:00:00Z");
        let f = UsageArgs::default().resolve(now, None).unwrap();
        assert_eq!(f.until_ns - f.since_ns, 7 * DAY_NS);
        let f = UsageArgs {
            since: Some("24h".into()),
            ..Default::default()
        }
        .resolve(now, None)
        .unwrap();
        assert_eq!(f.since, "2026-11-08T12:00:00Z");
        assert!(
            UsageArgs {
                since: Some("yesterday".into()),
                ..Default::default()
            }
            .resolve(now, None)
            .is_err()
        );
        assert!(
            UsageArgs {
                since: Some("2026-11-10T00:00:00Z".into()),
                ..Default::default()
            }
            .resolve(now, None)
            .is_err()
        );
    }

    #[test]
    fn days_between_follows_the_offset() {
        let since = at("2026-11-08T23:30:00Z").unix_timestamp_nanos() as i64;
        let until = at("2026-11-09T00:30:00Z").unix_timestamp_nanos() as i64;
        assert_eq!(days_between(since, until, 0), ["2026-11-08", "2026-11-09"]);
        assert_eq!(days_between(since, until, 3600), ["2026-11-09"]);
    }

    #[test]
    fn archetypes_are_tried_in_order() {
        let s = |turns, tools, minutes: i64| SessionFacts {
            turns,
            tools,
            first_ns: Some(0),
            last_ns: Some(minutes * 60 * NS),
            ..Default::default()
        };
        assert_eq!(archetype(&s(1, 0, 1)), "quick");
        assert_eq!(archetype(&s(2, 30, 1)), "tool-heavy");
        assert_eq!(archetype(&s(5, 10, 90)), "long-running");
        assert_eq!(archetype(&s(12, 12, 20)), "conversation");
        assert_eq!(archetype(&s(5, 15, 20)), "task");
    }

    #[test]
    fn cache_savings_use_the_observed_input_price() {
        let s = Sums {
            input: 1_000_000,
            input_cost: 3.0,
            cache_read: 2_000_000,
            cache_read_cost: 0.6,
            cache_write: 1_000_000,
            cache_write_cost: 3.75,
            ..Default::default()
        };
        let v = cache_json(&s);
        assert!((v["hit_ratio"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert!((v["savings_usd"].as_f64().unwrap() - 5.4).abs() < 1e-9);
        assert!((v["write_premium_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert!((v["net_savings_usd"].as_f64().unwrap() - 4.65).abs() < 1e-9);
    }
}
