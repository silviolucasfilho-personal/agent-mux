//! The SQLite store: open (create, pragmas, migrate, seed, recover, prune),
//! idempotent upserts, and the maintenance operations the CLI exposes.
//! One `Store` = one read-write connection, owned by the writer thread;
//! readers use `open_ro`.

pub mod model;
pub mod query;
pub mod schema;
pub mod writer;

use crate::tracing::pricing::{Cost, PriceTable, cost_for};
use crate::tracing::usage::{NormalizedUsage, normalize};
use crate::transcript::Provider;
use model::{LaunchRow, ObservationRow, SessionRow, StoreOp, TraceRow, usage_raw_json};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Unix nanoseconds now, clamped to i64.
pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// A run whose heartbeat is older than this is treated as crashed by the
/// recovery sweep.
pub const STALE_RUN_NS: i64 = 120 * 1_000_000_000;

pub struct OpenOptions {
    pub prices: PriceTable,
    pub run_id: String,
    pub retention_days: u32,
    pub agent_mux_version: String,
}

pub struct Store {
    conn: Connection,
    prices: PriceTable,
    run_id: String,
    /// Rows the store rejected inside otherwise-committed batches.
    pub op_failures: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneCounts {
    pub observations: i64,
    pub traces: i64,
    pub launches: i64,
    pub sessions: i64,
    pub runs: i64,
}

fn create_private_file(path: &Path) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("create {}: {e}", path.display())),
    }
}

fn migrate(conn: &Connection) -> Result<bool, String> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version > schema::SCHEMA_VERSION {
        return Err(format!(
            "database schema v{version} is newer than this agent-mux (v{}); upgrade agent-mux",
            schema::SCHEMA_VERSION
        ));
    }
    let fresh = version == 0;
    for v in version..schema::SCHEMA_VERSION {
        let sql = schema::MIGRATIONS[v as usize];
        conn.execute_batch(&format!(
            "BEGIN;\n{sql}\nPRAGMA user_version = {};\nCOMMIT;",
            v + 1
        ))
        .map_err(|e| format!("migration to v{}: {e}", v + 1))?;
    }
    Ok(fresh)
}

/// Opens (creating if needed) the store read-write. Errors are messages
/// suitable for a status-bar notice; the caller runs untraced on `Err`.
pub fn open_rw(path: &Path, opts: OpenOptions) -> Result<Store, String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let fresh_file = !path.exists();
    if fresh_file {
        create_private_file(path)?;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    if fresh_file {
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
            .map_err(|e| e.to_string())?;
    }
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous = NORMAL;\n\
         PRAGMA foreign_keys = ON;\n\
         PRAGMA temp_store = MEMORY;",
    )
    .map_err(|e| format!("pragmas on {}: {e}", path.display()))?;
    let fresh_schema = migrate(&conn)?;
    let now = now_ns();
    let store = Store {
        conn,
        prices: opts.prices,
        run_id: opts.run_id.clone(),
        op_failures: 0,
        last_error: None,
    };
    let version = opts.agent_mux_version.as_str();
    let meta = |k: &str, v: &str| {
        store.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![k, v],
        )
    };
    if fresh_schema {
        meta("created_at", &now.to_string()).map_err(|e| e.to_string())?;
        meta("created_by_version", version).map_err(|e| e.to_string())?;
    }
    meta("schema_version", &schema::SCHEMA_VERSION.to_string()).map_err(|e| e.to_string())?;
    meta("last_opened_by_version", version).map_err(|e| e.to_string())?;
    store
        .seed_models()
        .map_err(|e| format!("seed models: {e}"))?;
    store
        .conn
        .execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET pid = excluded.pid, heartbeat_ns = excluded.heartbeat_ns, ended_ns = NULL, termination = NULL",
            params![opts.run_id, i64::from(std::process::id()), version, now],
        )
        .map_err(|e| format!("register run: {e}"))?;
    store
        .recovery_sweep(now)
        .map_err(|e| format!("recovery sweep: {e}"))?;
    if opts.retention_days > 0 {
        let cutoff = now - i64::from(opts.retention_days) * 86_400 * 1_000_000_000;
        store
            .prune(cutoff, false)
            .map_err(|e| format!("retention prune: {e}"))?;
    }
    Ok(store)
}

