//! The Langfuse sink: one blocking exporter OS thread that consumes
//! `StoreOp` rows from a bounded queue, maps them to OTLP spans, batches
//! them, and POSTs to `{host}/api/public/otel/v1/traces`.
//!
//! OTLP is the path that works across Langfuse deployments: a v4 server
//! running `LANGFUSE_MIGRATION_V4_WRITE_MODE=events_only` accepts only
//! score and log events on `/api/public/ingestion`, while OTLP is
//! converted server-side into the same trace and observation events on
//! every version since 3.22.
//!
//! Failure policy (the invariant: tracing can never break, block, or slow
//! a session): the queue is try_send-only (full ⇒ drop + count); per-batch
//! retries are bounded; 429 honors Retry-After (capped, twice); two
//! consecutive 401/403 disable the exporter for the run; five consecutive
//! failed batches open a circuit breaker (60 s, then one half-open probe);
//! the final drain on shutdown is skipped entirely while the breaker is
//! open. Being an OS thread, the post-`kill_all` flush has a join point
//! that survives tokio runtime teardown.

pub mod map;

use crate::config::ResolvedLangfuse;
use crate::tracing::store::model::{ObservationRow, StoreOp, TraceRow, TraceStatus};
use crate::tracing::store::query::LaunchStats;
use map::{Batch, Event, MapCtx};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

/// RFC 4648 standard base64 with padding, encode-only.
pub fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn basic_auth(public_key: &str, secret_key: &str) -> String {
    format!(
        "Basic {}",
        base64_encode(format!("{public_key}:{secret_key}").as_bytes())
    )
}

/// Everything the exporter thread needs. Timings are configurable so tests
/// can shrink them; production values come from `ExporterConfig::new`.
#[derive(Debug, Clone)]
pub struct ExporterConfig {
    /// Full endpoint: `{host}/api/public/otel/v1/traces`.
    pub endpoint: String,
    /// `Basic base64(pk:sk)`.
    pub auth: String,
    pub flush_interval: Duration,
    pub max_batch_events: usize,
    pub max_batch_bytes: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub attempts: u32,
    pub backoff_base: Duration,
    pub retry_after_cap: Duration,
    pub max_rate_limit_waits: u32,
    pub breaker_threshold: u32,
    pub breaker_open: Duration,
}

impl ExporterConfig {
    pub fn new(lf: &ResolvedLangfuse) -> Self {
        ExporterConfig::with_endpoint(
            &lf.host,
            &lf.public_key,
            &lf.secret_key,
            lf.flush_interval_ms,
        )
    }

    pub fn with_endpoint(
        host: &str,
        public_key: &str,
        secret_key: &str,
        flush_interval_ms: u64,
    ) -> Self {
        ExporterConfig {
            endpoint: format!("{host}/api/public/otel/v1/traces"),
            auth: basic_auth(public_key, secret_key),
            flush_interval: Duration::from_millis(flush_interval_ms.max(1)),
            max_batch_events: 256,
            max_batch_bytes: 1024 * 1024,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(15),
            attempts: 3,
            backoff_base: Duration::from_secs(1),
            retry_after_cap: Duration::from_secs(60),
            max_rate_limit_waits: 2,
            breaker_threshold: 5,
            breaker_open: Duration::from_secs(60),
        }
    }
}

/// Bounded queue capacity: beyond this, ops drop (counted).
pub const QUEUE_CAPACITY: usize = 4096;

