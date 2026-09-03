//! The one writer OS thread: consumes `StoreOp`s from a bounded queue,
//! commits them in batches, and applies the failure policy (BUSY retries,
//! breaker). Being an OS thread (not a tokio task), the post-`kill_all`
//! flush has a join point that survives tokio runtime teardown.

use super::Store;
use super::model::StoreOp;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct WriterConfig {
    pub flush_interval: Duration,
    pub max_batch_ops: usize,
    pub busy_retries: u32,
    pub busy_backoff: Duration,
    pub breaker_threshold: u32,
    pub breaker_open: Duration,
    pub heartbeat_interval: Duration,
}

impl WriterConfig {
    pub fn new(flush_interval_ms: u64) -> Self {
        WriterConfig {
            flush_interval: Duration::from_millis(flush_interval_ms.max(1)),
            max_batch_ops: 512,
            busy_retries: 3,
            busy_backoff: Duration::from_millis(50),
            breaker_threshold: 5,
            breaker_open: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(30),
        }
    }
}

/// Bounded queue capacity: beyond this, ops drop (counted).
pub const QUEUE_CAPACITY: usize = 8192;

pub type StatusSink = Box<dyn Fn(&'static str, String) + Send>;
/// Called after every committed batch with the launch ids it touched.
pub type CommitHook = Box<dyn FnMut(&Store, &[String]) + Send>;

pub struct WriterHandle {
    /// Clone into pipelines; `try_send` only.
    pub tx: SyncSender<StoreOp>,
    pub dropped: Arc<AtomicU64>,
    done_rx: Receiver<()>,
}

impl WriterHandle {
    /// Drops this handle's sender and waits (bounded) for the thread to
    /// drain and finish. All pipeline-held clones of `tx` must be gone
    /// first or the wait just times out — quitting is never blocked.
    pub fn finish(self, deadline: Duration) -> bool {
        let WriterHandle { tx, done_rx, .. } = self;
        drop(tx);
        done_rx.recv_timeout(deadline).is_ok()
    }
}

enum BreakerState {
    Closed { consecutive: u32 },
    Open { until: Instant },
    HalfOpen,
}

pub(crate) struct Breaker {
    state: BreakerState,
    threshold: u32,
    open_for: Duration,
}

impl Breaker {
    pub(crate) fn new(threshold: u32, open_for: Duration) -> Self {
        Breaker {
            state: BreakerState::Closed { consecutive: 0 },
            threshold,
            open_for,
        }
    }

    pub(crate) fn is_open(&mut self, now: Instant) -> bool {
        match self.state {
            BreakerState::Open { until } if now < until => true,
            BreakerState::Open { .. } => {
                self.state = BreakerState::HalfOpen;
                false
            }
            _ => false,
        }
    }

    pub(crate) fn record_success(&mut self) {
        self.state = BreakerState::Closed { consecutive: 0 };
    }

    /// Returns true when this failure (re)opened the breaker.
    pub(crate) fn record_failure(&mut self, now: Instant) -> bool {
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

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(ffi, _)
            if matches!(ffi.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

struct WriterState {
    store: Store,
    cfg: WriterConfig,
    breaker: Breaker,
    status: StatusSink,
    on_commit: Option<CommitHook>,
    dropped: Arc<AtomicU64>,
    warned: std::collections::HashSet<&'static str>,
}

impl WriterState {
    fn note(&mut self, class: &'static str, message: String) {
        if self.warned.insert(class) {
            (self.status)(class, message);
        }
    }

    fn drop_ops(&mut self, count: usize) {
        self.dropped.fetch_add(count as u64, Ordering::Relaxed);
        self.note(
            "dropped",
            "tracing: some trace rows were dropped (store errors/backpressure)".to_string(),
        );
    }

    fn flush(&mut self, batch: Vec<StoreOp>) {
        if batch.is_empty() {
            return;
        }
        if self.breaker.is_open(Instant::now()) {
            self.drop_ops(batch.len());
            return;
        }
        let mut attempt = 0u32;
        loop {
            match self.store.apply(&batch) {
                Ok(failures) => {
                    if failures > 0 {
                        let detail = self.store.last_error.clone().unwrap_or_default();
                        self.note(
                            "op_failed",
                            format!("tracing: {failures} row(s) rejected by the store ({detail})"),
                        );
                    }
                    self.breaker.record_success();
                    if let Some(hook) = self.on_commit.as_mut() {
                        let mut launches: Vec<String> = batch
                            .iter()
                            .filter_map(|op| op.launch_id().map(str::to_string))
                            .collect();
                        launches.sort();
                        launches.dedup();
                        hook(&self.store, &launches);
                    }
                    return;
                }
                Err(e) if is_busy(&e) && attempt < self.cfg.busy_retries => {
                    attempt += 1;
                    // 50 / 200 / 800 ms
                    std::thread::sleep(self.cfg.busy_backoff * 4u32.pow(attempt - 1));
                }
                Err(e) => {
                    if self.breaker.record_failure(Instant::now()) {
                        self.note("breaker", format!("tracing paused: {e}"));
                    }
                    self.drop_ops(batch.len());
                    return;
                }
            }
        }
    }
}

/// Spawns the writer thread, taking ownership of the store.
pub fn spawn_writer(
    store: Store,
    cfg: WriterConfig,
    status: StatusSink,
    on_commit: Option<CommitHook>,
) -> WriterHandle {
    let (tx, rx) = sync_channel::<StoreOp>(QUEUE_CAPACITY);
    let (done_tx, done_rx) = sync_channel::<()>(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_thread = Arc::clone(&dropped);
    std::thread::spawn(move || {
        let mut state = WriterState {
            store,
            breaker: Breaker::new(cfg.breaker_threshold, cfg.breaker_open),
            cfg,
            status,
            on_commit,
            dropped: dropped_thread,
            warned: std::collections::HashSet::new(),
        };
        let mut batch: Vec<StoreOp> = Vec::new();
        let mut first_queued: Option<Instant> = None;
        let mut last_heartbeat = Instant::now();
        loop {
            let wait = match first_queued {
                Some(t0) => state
                    .cfg
                    .flush_interval
                    .saturating_sub(t0.elapsed())
                    .max(Duration::from_millis(1)),
                None => state.cfg.heartbeat_interval.min(Duration::from_secs(5)),
            };
            match rx.recv_timeout(wait) {
                Ok(op) => {
                    batch.push(op);
                    first_queued.get_or_insert_with(Instant::now);
                    let due = batch.len() >= state.cfg.max_batch_ops
                        || first_queued.is_some_and(|t0| t0.elapsed() >= state.cfg.flush_interval);
                    if due {
                        state.flush(std::mem::take(&mut batch));
                        first_queued = None;
                        last_heartbeat = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !batch.is_empty() {
                        state.flush(std::mem::take(&mut batch));
                        first_queued = None;
                        last_heartbeat = Instant::now();
                    } else if last_heartbeat.elapsed() >= state.cfg.heartbeat_interval {
                        let _ = state.store.heartbeat();
                        last_heartbeat = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // final drain — skipped only while the breaker is open
                    if !batch.is_empty() && !state.breaker.is_open(Instant::now()) {
                        state.flush(std::mem::take(&mut batch));
                    }
                    let _ = state.store.end_run();
                    break;
                }
            }
        }
        let _ = done_tx.send(());
    });
    WriterHandle {
        tx,
        dropped,
        done_rx,
    }
}