/// A read-only connection for the CLI and the browser. Never takes the
/// writer's lock.
pub fn open_ro(path: &Path) -> Result<Connection, String> {
    if !path.is_file() {
        return Err(format!("no trace store at {}", path.display()));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version > schema::SCHEMA_VERSION {
        return Err(format!(
            "database schema v{version} is newer than this agent-mux (v{})",
            schema::SCHEMA_VERSION
        ));
    }
    if version == 0 {
        return Err(format!(
            "{} is not an agent-mux trace store",
            path.display()
        ));
    }
    Ok(conn)
}

impl Store {
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn prices(&self) -> &PriceTable {
        &self.prices
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Replaces builtin/config price rows; hand-edited (`user`) rows win.
    fn seed_models(&self) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO models (id, provider, match, input_per_m, output_per_m, cache_read_per_m, cache_write_per_m, cache_write_1h_per_m, reasoning_per_m, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
               provider = excluded.provider, match = excluded.match,
               input_per_m = excluded.input_per_m, output_per_m = excluded.output_per_m,
               cache_read_per_m = excluded.cache_read_per_m, cache_write_per_m = excluded.cache_write_per_m,
               cache_write_1h_per_m = excluded.cache_write_1h_per_m, reasoning_per_m = excluded.reasoning_per_m,
               source = excluded.source, updated_at = excluded.updated_at
             WHERE models.source <> 'user'",
        )?;
        for m in self.prices.models() {
            let matches = serde_json::Value::from(m.matches.clone()).to_string();
            stmt.execute(params![
                m.id,
                m.provider,
                matches,
                m.input_per_m,
                m.output_per_m,
                m.cache_read_per_m,
                m.cache_write_per_m,
                m.cache_write_1h_per_m,
                m.reasoning_per_m,
                m.source.as_str(),
                m.updated_at,
            ])?;
        }
        Ok(())
    }

    /// One transaction. A rejected row is counted and skipped; the batch
    /// still commits. `Err` means the transaction itself failed.
    pub fn apply(&mut self, ops: &[StoreOp]) -> rusqlite::Result<usize> {
        let prices = &self.prices;
        let run_id = &self.run_id;
        let mut last_error = None;
        let tx = self.conn.transaction()?;
        let mut failures = 0usize;
        for op in ops {
            if let Err(e) = apply_op(&tx, op, prices) {
                failures += 1;
                last_error = Some(e.to_string());
            }
        }
        tx.execute(
            "UPDATE runs SET heartbeat_ns = ?1 WHERE id = ?2",
            params![now_ns(), run_id],
        )?;
        tx.commit()?;
        self.op_failures += failures as u64;
        if last_error.is_some() {
            self.last_error = last_error;
        }
        Ok(failures)
    }

    pub fn heartbeat(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE runs SET heartbeat_ns = ?1 WHERE id = ?2",
            params![now_ns(), self.run_id],
        )?;
        Ok(())
    }

    pub fn end_run(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE runs SET ended_ns = ?1, heartbeat_ns = ?1, termination = 'quit' WHERE id = ?2",
            params![now_ns(), self.run_id],
        )?;
        Ok(())
    }

    /// Closes what a crashed run left open. Guarded by heartbeats so a
    /// live second process is never touched.
    pub fn recovery_sweep(&self, now: i64) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE runs SET ended_ns = heartbeat_ns, termination = 'crash'
             WHERE ended_ns IS NULL AND id <> ?1 AND heartbeat_ns < ?2",
            params![self.run_id, now - STALE_RUN_NS],
        )?;
        tx.execute(
            "UPDATE launches SET termination = 'crash',
               ended_ns = COALESCE(
                 (SELECT MAX(COALESCE(o.end_ns, o.start_ns)) FROM traces t
                    JOIN observations o ON o.trace_id = t.id WHERE t.launch_id = launches.id),
                 started_ns)
             WHERE ended_ns IS NULL AND run_id IN (SELECT id FROM runs WHERE termination = 'crash')",
            [],
        )?;
        tx.execute(
            "UPDATE traces SET status = 'closed', closed_by = 'recovery',
               end_ns = COALESCE((SELECT MAX(COALESCE(end_ns, start_ns)) FROM observations WHERE trace_id = traces.id), start_ns)
             WHERE status = 'open' AND launch_id IN (SELECT id FROM launches WHERE termination = 'crash')",
            [],
        )?;
        tx.execute(
            "UPDATE observations SET end_ns = start_ns,
               status_message = COALESCE(status_message, 'no result observed (recovery)')
             WHERE end_ns IS NULL AND trace_id IN (SELECT id FROM traces WHERE closed_by = 'recovery')",
            [],
        )?;
        tx.commit()
    }

    /// Deletes everything that started before `cutoff_ns`, in FK order.
    pub fn prune(&self, cutoff_ns: i64, dry_run: bool) -> rusqlite::Result<PruneCounts> {
        let count = |sql: &str| -> rusqlite::Result<i64> {
            self.conn.query_row(sql, params![cutoff_ns], |r| r.get(0))
        };
        let counts = PruneCounts {
            observations: count(
                "SELECT COUNT(*) FROM observations WHERE trace_id IN (SELECT id FROM traces WHERE start_ns < ?1)",
            )?,
            traces: count("SELECT COUNT(*) FROM traces WHERE start_ns < ?1")?,
            launches: count(
                "SELECT COUNT(*) FROM launches WHERE started_ns < ?1 AND id NOT IN (SELECT launch_id FROM traces WHERE launch_id IS NOT NULL AND start_ns >= ?1)",
            )?,
            sessions: count(
                "SELECT COUNT(*) FROM sessions WHERE last_seen_ns < ?1 AND key NOT IN (SELECT session_key FROM traces WHERE start_ns >= ?1) AND key NOT IN (SELECT session_key FROM launches WHERE session_key IS NOT NULL AND started_ns >= ?1)",
            )?,
            runs: count(
                "SELECT COUNT(*) FROM runs WHERE started_ns < ?1 AND ended_ns IS NOT NULL AND id NOT IN (SELECT run_id FROM launches WHERE started_ns >= ?1)",
            )?,
        };
        if dry_run {
            return Ok(counts);
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM observations WHERE trace_id IN (SELECT id FROM traces WHERE start_ns < ?1)",
            params![cutoff_ns],
        )?;
        tx.execute("DELETE FROM traces WHERE start_ns < ?1", params![cutoff_ns])?;
        tx.execute(
            "DELETE FROM launches WHERE started_ns < ?1 AND id NOT IN (SELECT launch_id FROM traces WHERE launch_id IS NOT NULL)",
            params![cutoff_ns],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE last_seen_ns < ?1 AND key NOT IN (SELECT session_key FROM traces) AND key NOT IN (SELECT session_key FROM launches WHERE session_key IS NOT NULL)",
            params![cutoff_ns],
        )?;
        tx.execute(
            "DELETE FROM runs WHERE started_ns < ?1 AND ended_ns IS NOT NULL AND id NOT IN (SELECT run_id FROM launches)",
            params![cutoff_ns],
        )?;
        tx.commit()?;
        let _ = self.conn.execute_batch("PRAGMA incremental_vacuum;");
        Ok(counts)
    }

    /// Recomputes every observation's normalized usage and cost from its
    /// raw usage and the current price table. Returns (changed, unpriced).
    pub fn recost(&mut self) -> rusqlite::Result<(usize, usize)> {
        struct Row {
            rid: i64,
            model: Option<String>,
            usage: String,
            provider: String,
            total_cost: Option<f64>,
        }
        let rows: Vec<Row> = {
            let mut stmt = self.conn.prepare(
                "SELECT o.rid, o.model, o.usage, s.provider, o.total_cost_usd
                 FROM observations o JOIN traces t ON t.id = o.trace_id JOIN sessions s ON s.key = t.session_key
                 WHERE o.usage IS NOT NULL",
            )?;
            let mapped = stmt.query_map([], |r| {
                Ok(Row {
                    rid: r.get(0)?,
                    model: r.get(1)?,
                    usage: r.get(2)?,
                    provider: r.get(3)?,
                    total_cost: r.get(4)?,
                })
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = 0usize;
        let mut unpriced = 0usize;
        let tx = self.conn.transaction()?;
        for row in rows {
            let raw: Vec<(String, i64)> =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&row.usage)
                    .map(|m| {
                        m.into_iter()
                            .filter_map(|(k, v)| v.as_i64().map(|n| (k, n)))
                            .collect()
                    })
                    .unwrap_or_default();
            let provider = match row.provider.as_str() {
                "codex" => Provider::Codex,
                "antigravity" => Provider::Antigravity,
                _ => Provider::Claude,
            };
            let usage = normalize(provider, &raw);
            let (model_id, cost) = price(&self.prices, row.model.as_deref(), &usage);
            if model_id.is_none() && !usage.is_empty() {
                unpriced += 1;
            }
            let differs = match (row.total_cost, cost.total) {
                (None, None) => false,
                (Some(a), Some(b)) => (a - b).abs() > 1e-12,
                _ => true,
            };
            if differs {
                changed += 1;
            }
            tx.execute(
                "UPDATE observations SET model_id = ?2, input_tokens = ?3, output_tokens = ?4, cache_read_tokens = ?5,
                   cache_write_tokens = ?6, cache_write_1h_tokens = ?7, reasoning_tokens = ?8, total_tokens = ?9,
                   input_cost_usd = ?10, output_cost_usd = ?11, cache_read_cost_usd = ?12, cache_write_cost_usd = ?13, total_cost_usd = ?14
                 WHERE rid = ?1",
                params![
                    row.rid,
                    model_id,
                    usage.input,
                    usage.output,
                    usage.cache_read,
                    usage.cache_write,
                    usage.cache_write_1h,
                    usage.reasoning,
                    usage.total,
                    cost.input,
                    cost.output,
                    cost.cache_read,
                    cost.cache_write,
                    cost.total,
                ],
            )?;
        }
        tx.commit()?;
        Ok((changed, unpriced))
    }
}

fn price(
    prices: &PriceTable,
    model: Option<&str>,
    usage: &NormalizedUsage,
) -> (Option<String>, Cost) {
    if usage.is_empty() {
        return (None, Cost::default());
    }
    match model.and_then(|m| prices.find(m)) {
        Some(p) => (Some(p.id.clone()), cost_for(p, usage)),
        None => (None, Cost::default()),
    }
}

fn apply_op(tx: &Transaction, op: &StoreOp, prices: &PriceTable) -> rusqlite::Result<()> {
    match op {
        StoreOp::Launch(l) => upsert_launch(tx, l),
        StoreOp::Session(s) => upsert_session(tx, s),
        StoreOp::Trace(t) => upsert_trace(tx, t),
        StoreOp::Observation(o) => upsert_observation(tx, o, prices),
    }
}

fn ensure_session(tx: &Transaction, key: &str, seen_ns: i64) -> rusqlite::Result<()> {
    // the session row must exist (FK); a minimal one is enough — the
    // pipeline's own Session op fills in the rest whenever it arrives
    let (provider, session_id) = key.split_once(':').unwrap_or(("claude", key));
    tx.prepare_cached(
        "INSERT OR IGNORE INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns) VALUES (?1, ?2, ?3, ?4, ?4)",
    )?
    .execute(params![key, provider, session_id, seen_ns])?;
    Ok(())
}

fn upsert_launch(tx: &Transaction, l: &LaunchRow) -> rusqlite::Result<()> {
    if let Some(key) = &l.session_key {
        ensure_session(tx, key, l.started_ns)?;
    }
    let tags = serde_json::Value::from(l.tags.clone()).to_string();
    tx.prepare_cached(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode,
           correlation_plan, correlation, session_key, injected_session_id, attached, started_ns, ended_ns, termination,
           exit_code, parse_errors, dropped_ops, reported_cost_usd, reported_lines_added, reported_lines_removed,
           agent_mux_version, user_id, release, environment, tags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, COALESCE(?18, 0), COALESCE(?19, 0),
           ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
         ON CONFLICT(id) DO UPDATE SET
           correlation = COALESCE(excluded.correlation, correlation),
           session_key = COALESCE(excluded.session_key, session_key),
           attached = MAX(attached, excluded.attached),
           ended_ns = COALESCE(excluded.ended_ns, ended_ns),
           termination = COALESCE(excluded.termination, termination),
           exit_code = COALESCE(excluded.exit_code, exit_code),
           parse_errors = MAX(parse_errors, excluded.parse_errors),
           dropped_ops = MAX(dropped_ops, excluded.dropped_ops),
           reported_cost_usd = COALESCE(excluded.reported_cost_usd, reported_cost_usd),
           reported_lines_added = COALESCE(excluded.reported_lines_added, reported_lines_added),
           reported_lines_removed = COALESCE(excluded.reported_lines_removed, reported_lines_removed)",
    )?
    .execute(params![
        l.id,
        l.run_id,
        l.agent_mux_session,
        l.profile,
        l.provider,
        l.cwd,
        l.project_slug,
        l.content_mode,
        l.correlation_plan,
        l.correlation,
        l.session_key,
        l.injected_session_id,
        l.attached,
        l.started_ns,
        l.ended_ns,
        l.termination,
        l.exit_code,
        l.parse_errors,
        l.dropped_ops,
        l.reported_cost_usd,
        l.reported_lines_added,
        l.reported_lines_removed,
        l.agent_mux_version,
        l.user_id,
        l.release,
        l.environment,
        tags,
    ])?;
    Ok(())
}

fn upsert_session(tx: &Transaction, s: &SessionRow) -> rusqlite::Result<()> {
    let extra = s.extra.as_ref().map(|v| v.to_string());
    tx.prepare_cached(
        "INSERT INTO sessions (key, provider, session_id, user_id, cwd, project_slug, transcript_path, title, first_seen_ns, last_seen_ns, extra)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, COALESCE(?10, '{}'))
         ON CONFLICT(key) DO UPDATE SET
           user_id = COALESCE(excluded.user_id, user_id),
           cwd = COALESCE(excluded.cwd, cwd),
           project_slug = COALESCE(excluded.project_slug, project_slug),
           transcript_path = COALESCE(excluded.transcript_path, transcript_path),
           title = COALESCE(title, excluded.title),
           first_seen_ns = MIN(first_seen_ns, excluded.first_seen_ns),
           last_seen_ns = MAX(last_seen_ns, excluded.last_seen_ns),
           extra = json_patch(extra, excluded.extra)",
    )?
    .execute(params![
        s.key,
        s.provider,
        s.session_id,
        s.user_id,
        s.cwd,
        s.project_slug,
        s.transcript_path,
        s.title,
        s.seen_ns,
        extra,
    ])?;
    Ok(())
}

fn upsert_trace(tx: &Transaction, t: &TraceRow) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns) VALUES (?1, ?2, ?3, ?4, ?4)",
    )?
    .execute(params![t.session_key, t.provider, t.session_id, t.start_ns])?;
    tx.prepare_cached("UPDATE sessions SET last_seen_ns = MAX(last_seen_ns, ?2) WHERE key = ?1")?
        .execute(params![t.session_key, t.end_ns.unwrap_or(t.start_ns)])?;
    let skills = t
        .skills
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::Value::from(s.clone()).to_string());
    let metadata = t.metadata.as_ref().map(|v| v.to_string());
    tx.prepare_cached(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, input, output, thinking,
           skills, reported_duration_ms, reported_message_count, session_cost_usd, timing_approx, ordinal_salted, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, COALESCE(?12, '[]'), ?13, ?14, ?15, ?16, ?17, COALESCE(?18, '{}'))
         ON CONFLICT(id) DO UPDATE SET
           launch_id = COALESCE(excluded.launch_id, launch_id),
           name = excluded.name,
           status = excluded.status,
           start_ns = excluded.start_ns,
           end_ns = COALESCE(excluded.end_ns, end_ns),
           input = COALESCE(excluded.input, input),
           output = COALESCE(excluded.output, output),
           thinking = COALESCE(excluded.thinking, thinking),
           skills = CASE WHEN excluded.skills = '[]' THEN skills ELSE excluded.skills END,
           reported_duration_ms = COALESCE(excluded.reported_duration_ms, reported_duration_ms),
           reported_message_count = COALESCE(excluded.reported_message_count, reported_message_count),
           session_cost_usd = COALESCE(excluded.session_cost_usd, session_cost_usd),
           timing_approx = MAX(timing_approx, excluded.timing_approx),
           ordinal_salted = excluded.ordinal_salted,
           closed_by = CASE WHEN excluded.status = 'open' THEN closed_by ELSE NULL END,
           metadata = json_patch(metadata, excluded.metadata)",
    )?
    .execute(params![
        t.id,
        t.session_key,
        t.launch_id,
        t.ordinal,
        t.name,
        t.status.as_str(),
        t.start_ns,
        t.end_ns,
        t.input,
        t.output,
        t.thinking,
        skills,
        t.reported_duration_ms,
        t.reported_message_count,
        t.session_cost_usd,
        t.timing_approx,
        t.ordinal_salted,
        metadata,
    ])?;
    Ok(())
}