pub type StatusSink = Box<dyn Fn(&'static str, String) + Send>;
/// Called after every flushed batch with the launches it touched.
pub type StatsSink = Box<dyn FnMut(&str, LaunchStats) + Send>;

pub struct ExporterHandle {
    /// Clone into pipelines; `try_send` only.
    pub tx: SyncSender<StoreOp>,
    pub dropped: Arc<AtomicU64>,
    done_rx: Receiver<()>,
}

impl ExporterHandle {
    /// Drops this handle's sender and waits (bounded) for the thread to
    /// drain and finish. All pipeline-held clones of `tx` must be gone
    /// first or the wait just times out — quitting is never blocked.
    pub fn finish(self, deadline: Duration) -> bool {
        let ExporterHandle { tx, done_rx, .. } = self;
        drop(tx);
        done_rx.recv_timeout(deadline).is_ok()
    }
}

#[derive(Debug, PartialEq)]
pub enum PostOutcome {
    /// Accepted; carries the server's rejection notes, if any.
    Success(Vec<String>),
    Retryable,
    RateLimited(Duration),
    AuthFailure,
    ClientError(u16),
}

enum BreakerState {
    Closed {
        consecutive: u32,
    },
    Open {
        until: Instant,
    },
    /// One probe is in flight; its outcome decides Closed vs re-Open.
    HalfOpen,
}

struct Breaker {
    state: BreakerState,
    threshold: u32,
    open_for: Duration,
}

impl Breaker {
    fn new(threshold: u32, open_for: Duration) -> Self {
        Breaker {
            state: BreakerState::Closed { consecutive: 0 },
            threshold,
            open_for,
        }
    }

    /// True while open (drop instead of attempting). Once the window has
    /// passed, transitions to half-open and lets exactly one batch through.
    fn is_open(&mut self, now: Instant) -> bool {
        match self.state {
            BreakerState::Open { until } if now < until => true,
            BreakerState::Open { .. } => {
                self.state = BreakerState::HalfOpen;
                false
            }
            _ => false,
        }
    }

    fn record_success(&mut self) {
        self.state = BreakerState::Closed { consecutive: 0 };
    }

    /// Returns true when this failure (re)opened the breaker.
    fn record_failure(&mut self, now: Instant) -> bool {
        match self.state {
            BreakerState::HalfOpen | BreakerState::Open { .. } => {
                self.state = BreakerState::Open {
                    until: now + self.open_for,
                };
                true
            }
            BreakerState::Closed { consecutive } => {
                let consecutive = consecutive + 1;
                if consecutive >= self.threshold {
                    self.state = BreakerState::Open {
                        until: now + self.open_for,
                    };
                    true
                } else {
                    self.state = BreakerState::Closed { consecutive };
                    false
                }
            }
        }
    }
}

fn agent(connect: Duration, request: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(connect))
        .timeout_global(Some(request))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// Rejections the server reports on an accepted export: OTLP's
/// `{"partialSuccess":{"rejectedSpans":N,"errorMessage":"…"}}` (proto3
/// JSON may send the count as a string), or a plain `{"error":"…"}` some
/// Langfuse builds answer with. Returns (rejected spans, notes).
fn rejections(body: &str) -> (u64, Vec<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return (0, Vec::new());
    };
    if let Some(error) = v.get("error").and_then(|e| e.as_str()) {
        return (0, vec![format!("langfuse: {error}")]);
    }
    let Some(partial) = v.get("partialSuccess").filter(|p| p.is_object()) else {
        return (0, Vec::new());
    };
    let rejected = partial
        .get("rejectedSpans")
        .map(|r| match r {
            serde_json::Value::String(s) => s.parse::<u64>().unwrap_or(0),
            other => other.as_u64().unwrap_or(0),
        })
        .unwrap_or(0);
    let message = partial
        .get("errorMessage")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if rejected == 0 && message.is_empty() {
        return (0, Vec::new());
    }
    let detail = if message.is_empty() {
        String::new()
    } else {
        format!(" ({message})")
    };
    (
        rejected,
        vec![format!("langfuse: {rejected} span(s) rejected{detail}")],
    )
}

pub fn post_batch(agent: &ureq::Agent, cfg: &ExporterConfig, body: &str) -> PostOutcome {
    let result = agent
        .post(&cfg.endpoint)
        .header("Authorization", &cfg.auth)
        .header("Content-Type", "application/json")
        .header("x-langfuse-sdk-name", "agent-mux")
        .header("x-langfuse-sdk-version", env!("CARGO_PKG_VERSION"))
        .header(
            "User-Agent",
            concat!("agent-mux/", env!("CARGO_PKG_VERSION")),
        )
        .send(body.as_bytes());
    match result {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            match status {
                200..=299 => {
                    let text = resp.body_mut().read_to_string().unwrap_or_default();
                    PostOutcome::Success(rejections(&text).1)
                }
                401 | 403 => PostOutcome::AuthFailure,
                429 => {
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(Duration::from_secs(5));
                    PostOutcome::RateLimited(retry_after.min(cfg.retry_after_cap))
                }
                500..=599 => PostOutcome::Retryable,
                other => PostOutcome::ClientError(other),
            }
        }
        Err(_) => PostOutcome::Retryable,
    }
}

/// ±25% deterministic-ish jitter.
fn jitter(base: Duration, salt: u64) -> Duration {
    let nanos = base.as_nanos() as u64;
    let mut x = nanos ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    let quarter = nanos / 4;
    if quarter == 0 {
        return base;
    }
    let offset = x % (quarter * 2);
    Duration::from_nanos(nanos - quarter + offset)
}

