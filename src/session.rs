use crate::config::Profile;
use crate::events::AppEvent;
use crate::status::{BellCounter, Status, StatusTracker};
use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Sender;

/// Find a native .exe for `command`: either `command` is itself a path to an
/// existing .exe, or `<command>.exe` exists in one of `path_var`'s entries.
pub fn find_exe(command: &str, path_var: &OsStr) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && p.is_file() {
        return Some(p.to_path_buf());
    }
    if p.is_absolute() {
        return None;
    }
    let has_exe_ext = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    for dir in std::env::split_paths(path_var) {
        let candidate = if has_exe_ext {
            dir.join(command)
        } else {
            dir.join(format!("{command}.exe"))
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Extensions (besides `.exe`, which `find_exe` already covers) that
/// Windows can run through `cmd /c` -- npm-installed CLIs and many other
/// dev tools ship as one of these instead of a native `.exe`.
#[cfg(windows)]
const SHIM_EXTENSIONS: [&str; 3] = ["cmd", "bat", "com"];

/// True if `command` names something a Windows `cmd /c <command>`
/// invocation could run, without itself being a native `.exe` (that's
/// `find_exe`'s job): an existing file with `command`'s own extension, a
/// `.cmd`/`.bat`/`.com` shim of that name in one of `path_var`'s entries,
/// or `command` itself already pointing at an existing file (e.g. an
/// absolute or relative path handed through as-is).
#[cfg(windows)]
fn find_shim(command: &str, path_var: &OsStr) -> bool {
    let p = Path::new(command);
    if p.is_file() {
        return true;
    }
    if p.is_absolute() {
        return false;
    }
    let has_ext = p.extension().is_some();
    for dir in std::env::split_paths(path_var) {
        if has_ext && dir.join(command).is_file() {
            return true;
        }
        for ext in SHIM_EXTENSIONS {
            if dir.join(format!("{command}.{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// (program, prefix_args) to hand to CommandBuilder, or `None` if `command`
/// can't be found at all. On Windows this is three-way:
/// 1. a native `.exe` (`find_exe`) -> run it directly;
/// 2. otherwise something `cmd /c` could run (`find_shim`: a `.cmd`/`.bat`/
///    `.com` shim, a file already carrying its own extension, or an
///    existing file path) -> `cmd.exe /c <command>`, letting `cmd` do its
///    own PATHEXT-aware resolution;
/// 3. nothing found -> `None`, so `Session::spawn` fails up front instead
///    of spawning `cmd.exe` for a command that doesn't exist and only
///    failing later with exit code 9009.
///
/// Unix has no shim concept and no separate resolution step of its own --
/// this always succeeds, and a genuinely missing command surfaces as a
/// spawn error from the OS.
#[cfg(windows)]
pub fn resolve_command(command: &str) -> Option<(String, Vec<String>)> {
    resolve_command_with_path(command, &std::env::var_os("PATH").unwrap_or_default())
}

#[cfg(windows)]
fn resolve_command_with_path(command: &str, path_var: &OsStr) -> Option<(String, Vec<String>)> {
    if let Some(exe) = find_exe(command, path_var) {
        return Some((exe.to_string_lossy().into_owned(), vec![]));
    }
    if find_shim(command, path_var) {
        return Some(("cmd.exe".into(), vec!["/c".into(), command.into()]));
    }
    None
}

#[cfg(not(windows))]
pub fn resolve_command(command: &str) -> Option<(String, Vec<String>)> {
    Some((command.to_string(), vec![]))
}

pub struct Session {
    pub id: usize,
    pub profile: Profile,
    pub dir: PathBuf,
    pub parser: vt100::Parser<BellCounter>,
    pub tracker: StatusTracker,
    /// Langfuse pipeline handle, attached by App after a successful spawn
    /// when tracing is planned for this launch. Dropping the Session closes
    /// the phase channel, which the pipeline treats like an exit.
    pub trace: Option<crate::langfuse::SessionTraceHandle>,
    /// Cached total scrollback row count, refreshed at the end of
    /// `process_output`/`resize`. See `scroll_view`/`probe_scrollback_len`
    /// for why this needs caching rather than reading it on demand.
    scrollback_len: usize,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty>,
    // Shared with the exit-watcher thread spawned in `spawn()` (see there
    // for why exit detection can't rely solely on the reader thread's EOF).
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

/// How often the exit-watcher thread polls `Child::try_wait()`.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

const SCROLLBACK_LINES: usize = 1000;

impl Session {
    /// Spawns `profile.command` in a new pty and starts two background
    /// threads that report through `tx`:
    ///
    /// - a **reader thread** that forwards pty output as `AppEvent::PtyOutput`
    ///   and, once `read()` returns `Ok(0)`/`Err` (the pty's output stream
    ///   ending), sends one `AppEvent::PtyExit`;
    /// - an **exit-watcher thread** that polls the child process directly
    ///   and sends its own `AppEvent::PtyExit` as soon as the child has
    ///   actually exited. This is necessary because on Windows the pty's
    ///   output pipe does not EOF just because the child exited -- it stays
    ///   open for as long as this Session's pseudo console handle
    ///   (`master`) is alive, so the reader thread's EOF path alone would
    ///   never fire for a normal exit there.
    ///
    /// Because of this, `AppEvent::PtyExit` for a given `id` is **not**
    /// guaranteed once-per-session (either or both threads can send one)
    /// and is **not** an end-of-output marker (a watcher-thread `PtyExit`
    /// can arrive before the reader thread's last `PtyOutput` batches). See
    /// `AppEvent::PtyExit`'s doc comment for what this means for consumers.
    /// `extra_args` are appended AFTER `profile.args` and `extra_env` is
    /// set additively on the child only — launch-time extras (e.g. an
    /// injected `--session-id`) are deliberately NOT part of `profile`, so
    /// respawn (which clones the profile) re-plans fresh ones.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: usize,
        profile: Profile,
        dir: PathBuf,
        rows: u16,
        cols: u16,
        tx: Sender<AppEvent>,
        extra_args: &[String],
        extra_env: &[(String, String)],
    ) -> anyhow::Result<Session> {
        anyhow::ensure!(dir.is_dir(), "not a directory: {}", dir.display());
        let (program, prefix_args) = resolve_command(&profile.command)
            .ok_or_else(|| anyhow::anyhow!("command not found: {}", profile.command))?;
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty failed")?;
        let mut cmd = CommandBuilder::new(program);
        for a in &prefix_args {
            cmd.arg(a);
        }
        for a in &profile.args {
            cmd.arg(a);
        }
        for a in extra_args {
            cmd.arg(a);
        }
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.cwd(&dir);
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to spawn `{}`", profile.command))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;
        let writer = pair.master.take_writer().context("take writer")?;
        let child = Arc::new(Mutex::new(child));

        // Reader thread: forwards PTY output; on EOF/err, signals exit once
        // (the mandated contract) and ends.
        let reader_tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = reader_tx.blocking_send(AppEvent::PtyExit { id });
                        break;
                    }
                    Ok(n) => {
                        let bytes = buf[..n].to_vec();
                        if reader_tx
                            .blocking_send(AppEvent::PtyOutput { id, bytes })
                            .is_err()
                        {
                            break; // app is shutting down
                        }
                    }
                }
            }
        });

        // Exit-watcher thread: polls `try_wait()` for the child directly.
        // On Windows, ConPTY's output pipe does not EOF just because the
        // child process exited -- it stays open for as long as the pseudo
        // console handle (this Session's `master`) is alive, so the reader
        // thread's EOF path above would never fire for a normal exit. This
        // thread is the reliable cross-platform signal; the reader thread's
        // EOF/err path stays as a secondary, faster path where it does fire
        // (e.g. real Unix ptys, or once the Session itself is torn down).
        let watcher_child = Arc::clone(&child);
        let watcher_tx = tx;
        std::thread::spawn(move || {
            loop {
                if watcher_tx.is_closed() {
                    break; // app is shutting down
                }
                let exited = watcher_child
                    .lock()
                    .ok()
                    .and_then(|mut c| c.try_wait().ok().flatten());
                if exited.is_some() {
                    let _ = watcher_tx.blocking_send(AppEvent::PtyExit { id });
                    break;
                }
                std::thread::sleep(EXIT_POLL_INTERVAL);
            }
        });

        Ok(Session {
            id,
            profile,
            dir,
            trace: None,
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                SCROLLBACK_LINES,
                BellCounter::default(),
            ),
            tracker: StatusTracker::new(),
            scrollback_len: 0,
            writer,
            master: pair.master,
            child,
        })
    }

    pub fn process_output(&mut self, bytes: &[u8], now: Instant, focused: bool) {
        // Content-anchored while scrolled: vt100's `Grid::scroll_up`
        // already increments `scrollback_offset` by 1 for every row it
        // pushes into scrollback whenever the offset is nonzero (see
        // vendored vt100 0.16.2 grid.rs, `scroll_up`), so no manual
        // re-anchoring is needed here -- just process and refresh the
        // cached length. Limitation: that increment is clamped to
        // scrollback's length, so once scrollback saturates at
        // SCROLLBACK_LINES, a view pinned at the very top (offset == len)
        // can drift as the oldest rows get dropped out from under it --
        // same trade-off real emulators make at their buffer cap.
        self.parser.process(bytes);
        self.scrollback_len = self.probe_scrollback_len();
        let bells = self.parser.callbacks().count;
        self.tracker.on_output(now, bells, focused);
        // ConPTY (and some full-screen TUIs) query the cursor position and
        // block until we answer on the input side. See BellCounter's doc
        // comment for why this lives on the callbacks type, and for why
        // this is a count rather than a flag: a batch can contain more than
        // one plain `CSI 6 n` query, and each needs its own reply.
        let pending = std::mem::take(&mut self.parser.callbacks_mut().pending_cursor_reports);
        if pending > 0 {
            let (row, col) = self.parser.screen().cursor_position();
            let reply = format!("\x1b[{};{}R", row + 1, col + 1);
            for _ in 0..pending {
                // Must be a single write_all: `write!` here fragments the
                // reply across several small write() calls (one per
                // formatted segment), and ConPTY's handshake reader does
                // not tolerate a split escape sequence -- it just hangs
                // forever.
                let _ = self.writer.write_all(reply.as_bytes());
            }
            let _ = self.writer.flush();
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        // Reflow can change how many rows scrollback holds.
        self.scrollback_len = self.probe_scrollback_len();
    }

    /// Rows currently scrolled back (0 = live bottom).
    pub fn scrolled(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// (total scrollback rows, current offset) without touching the parser.
    /// The length is cached by process_output/resize because probing vt100
    /// for it requires mutation, and the render path is immutable.
    pub fn scroll_view(&self) -> (usize, usize) {
        (self.scrollback_len, self.parser.screen().scrollback())
    }

    /// vt100 doesn't expose scrollback length; set_scrollback self-clamps,
    /// so probing with usize::MAX and restoring reads it in O(1).
    fn probe_scrollback_len(&mut self) -> usize {
        let cur = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let len = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(cur);
        len
    }

    /// Positive delta scrolls back into history; negative toward live.
    /// Clamped at both ends (vt100 clamps the top, we clamp at 0).
    pub fn scroll_by(&mut self, delta: i32) {
        let cur = self.parser.screen().scrollback() as i64;
        let next = (cur + i64::from(delta)).max(0) as usize;
        self.parser.screen_mut().set_scrollback(next);
    }

    pub fn set_scroll(&mut self, offset: usize) {
        self.parser.screen_mut().set_scrollback(offset);
    }

    pub fn scroll_to_top(&mut self) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    pub fn kill(&mut self) {
        if let Ok(child) = self.child.lock() {
            let _ = child.clone_killer().kill();
        }
    }

    /// EOF (or an exit-watcher signal) already happened, so the child is
    /// gone or going; wait briefly for the exit code, then give up and
    /// record an unknown code. Never holds the child lock across the sleep,
    /// so it doesn't block `kill()`/other callers polling `try_wait()`
    /// concurrently. Idempotent: `PtyExit` for a given id can arrive twice
    /// (reader thread + exit-watcher thread), and calling this a second
    /// time just re-locks, finds the child already reaped, and re-records
    /// (or -- once already recorded -- `StatusTracker::on_exit` overwriting
    /// its own `exited` field is harmless).
    pub fn mark_exited(&mut self) {
        for _ in 0..20 {
            let result = self.child.lock().ok().map(|mut c| c.try_wait());
            match result {
                Some(Ok(Some(status))) => {
                    self.tracker.on_exit(Some(status.exit_code()));
                    return;
                }
                Some(Ok(None)) => std::thread::sleep(Duration::from_millis(50)),
                Some(Err(_)) | None => break,
            }
        }
        self.tracker.on_exit(None);
    }

    pub fn status(&self, now: Instant) -> Status {
        self.tracker.status(now)
    }
}

#[cfg(test)]
mod scroll_tests {
    #[test]
    fn vt100_set_scrollback_self_clamps_and_probe_reads_len() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..30 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        // 30 lines on a 5-row screen -> scrollback holds the rest
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        assert!(len >= 20, "expected >=20 scrollback rows, got {len}");
        parser.screen_mut().set_scrollback(0);
        assert_eq!(parser.screen().scrollback(), 0);
        // offset beyond len clamps to len
        parser.screen_mut().set_scrollback(len + 50);
        assert_eq!(parser.screen().scrollback(), len);
    }

    #[test]
    fn scrolled_view_shows_history() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..30 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        assert!(!parser.screen().contents().contains("line-0"));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert!(parser.screen().contents().contains("line-0"));
    }

    fn top_line(parser: &vt100::Parser<()>) -> String {
        parser
            .screen()
            .contents()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Below the scrollback cap: vt100's own `scroll_up` grows the offset
    /// by 1 per row pushed into scrollback whenever scrolled, so the view
    /// stays pinned on new output with no help from `Session`.
    #[test]
    fn vt100_natively_anchors_view_below_scrollback_cap() {
        let mut parser: vt100::Parser<()> = vt100::Parser::new(5, 20, 100);
        for i in 0..30 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(5);
        let before = top_line(&parser);
        for i in 30..33 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        assert_eq!(parser.screen().scrollback(), 8);
        assert_eq!(top_line(&parser), before, "view moved while scrolled");
    }

    /// At scrollback saturation: the same auto-increment still applies (it
    /// is clamped to the -- now constant -- scrollback length, not to
    /// whether the cap has been hit), so a view scrolled partway back
    /// still tracks new output without drifting, as long as it isn't
    /// pinned at the very top (offset == len; see the limitation comment
    /// in `process_output`).
    #[test]
    fn vt100_natively_anchors_view_at_scrollback_saturation() {
        let mut parser: vt100::Parser<()> = vt100::Parser::new(5, 20, 20);
        for i in 0..40 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(10);
        let before = top_line(&parser);
        for i in 40..45 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        assert_eq!(parser.screen().scrollback(), 15);
        assert_eq!(top_line(&parser), before, "view moved while scrolled");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn finds_exe_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("myagent.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let found = find_exe("myagent", dir.path().as_os_str());
        assert_eq!(found, Some(exe));
    }

    #[test]
    fn explicit_exe_path_is_used_directly() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let cmd = exe.to_string_lossy().into_owned();
        assert_eq!(find_exe(&cmd, std::ffi::OsStr::new("")), Some(exe));
    }

    #[test]
    fn missing_command_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_exe("nope-nothing-here", dir.path().as_os_str()), None);
    }

    #[cfg(windows)]
    #[test]
    fn unresolved_command_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // "definitely-not-on-path-xyz" won't resolve as an .exe, a
        // .cmd/.bat/.com shim, or an existing file path.
        assert_eq!(
            resolve_command_with_path("definitely-not-on-path-xyz", dir.path().as_os_str()),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn cmd_shim_on_path_resolves_via_cmd_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myscript.cmd"), b"@echo off\r\n").unwrap();
        let (program, args) =
            resolve_command_with_path("myscript", dir.path().as_os_str()).unwrap();
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, vec!["/c".to_string(), "myscript".to_string()]);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_always_direct() {
        let (program, args) = resolve_command("claude").unwrap();
        assert_eq!(program, "claude");
        assert!(args.is_empty());
    }
}