fn upsert_observation(
    tx: &Transaction,
    o: &ObservationRow,
    prices: &PriceTable,
) -> rusqlite::Result<()> {
    let usage = o.usage.clone().unwrap_or_default();
    let (model_id, cost) = price(prices, o.model.as_deref(), &usage);
    let usage_json = o.usage_raw.as_ref().map(|raw| usage_raw_json(raw));
    let metadata = if o.metadata.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(o.metadata.clone()).to_string())
    };
    tx.prepare_cached(
        "INSERT INTO observations (id, trace_id, parent_id, type, name, kind, start_ns, end_ns, level, status_message, model, model_id,
           input, output, thinking, usage, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_write_1h_tokens,
           reasoning_tokens, total_tokens, input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_write_cost_usd, total_cost_usd,
           tool_id, tool_name, skill, mcp_server, path, is_error, ts_approx, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
           ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, COALESCE(?36, '{}'))
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           kind = COALESCE(excluded.kind, kind),
           start_ns = excluded.start_ns,
           end_ns = COALESCE(excluded.end_ns, end_ns),
           level = excluded.level,
           status_message = COALESCE(excluded.status_message, status_message),
           model = COALESCE(excluded.model, model),
           model_id = COALESCE(excluded.model_id, model_id),
           input = COALESCE(excluded.input, input),
           output = COALESCE(excluded.output, output),
           thinking = COALESCE(excluded.thinking, thinking),
           usage = COALESCE(excluded.usage, usage),
           input_tokens = COALESCE(excluded.input_tokens, input_tokens),
           output_tokens = COALESCE(excluded.output_tokens, output_tokens),
           cache_read_tokens = COALESCE(excluded.cache_read_tokens, cache_read_tokens),
           cache_write_tokens = COALESCE(excluded.cache_write_tokens, cache_write_tokens),
           cache_write_1h_tokens = COALESCE(excluded.cache_write_1h_tokens, cache_write_1h_tokens),
           reasoning_tokens = COALESCE(excluded.reasoning_tokens, reasoning_tokens),
           total_tokens = COALESCE(excluded.total_tokens, total_tokens),
           input_cost_usd = COALESCE(excluded.input_cost_usd, input_cost_usd),
           output_cost_usd = COALESCE(excluded.output_cost_usd, output_cost_usd),
           cache_read_cost_usd = COALESCE(excluded.cache_read_cost_usd, cache_read_cost_usd),
           cache_write_cost_usd = COALESCE(excluded.cache_write_cost_usd, cache_write_cost_usd),
           total_cost_usd = COALESCE(excluded.total_cost_usd, total_cost_usd),
           tool_id = COALESCE(excluded.tool_id, tool_id),
           tool_name = COALESCE(excluded.tool_name, tool_name),
           skill = COALESCE(excluded.skill, skill),
           mcp_server = COALESCE(excluded.mcp_server, mcp_server),
           path = COALESCE(excluded.path, path),
           is_error = MAX(is_error, excluded.is_error),
           ts_approx = MAX(ts_approx, excluded.ts_approx),
           metadata = json_patch(metadata, excluded.metadata)",
    )?
    .execute(params![
        o.id,
        o.trace_id,
        o.parent_id,
        o.obs_type.as_str(),
        o.name,
        o.kind,
        o.start_ns,
        o.end_ns,
        o.level.as_str(),
        o.status_message,
        o.model,
        model_id,
        o.input,
        o.output,
        o.thinking,
        usage_json,
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.cache_write_1h,
        usage.reasoning,
        usage.total,
        cost.input,
        cost.output,
        cost.cache_read,
        cost.cache_write,
        cost.total,
        o.tool_id,
        o.tool_name,
        o.skill,
        o.mcp_server,
        o.path,
        o.is_error,
        o.ts_approx,
        metadata,
    ])?;
    Ok(())
}