/// Per-launch rollup kept in memory: the same numbers the store's
/// `launch_stats` view yields, for the status-bar badge.
#[derive(Default)]
struct LaunchAgg {
    closed_turns: HashSet<String>,
    tokens: HashMap<String, i64>,
    cost: HashMap<String, f64>,
    running_tools: Vec<(String, String)>, // (observation id, name)
}

impl LaunchAgg {
    fn stats(&self) -> LaunchStats {
        let total_tokens: i64 = self.tokens.values().sum();
        let cost_usd: f64 = self.cost.values().sum();
        LaunchStats {
            turns: self.closed_turns.len() as i64,
            total_tokens: (total_tokens > 0).then_some(total_tokens),
            cost_usd: (cost_usd > 0.0).then_some(cost_usd),
            running_tool: self.running_tools.last().map(|(_, n)| n.clone()),
        }
    }
}

struct ExporterState {
    cfg: ExporterConfig,
    agent: ureq::Agent,
    breaker: Breaker,
    auth_failures: u32,
    disabled: bool,
    status: StatusSink,
    stats: StatsSink,
    dropped: Arc<AtomicU64>,
    warned: HashSet<&'static str>,
    ctx: MapCtx,
    /// trace id → launch id, so observation rows find their launch.
    trace_launch: HashMap<String, String>,
    aggs: HashMap<String, LaunchAgg>,
}

impl ExporterState {
    /// Once-per-class status line to the UI.
    fn note(&mut self, class: &'static str, message: String) {
        if self.warned.insert(class) {
            (self.status)(class, message);
        }
    }

    fn drop_events(&mut self, count: usize) {
        self.dropped.fetch_add(count as u64, Ordering::Relaxed);
        self.note(
            "dropped",
            "langfuse: some trace rows were dropped (network/backpressure)".to_string(),
        );
    }

    fn aggregate_trace(&mut self, t: &TraceRow) -> Option<String> {
        let launch_id = t.launch_id.clone()?;
        self.trace_launch.insert(t.id.clone(), launch_id.clone());
        let agg = self.aggs.entry(launch_id.clone()).or_default();
        if t.status != TraceStatus::Open {
            agg.closed_turns.insert(t.id.clone());
        }
        Some(launch_id)
    }

    fn aggregate_observation(&mut self, o: &ObservationRow) -> Option<String> {
        let launch_id = self.trace_launch.get(&o.trace_id)?.clone();
        let cost = map::row_cost(o, &self.ctx.prices);
        let agg = self.aggs.entry(launch_id.clone()).or_default();
        if let Some(total) = o.usage.as_ref().and_then(|u| u.total) {
            agg.tokens.insert(o.id.clone(), total);
        }
        if let Some(c) = cost {
            agg.cost.insert(o.id.clone(), c);
        }
        agg.running_tools.retain(|(id, _)| id != &o.id);
        if o.end_ns.is_none() && o.tool_id.is_some() {
            agg.running_tools.push((o.id.clone(), o.name.clone()));
        }
        Some(launch_id)
    }

    /// Maps an op, updating the launch rollups. Returns the event and the
    /// launch it belongs to.
    fn accept(&mut self, op: &StoreOp) -> Option<(Event, Option<String>)> {
        let launch = match op {
            StoreOp::Trace(t) => self.aggregate_trace(t),
            StoreOp::Observation(o) => self.aggregate_observation(o),
            _ => None,
        };
        let mut event = map::event_for(op, &self.ctx)?;
        if event.launch_id.is_none() {
            event.launch_id = launch.clone();
        }
        Some((event, launch))
    }

    fn report_stats(&mut self, launches: &HashSet<String>) {
        for id in launches {
            if let Some(agg) = self.aggs.get(id) {
                let stats = agg.stats();
                (self.stats)(id, stats);
            }
        }
    }

