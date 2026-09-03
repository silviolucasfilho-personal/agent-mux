use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    /// A captured terminal mouse event (wheel, click, drag). Routed by
    /// App::handle_mouse; events outside the main pane are dropped there.
    Mouse(MouseEvent),
    /// (cols, rows) as crossterm reports them.
    Resize(u16, u16),
    PtyOutput {
        id: usize,
        bytes: Vec<u8>,
    },
    /// Session `id`'s pty has exited (or is exiting). Two things a consumer
    /// must know before handling this, both driven by how `Session::spawn`
    /// (session.rs) detects exit -- see its doc comment for the full
    /// reasoning:
    ///
    /// - **Not once-per-session.** A reader thread (on `read()` EOF/err)
    ///   and a separate exit-watcher thread each independently detect exit
    ///   and can each send this for the same `id`, so duplicates for one
    ///   session are expected, not a bug. Handling (e.g.
    ///   `Session::mark_exited`) must be idempotent.
    /// - **Not an end-of-output marker.** Ordering relative to a session's
    ///   final `PtyOutput` batches is unspecified -- the watcher thread's
    ///   copy can arrive before the reader thread has forwarded the last
    ///   bit of output. Consumers must keep processing any `PtyOutput` that
    ///   arrives after a `PtyExit` rather than treating `PtyExit` as a
    ///   signal to stop feeding the parser.
    PtyExit {
        id: usize,
    },
    /// One-line tracing status notice for the status bar (emitted at most
    /// once per class per run by the trace runtime/writer).
    TraceStatus(String),
    /// Live per-launch rollup from the store writer (throttled to one per
    /// launch per second); `launch_id` matches `SessionTraceHandle`.
    TraceStats {
        launch_id: String,
        stats: crate::tracing::store::query::LaunchStats,
    },
    Tick,
}
