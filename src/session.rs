use crate::config::Profile;
use crate::events::AppEvent;
use crate::status::{BellCounter, Status, StatusTracker};
use anyhow::Context;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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
        let candidate =
            if has_exe_ext { dir.join(command) } else { dir.join(format!("{command}.exe")) };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// (program, prefix_args) to hand to CommandBuilder. On Windows,
/// npm-installed CLIs are .cmd shims that ConPTY can't spawn directly, so
/// anything that isn't a native .exe goes through `cmd /c`.
#[cfg(windows)]
pub fn resolve_command(command: &str) -> (String, Vec<String>) {
    match find_exe(command, &std::env::var_os("PATH").unwrap_or_default()) {
        Some(exe) => (exe.to_string_lossy().into_owned(), vec![]),
        None => ("cmd.exe".into(), vec!["/c".into(), command.into()]),
    }
}

#[cfg(not(windows))]
pub fn resolve_command(command: &str) -> (String, Vec<String>) {
    (command.to_string(), vec![])
}

pub struct Session {
    pub id: usize,
    pub profile: Profile,
    pub dir: PathBuf,
    pub parser: vt100::Parser<BellCounter>,
    pub tracker: StatusTracker,
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
    pub fn spawn(
        id: usize,
        profile: Profile,
        dir: PathBuf,
        rows: u16,
        cols: u16,
        tx: Sender<AppEvent>,
    ) -> anyhow::Result<Session> {
        anyhow::ensure!(dir.is_dir(), "not a directory: {}", dir.display());
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("openpty failed")?;
        let (program, prefix_args) = resolve_command(&profile.command);
        let mut cmd = CommandBuilder::new(program);
        for a in &prefix_args {
            cmd.arg(a);
        }
        for a in &profile.args {
            cmd.arg(a);
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
                        if reader_tx.blocking_send(AppEvent::PtyOutput { id, bytes }).is_err() {
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
        std::thread::spawn(move || loop {
            if watcher_tx.is_closed() {
                break; // app is shutting down
            }
            let exited =
                watcher_child.lock().ok().and_then(|mut c| c.try_wait().ok().flatten());
            if exited.is_some() {
                let _ = watcher_tx.blocking_send(AppEvent::PtyExit { id });
                break;
            }
            std::thread::sleep(EXIT_POLL_INTERVAL);
        });

        Ok(Session {
            id,
            profile,
            dir,
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                SCROLLBACK_LINES,
                BellCounter::default(),
            ),
            tracker: StatusTracker::new(),
            writer,
            master: pair.master,
            child,
        })
    }

    pub fn process_output(&mut self, bytes: &[u8], now: Instant, focused: bool) {
        self.parser.process(bytes);
        let bells = self.parser.callbacks().count;
        self.tracker.on_output(now, bells, focused);
        // ConPTY (and some full-screen TUIs) query the cursor position and
        // block until we answer on the input side. See BellCounter's doc
        // comment for why this lives on the callbacks type.
        if self.parser.callbacks().needs_cursor_report {
            self.parser.callbacks_mut().needs_cursor_report = false;
            let (row, col) = self.parser.screen().cursor_position();
            // Must be a single write_all: `write!` here fragments the reply
            // across several small write() calls (one per formatted
            // segment), and ConPTY's handshake reader does not tolerate a
            // split escape sequence -- it just hangs forever.
            let reply = format!("\x1b[{};{}R", row + 1, col + 1);
            let _ = self.writer.write_all(reply.as_bytes());
            let _ = self.writer.flush();
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    pub fn kill(&mut self) {
        if let Ok(child) = self.child.lock() {
            let _ = child.clone_killer().kill();
        }
    }

    pub fn mark_exited(&mut self) {
        let code = self
            .child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .map(|s| s.exit_code());
        self.tracker.on_exit(code);
    }

    pub fn status(&self, now: Instant) -> Status {
        self.tracker.status(now)
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
    fn unresolved_command_falls_back_to_cmd_shim() {
        // "definitely-not-on-path-xyz" won't resolve to an .exe
        let (program, args) = resolve_command("definitely-not-on-path-xyz");
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, vec!["/c".to_string(), "definitely-not-on-path-xyz".to_string()]);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_always_direct() {
        let (program, args) = resolve_command("claude");
        assert_eq!(program, "claude");
        assert!(args.is_empty());
    }
}