    /// Sends one batch with the full retry/breaker policy. The batch is
    /// consumed either way — the queue is a conduit, not a durable buffer.
    fn flush(&mut self, batch: Batch) {
        if batch.is_empty() {
            return;
        }
        let launches: HashSet<String> = batch
            .events
            .iter()
            .filter_map(|e| e.launch_id.clone())
            .collect();
        if self.disabled || self.breaker.is_open(Instant::now()) {
            self.drop_events(batch.len());
            return;
        }
        let body = batch.body();
        let mut rate_limit_waits = 0u32;
        let mut attempt = 0u32;
        loop {
            match post_batch(&self.agent, &self.cfg, &body) {
                PostOutcome::Success(notes) => {
                    self.auth_failures = 0;
                    self.breaker.record_success();
                    if let Some(first) = notes.first() {
                        // accepted, but the server kept fewer spans than
                        // the batch carried
                        self.dropped
                            .fetch_add(batch.len() as u64, Ordering::Relaxed);
                        self.note("rejected", first.clone());
                    }
                    self.report_stats(&launches);
                    return;
                }
                PostOutcome::AuthFailure => {
                    self.auth_failures += 1;
                    if self.auth_failures >= 2 {
                        self.disabled = true;
                        self.note(
                            "auth",
                            "langfuse: authentication failed — export disabled for this run".into(),
                        );
                    }
                    self.drop_events(batch.len());
                    return;
                }
                PostOutcome::ClientError(code) => {
                    self.note(
                        "client_error",
                        format!("langfuse: request rejected ({code})"),
                    );
                    self.drop_events(batch.len());
                    return;
                }
                PostOutcome::RateLimited(wait) => {
                    if rate_limit_waits >= self.cfg.max_rate_limit_waits {
                        self.drop_events(batch.len());
                        return; // rate limiting does NOT count toward the breaker
                    }
                    rate_limit_waits += 1;
                    std::thread::sleep(wait);
                }
                PostOutcome::Retryable => {
                    attempt += 1;
                    if attempt >= self.cfg.attempts {
                        if self.breaker.record_failure(Instant::now()) {
                            self.note("breaker", "langfuse: unreachable — export paused".into());
                        }
                        self.drop_events(batch.len());
                        return;
                    }
                    // 1x then 4x base, ±25% jitter
                    let base = self.cfg.backoff_base * if attempt == 1 { 1 } else { 4 };
                    std::thread::sleep(jitter(base, u64::from(attempt)));
                }
            }
        }
    }

    fn breaker_is_open(&mut self) -> bool {
        self.disabled || self.breaker.is_open(Instant::now())
    }
}