/// Reads one meta value.
pub fn meta_value(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
        r.get(0)
    })
    .optional()
}

#[cfg(test)]
mod tests {
    use super::model::*;
    use super::*;
    use crate::tracing::usage::NormalizedUsage;

    fn open_temp(dir: &std::path::Path) -> Store {
        open_rw(
            &dir.join("t.db"),
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-1".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap()
    }

    fn launch(id: &str) -> LaunchRow {
        LaunchRow {
            id: id.into(),
            run_id: "run-1".into(),
            agent_mux_session: 0,
            profile: "Claude Code".into(),
            provider: "claude".into(),
            cwd: "/proj".into(),
            project_slug: "-proj".into(),
            content_mode: "full".into(),
            correlation_plan: "deterministic".into(),
            correlation: None,
            session_key: None,
            injected_session_id: true,
            attached: false,
            started_ns: 1_000,
            ended_ns: None,
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: Some("me".into()),
            release: None,
            environment: None,
            tags: vec!["agent-mux".into(), "claude".into()],
        }
    }

    fn trace(id: &str, status: TraceStatus, end: Option<i64>) -> TraceRow {
        TraceRow {
            id: id.into(),
            session_key: "claude:s1".into(),
            provider: "claude".into(),
            session_id: "s1".into(),
            launch_id: Some("l1".into()),
            ordinal: 1,
            name: "turn 1".into(),
            status,
            start_ns: 1_000,
            end_ns: end,
            input: Some("hello".into()),
            output: None,
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            ordinal_salted: false,
            metadata: None,
        }
    }

    fn observation(id: &str, end: Option<i64>) -> ObservationRow {
        ObservationRow {
            id: id.into(),
            trace_id: "t1".into(),
            parent_id: None,
            obs_type: ObservationType::Generation,
            name: "assistant".into(),
            kind: None,
            start_ns: 1_100,
            end_ns: end,
            level: Level::Default,
            status_message: None,
            model: Some("claude-sonnet-4-5-20250929".into()),
            input: None,
            output: Some("hi".into()),
            thinking: None,
            usage_raw: Some(vec![
                ("input_tokens".into(), 1_000_000),
                ("output_tokens".into(), 100_000),
            ]),
            usage: Some(NormalizedUsage {
                input: Some(1_000_000),
                output: Some(100_000),
                total: Some(1_100_000),
                ..Default::default()
            }),
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            is_error: false,
            ts_approx: false,
            metadata: serde_json::Map::new(),
        }
    }

    #[test]
    fn fresh_store_has_schema_pragmas_and_seeded_models() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_temp(dir.path());
        let version: i32 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
        let mode: String = store
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i32 = store
            .conn()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let models: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM models", [], |r| r.get(0))
            .unwrap();
        assert!(models >= 10);
        assert_eq!(
            meta_value(store.conn(), "schema_version")
                .unwrap()
                .as_deref(),
            Some("1")
        );
        let runs: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM runs WHERE id = 'run-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(runs, 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("t.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn newer_schema_is_refused_and_reopen_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = open_temp(dir.path());
            store.apply(&[StoreOp::Launch(launch("l1"))]).unwrap();
        }
        // reopen: same data, no re-migration
        {
            let store = open_temp(dir.path());
            let n: i64 = store
                .conn()
                .query_row("SELECT COUNT(*) FROM launches", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
        }
        let conn = Connection::open(dir.path().join("t.db")).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        drop(conn);
        let err = open_rw(
            &dir.path().join("t.db"),
            OpenOptions {
                prices: PriceTable::empty(),
                run_id: "run-2".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .err()
        .unwrap();
        assert!(err.contains("newer"), "{err}");
        assert!(open_ro(&dir.path().join("t.db")).is_err());
    }

    #[test]
    fn upserts_are_idempotent_and_never_erase() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_temp(dir.path());
        let ops = vec![
            StoreOp::Launch(launch("l1")),
            StoreOp::Trace(trace("t1", TraceStatus::Open, None)),
            StoreOp::Observation(observation("o1", None)),
        ];
        assert_eq!(store.apply(&ops).unwrap(), 0);
        assert_eq!(store.apply(&ops).unwrap(), 0);
        let count = |sql: &str| -> i64 { store.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM traces"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM observations"), 1);
        assert_eq!(
            count("SELECT COUNT(*) FROM sessions"),
            1,
            "minimal session row auto-created"
        );
        // priced at write time
        let cost: f64 = store
            .conn()
            .query_row(
                "SELECT total_cost_usd FROM observations WHERE id = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((cost - 4.5).abs() < 1e-9, "{cost}");
        let model_id: String = store
            .conn()
            .query_row(
                "SELECT model_id FROM observations WHERE id = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model_id, "claude-sonnet-4-5");
        // close the turn with LESS information: nothing erased, end set
        let mut closed = trace("t1", TraceStatus::Closed, Some(5_000));
        closed.input = None;
        closed.output = Some("done".into());
        store.apply(&[StoreOp::Trace(closed)]).unwrap();
        let (status, input, output, end): (String, Option<String>, Option<String>, Option<i64>) =
            store
                .conn()
                .query_row(
                    "SELECT status, input, output, end_ns FROM traces WHERE id = 't1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
        assert_eq!(status, "closed");
        assert_eq!(input.as_deref(), Some("hello"));
        assert_eq!(output.as_deref(), Some("done"));
        assert_eq!(end, Some(5_000));
        // launch end update keeps start facts
        let mut ended = launch("l1");
        ended.ended_ns = Some(9_000);
        ended.termination = Some("exit".into());
        ended.exit_code = Some(0);
        ended.session_key = Some("claude:s1".into());
        store.apply(&[StoreOp::Launch(ended)]).unwrap();
        let (term, code, key): (String, i64, String) = store
            .conn()
            .query_row(
                "SELECT termination, exit_code, session_key FROM launches WHERE id = 'l1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (term.as_str(), code, key.as_str()),
            ("exit", 0, "claude:s1")
        );
        // session title: first write wins
        let session = |title: &str| {
            StoreOp::Session(SessionRow {
                key: "claude:s1".into(),
                provider: "claude".into(),
                session_id: "s1".into(),
                user_id: None,
                cwd: Some("/proj".into()),
                project_slug: None,
                transcript_path: None,
                title: Some(title.into()),
                seen_ns: 2_000,
                extra: Some(serde_json::json!({"model": "m"})),
            })
        };
        store.apply(&[session("first"), session("second")]).unwrap();
        let (title, extra): (String, String) = store
            .conn()
            .query_row(
                "SELECT title, extra FROM sessions WHERE key = 'claude:s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "first");
        assert!(extra.contains("\"model\":\"m\""));
        // views
        let (turns, total_cost): (i64, f64) = store
            .conn()
            .query_row(
                "SELECT turn_count, total_cost_usd FROM session_stats WHERE key = 'claude:s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(turns, 1);
        assert!((total_cost - 4.5).abs() < 1e-9);
        // FTS sees the content
        let hits: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM traces_fts WHERE traces_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn rejected_rows_are_counted_but_the_batch_commits() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_temp(dir.path());
        // observation for a trace that does not exist violates the FK
        let ops = vec![
            StoreOp::Launch(launch("l1")),
            StoreOp::Observation(observation("orphan", Some(2_000))),
        ];
        assert_eq!(store.apply(&ops).unwrap(), 1);
        assert_eq!(store.op_failures, 1);
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM launches", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn recovery_sweep_closes_only_stale_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let mut store = open_temp(dir.path());
            store
                .apply(&[
                    StoreOp::Launch(launch("l1")),
                    StoreOp::Trace(trace("t1", TraceStatus::Open, None)),
                    StoreOp::Observation(observation("o1", None)),
                ])
                .unwrap();
            // simulate a crash: no end_run
        }
        // a second process opening right away: run-1 still "alive" (fresh heartbeat)
        let opts = |run: &str| OpenOptions {
            prices: PriceTable::empty(),
            run_id: run.into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        };
        let store = open_rw(&path, opts("run-2")).unwrap();
        let status: String = store
            .conn()
            .query_row("SELECT status FROM traces WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "open", "fresh heartbeat must not be swept");
        drop(store);
        // age the heartbeat past the staleness window
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE runs SET heartbeat_ns = heartbeat_ns - ?1 WHERE id = 'run-1'",
            params![STALE_RUN_NS * 2],
        )
        .unwrap();
        drop(conn);
        let store = open_rw(&path, opts("run-3")).unwrap();
        let (status, closed_by): (String, Option<String>) = store
            .conn()
            .query_row(
                "SELECT status, closed_by FROM traces WHERE id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "closed");
        assert_eq!(closed_by.as_deref(), Some("recovery"));
        let term: String = store
            .conn()
            .query_row(
                "SELECT termination FROM launches WHERE id = 'l1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(term, "crash");
        let end: Option<i64> = store
            .conn()
            .query_row("SELECT end_ns FROM observations WHERE id = 'o1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(end.is_some());
    }

    #[test]
    fn prune_cascades_and_recost_recomputes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open_temp(dir.path());
        store
            .apply(&[
                StoreOp::Launch(launch("l1")),
                StoreOp::Trace(trace("t1", TraceStatus::Closed, Some(2_000))),
                StoreOp::Observation(observation("o1", Some(2_000))),
            ])
            .unwrap();
        let dry = store.prune(10_000, true).unwrap();
        assert_eq!((dry.traces, dry.observations), (1, 1));
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM traces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "dry run deletes nothing");
        // recost against an empty table: everything becomes unpriced
        store.prices = PriceTable::empty();
        let (changed, unpriced) = store.recost().unwrap();
        assert_eq!((changed, unpriced), (1, 1));
        let cost: Option<f64> = store
            .conn()
            .query_row(
                "SELECT total_cost_usd FROM observations WHERE id = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(cost.is_none());
        let counts = store.prune(10_000, false).unwrap();
        assert_eq!(counts.traces, 1);
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
