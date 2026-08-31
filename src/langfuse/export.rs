//! The one blocking exporter OS thread: consumes completed spans from a
//! bounded queue, batches them, and POSTs OTLP/JSON to Langfuse.
//!
//! Failure policy (the invariant: tracing can never break, block, or slow a
//! session): the queue is try_send-only (full ⇒ drop + count); per-batch
//! retries are bounded; 429 honors Retry-After (capped, twice); two
//! consecutive 401/403 disable the exporter for the run; five consecutive
//! failed batches open a circuit breaker (60 s, then one half-open probe);
//! the final drain on shutdown is skipped entirely while the breaker is
//! open. Being an OS thread (not a tokio task), the post-`kill_all` flush
//! has a join point that survives tokio runtime teardown.

use crate::langfuse::otlp::{self, Span};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Everything the exporter thread needs. Timings are configurable so tests
/// can shrink them; production values come from `ExporterConfig::new`.
#[derive(Debug, Clone)]
pub struct ExporterConfig {
    /// Full endpoint: `{host}/api/public/otel/v1/traces`.
    pub endpoint: String,
    /// `Basic base64(pk:sk)`.
    pub auth: String,
    pub flush_interval: Duration,
    pub max_batch_spans: usize,
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
    pub fn new(host: &str, public_key: &str, secret_key: &str, flush_interval_ms: u64) -> Self {
        ExporterConfig {
            endpoint: format!("{host}/api/public/otel/v1/traces"),
            auth: otlp::basic_auth(public_key, secret_key),
            flush_interval: Duration::from_millis(flush_interval_ms),
            max_batch_spans: 256,
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

/// Bounded queue capacity: beyond this, spans drop (counted).
const QUEUE_CAPACITY: usize = 2048;

pub type StatusSink = Box<dyn Fn(&'static str, String) + Send>;

pub struct ExporterHandle {
    /// Clone into pipelines; `try_send` only.
    pub tx: SyncSender<Span>,
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

enum PostOutcome {
    Success,
    Retryable,
    RateLimited(Duration),
    AuthFailure,
    ClientError(u16),
}

enum BreakerState {
    Closed { consecutive: u32 },
    Open { until: Instant },
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
            // a failed half-open probe reopens immediately
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

fn post_batch(agent: &ureq::Agent, cfg: &ExporterConfig, body: &str) -> PostOutcome {
    let result = agent
        .post(&cfg.endpoint)
        .header("Authorization", &cfg.auth)
        .header("Content-Type", "application/json")
        .header("x-langfuse-ingestion-version", "4")
        .header("User-Agent", concat!("agent-mux/", env!("CARGO_PKG_VERSION")))
        .send(body.as_bytes());
    match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match status {
                200..=299 => PostOutcome::Success,
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

/// ±25% deterministic-ish jitter (cheap xorshift over the attempt count and
/// address entropy; no Math.random-style dependency needed).
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

struct ExporterState {
    cfg: ExporterConfig,
    agent: ureq::Agent,
    breaker: Breaker,
    auth_failures: u32,
    disabled: bool,
    status: StatusSink,
    dropped: Arc<AtomicU64>,
    warned: std::collections::HashSet<&'static str>,
}

impl ExporterState {
    /// Once-per-class status line to the UI.
    fn note(&mut self, class: &'static str, message: String) {
        if self.warned.insert(class) {
            (self.status)(class, message);
        }
    }

    fn drop_spans(&mut self, count: usize) {
        self.dropped.fetch_add(count as u64, Ordering::Relaxed);
        self.note(
            "dropped",
            "langfuse: some spans were dropped (network/backpressure)".to_string(),
        );
    }

    /// Sends one batch with the full retry/breaker policy. The batch is
    /// consumed either way — the queue is a conduit, not a durable buffer.
    fn flush(&mut self, batch: Vec<Span>) {
        if batch.is_empty() {
            return;
        }
        if self.disabled {
            self.drop_spans(batch.len());
            return;
        }
        if self.breaker.is_open(Instant::now()) {
            self.drop_spans(batch.len());
            return;
        }
        let body = otlp::build_request(&batch);
        let mut rate_limit_waits = 0u32;
        let mut attempt = 0u32;
        loop {
            match post_batch(&self.agent, &self.cfg, &body) {
                PostOutcome::Success => {
                    // a successful POST proves the credentials work, so any
                    // earlier 401/403 was transient — "2 consecutive" resets
                    self.auth_failures = 0;
                    self.breaker.record_success();
                    return;
                }
                PostOutcome::AuthFailure => {
                    self.auth_failures += 1;
                    if self.auth_failures >= 2 {
                        self.disabled = true;
                        self.note(
                            "auth",
                            "langfuse: authentication failed — tracing disabled".into(),
                        );
                    }
                    self.drop_spans(batch.len());
                    return;
                }
                PostOutcome::ClientError(code) => {
                    self.note("client_error", format!("langfuse: request rejected ({code})"));
                    self.drop_spans(batch.len());
                    return;
                }
                PostOutcome::RateLimited(wait) => {
                    if rate_limit_waits >= self.cfg.max_rate_limit_waits {
                        self.drop_spans(batch.len());
                        return; // rate limiting does NOT count toward the breaker
                    }
                    rate_limit_waits += 1;
                    std::thread::sleep(wait);
                }
                PostOutcome::Retryable => {
                    attempt += 1;
                    if attempt >= self.cfg.attempts {
                        if self.breaker.record_failure(Instant::now()) {
                            self.note(
                                "breaker",
                                "langfuse: unreachable — tracing paused".into(),
                            );
                        }
                        self.drop_spans(batch.len());
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
pub fn spawn_exporter(cfg: ExporterConfig, status: StatusSink) -> ExporterHandle {
    let (tx, rx) = sync_channel::<Span>(QUEUE_CAPACITY);
    let (done_tx, done_rx) = sync_channel::<()>(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_thread = Arc::clone(&dropped);
    std::thread::spawn(move || {
        let agent_config = ureq::Agent::config_builder()
            .timeout_connect(Some(cfg.connect_timeout))
            .timeout_global(Some(cfg.request_timeout))
            .http_status_as_error(false)
            .build();
        let mut state = ExporterState {
            agent: agent_config.new_agent(),
            breaker: Breaker::new(cfg.breaker_threshold, cfg.breaker_open),
            cfg,
            auth_failures: 0,
            disabled: false,
            status,
            dropped: dropped_thread,
            warned: std::collections::HashSet::new(),
        };
        let mut batch: Vec<Span> = Vec::new();
        let mut batch_bytes = 0usize;
        let mut first_queued: Option<Instant> = None;
        loop {
            let wait = match first_queued {
                Some(t0) => state
                    .cfg
                    .flush_interval
                    .saturating_sub(t0.elapsed())
                    .max(Duration::from_millis(1)),
                // floor the idle wait too: a zero flush_interval must not
                // hot-spin this thread at 100% CPU
                None => state.cfg.flush_interval.max(Duration::from_millis(1)),
            };
            match rx.recv_timeout(wait) {
                Ok(span) => {
                    batch_bytes += otlp::estimated_size(&span);
                    batch.push(span);
                    first_queued.get_or_insert_with(Instant::now);
                    let due = batch.len() >= state.cfg.max_batch_spans
                        || batch_bytes >= state.cfg.max_batch_bytes
                        || first_queued.is_some_and(|t0| t0.elapsed() >= state.cfg.flush_interval);
                    if due {
                        state.flush(std::mem::take(&mut batch));
                        batch_bytes = 0;
                        first_queued = None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !batch.is_empty() {
                        state.flush(std::mem::take(&mut batch));
                        batch_bytes = 0;
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
    ExporterHandle { tx, dropped, done_rx }
}

/// One end-to-end host+auth probe with the exact headers the exporter uses:
/// POSTs an empty resourceSpans batch, expects 200. Used by `langfuse doctor`.
pub fn probe(host: &str, public_key: &str, secret_key: &str) -> Result<(), String> {
    let cfg = ExporterConfig::new(host, public_key, secret_key, 3000);
    let agent_config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build();
    let agent = agent_config.new_agent();
    match post_batch(&agent, &cfg, r#"{"resourceSpans":[]}"#) {
        PostOutcome::Success => Ok(()),
        PostOutcome::AuthFailure => Err("authentication failed (401/403) — check your keys".into()),
        PostOutcome::RateLimited(_) => Err("rate limited (429) — keys look valid though".into()),
        PostOutcome::ClientError(code) => Err(format!("server rejected the request ({code})")),
        PostOutcome::Retryable => Err("could not reach the endpoint (network/DNS/5xx)".into()),
    }
}