/// Spawns the exporter thread. Returns the handle whose `tx` feeds it.
pub fn spawn_exporter(
    cfg: ExporterConfig,
    ctx: MapCtx,
    status: StatusSink,
    stats: StatsSink,
) -> ExporterHandle {
    let (tx, rx) = sync_channel::<StoreOp>(QUEUE_CAPACITY);
    let (done_tx, done_rx) = sync_channel::<()>(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_thread = Arc::clone(&dropped);
    std::thread::spawn(move || {
        let mut state = ExporterState {
            agent: agent(cfg.connect_timeout, cfg.request_timeout),
            breaker: Breaker::new(cfg.breaker_threshold, cfg.breaker_open),
            cfg,
            auth_failures: 0,
            disabled: false,
            status,
            stats,
            dropped: dropped_thread,
            warned: HashSet::new(),
            ctx,
            trace_launch: HashMap::new(),
            aggs: HashMap::new(),
        };
        let mut batch = Batch::default();
        let mut first_queued: Option<Instant> = None;
        loop {
            let wait = match first_queued {
                Some(t0) => state
                    .cfg
                    .flush_interval
                    .saturating_sub(t0.elapsed())
                    .max(Duration::from_millis(1)),
                None => state.cfg.flush_interval.max(Duration::from_millis(1)),
            };
            match rx.recv_timeout(wait) {
                Ok(op) => {
                    if let Some((event, _)) = state.accept(&op) {
                        batch.push(event);
                        first_queued.get_or_insert_with(Instant::now);
                    }
                    let due = batch.is_full(state.cfg.max_batch_events, state.cfg.max_batch_bytes)
                        || first_queued.is_some_and(|t0| t0.elapsed() >= state.cfg.flush_interval);
                    if due && !batch.is_empty() {
                        state.flush(std::mem::take(&mut batch));
                        first_queued = None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !batch.is_empty() {
                        state.flush(std::mem::take(&mut batch));
                        first_queued = None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // final drain — skipped entirely when the breaker is
                    // open or auth is dead: a known-down Langfuse must cost
                    // milliseconds at quit, not the deadline
                    if !batch.is_empty() && !state.breaker_is_open() {
                        state.flush(std::mem::take(&mut batch));
                    }
                    break;
                }
            }
        }
        let _ = done_tx.send(());
    });
    ExporterHandle {
        tx,
        dropped,
        done_rx,
    }
}

/// What a replay did (or would do, when dry).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReplayReport {
    pub sessions: usize,
    pub ops: usize,
    pub events: usize,
    pub dropped: u64,
    pub first_event: Option<String>,
    pub notes: Vec<String>,
}

/// Replays stored sessions through the mapper and exporter (Decision 9):
/// the same deterministic ids, so Langfuse merges rather than duplicates.
/// Blocks until the exporter has drained or `deadline` passes; the CLI is
/// the only caller, so backpressure (not dropping) is right here.
pub fn replay_sessions(
    conn: &rusqlite::Connection,
    cfg: ExporterConfig,
    ctx: MapCtx,
    session_keys: &[String],
    dry_run: bool,
    deadline: Duration,
) -> Result<ReplayReport, String> {
    let mut report = ReplayReport::default();
    let mut ops: Vec<StoreOp> = Vec::new();
    for key in session_keys {
        let rows = crate::tracing::store::read_session_ops(conn, key)
            .map_err(|e| format!("read {key}: {e}"))?;
        report.sessions += 1;
        ops.extend(rows);
    }
    report.ops = ops.len();
    let events: Vec<Event> = ops
        .iter()
        .filter_map(|op| map::event_for(op, &ctx))
        .collect();
    report.events = events.len();
    report.first_event = events.first().map(|e| e.json.clone());
    if dry_run || events.is_empty() {
        return Ok(report);
    }
    let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let notes_sink = Arc::clone(&notes);
    let handle = spawn_exporter(
        cfg,
        ctx,
        Box::new(move |_, message| notes_sink.lock().unwrap().push(message)),
        Box::new(|_, _| {}),
    );
    for op in ops {
        if handle.tx.send(op).is_err() {
            break;
        }
    }
    let dropped = Arc::clone(&handle.dropped);
    let finished = handle.finish(deadline);
    report.dropped = dropped.load(Ordering::Relaxed);
    report.notes = notes.lock().unwrap().clone();
    if !finished {
        report
            .notes
            .push("langfuse: the replay did not finish before the deadline".into());
    }
    Ok(report)
}

/// One end-to-end host+auth probe with the exact headers the exporter
/// uses: POSTs an empty OTLP export, expects 2xx.
pub fn probe(lf: &ResolvedLangfuse) -> Result<(), String> {
    probe_endpoint(&ExporterConfig::new(lf))
}

pub fn probe_endpoint(cfg: &ExporterConfig) -> Result<(), String> {
    let agent = agent(Duration::from_secs(5), Duration::from_secs(10));
    match post_batch(&agent, cfg, r#"{"resourceSpans":[]}"#) {
        PostOutcome::Success(_) => Ok(()),
        PostOutcome::AuthFailure => Err("authentication failed (401/403) — check your keys".into()),
        PostOutcome::RateLimited(_) => Err("rate limited (429) — keys look valid though".into()),
        PostOutcome::ClientError(code) => Err(format!("server rejected the request ({code})")),
        PostOutcome::Retryable => Err("could not reach the endpoint (network/DNS/5xx)".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_header_and_rejection_parsing() {
        assert_eq!(basic_auth("pk", "sk"), "Basic cGs6c2s=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        // a clean export says nothing
        assert_eq!(rejections("{}"), (0, Vec::new()));
        assert_eq!(rejections(r#"{"partialSuccess":{}}"#), (0, Vec::new()));
        assert_eq!(rejections("not json"), (0, Vec::new()));
        // proto3 JSON sends int64 as a string; a number is accepted too
        let (n, notes) =
            rejections(r#"{"partialSuccess":{"rejectedSpans":"3","errorMessage":"bad span"}}"#);
        assert_eq!(n, 3);
        assert_eq!(notes, vec!["langfuse: 3 span(s) rejected (bad span)"]);
        let (n, notes) = rejections(r#"{"partialSuccess":{"rejectedSpans":2}}"#);
        assert_eq!(n, 2);
        assert_eq!(notes, vec!["langfuse: 2 span(s) rejected"]);
        // some builds answer a bad export with a plain error object
        let (n, notes) = rejections(r#"{"error":"Invalid content type"}"#);
        assert_eq!(n, 0);
        assert_eq!(notes, vec!["langfuse: Invalid content type"]);
    }

    #[test]
    fn breaker_opens_after_threshold_and_half_opens_once() {
        let mut b = Breaker::new(2, Duration::from_millis(50));
        let t0 = Instant::now();
        assert!(!b.record_failure(t0));
        assert!(b.record_failure(t0), "second failure opens");
        assert!(b.is_open(t0));
        assert!(
            !b.is_open(t0 + Duration::from_millis(60)),
            "half-open lets one through"
        );
        assert!(
            b.record_failure(t0 + Duration::from_millis(60)),
            "failed probe reopens"
        );
        b.record_success();
        assert!(!b.is_open(t0 + Duration::from_millis(200)));
    }
}
