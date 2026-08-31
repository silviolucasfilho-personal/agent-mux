use crate::config::Profile;
use crate::events::AppEvent;
use crate::history::{self, SessionSummary};
use crate::keys::encode_key;
use crate::mouse::{WheelRoute, encode_mouse, route_wheel};
use crate::search::SearchState;
use crate::selection::{self, Pos, Selection};
use crate::session::Session;
use crate::status::Status;
use crate::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;
use tokio::sync::mpsc::Sender;

#[derive(Debug)]
pub enum Mode {
    Control,
    Attached,
    NewSession(DialogState),
    SessionHistory(HistoryState),
    ConfirmKill,
    ConfirmQuit,
}

#[derive(Debug)]
pub enum Action {
    None,
    Quit,
    EnterConfirmQuit,
    MoveUp,
    MoveDown,
    Attach,
    Detach,
    OpenNewSession,
    OpenSessionHistory,
    KillSelected,
    EnterConfirmKill,
    RemoveSelected,
    RespawnSelected,
    CancelToControl,
    ForwardBytes(Vec<u8>),
    SendLiteralDetachKey,
    /// NewSession mode: App routes the key to the DialogState it owns.
    DialogKey,
    /// SessionHistory mode: App routes the key to the HistoryState it owns.
    HistoryKey,
}

pub struct DispatchCtx {
    pub selected_status: Option<Status>,
    pub any_working: bool,
    pub just_detached: bool,
}

fn is_ctrl_q(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
}

/// Bytes to write for a paste: wrapped in bracketed-paste markers when the
/// child enabled bracketed paste, otherwise newlines normalized to CR so a
/// multi-line paste presses Enter instead of inserting raw LFs.
///
/// In the bracketed branch, any literal `ESC[201~` (the paste-end marker)
/// already present in the clipboard text is stripped first. Otherwise
/// clipboard content containing that sequence would terminate the bracket
/// early and the remainder would land in the child as raw keystrokes
/// instead of pasted text -- a classic paste-injection: e.g. a copied
/// snippet ending the bracket then typing `rm -rf ~` as if the user had.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let sanitized = text.replace("\x1b[201~", "");
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(sanitized.as_bytes());
        b.extend_from_slice(b"\x1b[201~");
        b
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod paste_tests {
    use super::paste_bytes;

    #[test]
    fn bracketed_paste_wraps_verbatim() {
        assert_eq!(
            paste_bytes("a\r\nb\nc", true),
            b"\x1b[200~a\r\nb\nc\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_paste_strips_embedded_terminator() {
        // an embedded terminator must not end the bracket early; it's
        // stripped, and the wrapper still appears exactly once, at the end
        let text = "before\x1b[201~after";
        assert_eq!(
            paste_bytes(text, true),
            b"\x1b[200~beforeafter\x1b[201~".to_vec()
        );
    }

    #[test]
    fn unbracketed_paste_normalizes_newlines_to_cr() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(paste_bytes("plain", false), b"plain".to_vec());
    }

    #[test]
    fn unbracketed_paste_leaves_terminator_look_alike_untouched() {
        // the unbracketed path never wraps in bracket markers at all, so an
        // embedded ESC[201~ isn't a terminator here and is passed through
        let text = "before\x1b[201~after";
        assert_eq!(paste_bytes(text, false), text.as_bytes().to_vec());
    }
}

pub fn dispatch(mode: &Mode, key: &KeyEvent, ctx: &DispatchCtx) -> Action {
    match mode {
        Mode::Control => {
            // Handle Ctrl+Q before the key.code match: matching on code alone
            // would let Ctrl+Q fall into the Char('q') arm and quit the app.
            if is_ctrl_q(key) {
                return if ctx.just_detached {
                    Action::SendLiteralDetachKey
                } else {
                    Action::None
                };
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                KeyCode::Enter if ctx.selected_status.is_some() => Action::Attach,
                KeyCode::Char('n') => Action::OpenNewSession,
                KeyCode::Char('l') | KeyCode::Char('L') => Action::OpenSessionHistory,
                KeyCode::Char('x') => match ctx.selected_status {
                    Some(Status::Exited(_)) => Action::RemoveSelected,
                    Some(_) => Action::EnterConfirmKill,
                    None => Action::None,
                },
                KeyCode::Char('r') => match ctx.selected_status {
                    Some(Status::Exited(_)) => Action::RespawnSelected,
                    _ => Action::None,
                },
                KeyCode::Char('q') => {
                    if ctx.any_working {
                        Action::EnterConfirmQuit
                    } else {
                        Action::Quit
                    }
                }
                _ => Action::None,
            }
        }
        Mode::Attached => {
            if is_ctrl_q(key) {
                Action::Detach
            } else {
                match encode_key(key) {
                    Some(bytes) => Action::ForwardBytes(bytes),
                    None => Action::None,
                }
            }
        }
        Mode::NewSession(_) => Action::DialogKey,
        Mode::SessionHistory(_) => Action::HistoryKey,
        Mode::ConfirmKill => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::KillSelected,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelToControl,
            _ => Action::None,
        },
        Mode::ConfirmQuit => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::Quit,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelToControl,
            _ => Action::None,
        },
    }
}

#[derive(Debug)]
pub enum DialogField {
    Profile,
    Dir,
}

#[derive(Debug)]
pub enum DialogResult {
    Submit,
    Cancel,
    Consumed,
}

/// Resolves a user-provided or profile working directory path:
/// - empty / whitespace / `.` -> current working directory
/// - `~/...` or `~` -> user's home directory joined with the subpath
/// - other paths -> parsed as PathBuf directly
pub fn resolve_working_dir(dir_str: &str) -> std::path::PathBuf {
    let trimmed = dir_str.trim();
    if trimmed.is_empty() || trimmed == "." {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    } else if let Some(stripped) = trimmed.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            std::path::PathBuf::from(home).join(stripped)
        } else {
            std::path::PathBuf::from(trimmed)
        }
    } else if trimmed == "~" {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            std::path::PathBuf::from(home)
        } else {
            std::path::PathBuf::from(trimmed)
        }
    } else {
        std::path::PathBuf::from(trimmed)
    }
}

/// Lists subdirectories of a given directory, including ".." if a parent exists.
pub fn list_subdirectories(dir: &std::path::Path) -> Vec<String> {
    let mut entries = Vec::new();
    if dir.parent().is_some() {
        entries.push("..".to_string());
    }
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        let mut subdirs: Vec<String> = read_dir
            .filter_map(|e| e.ok())
            .filter(|e| {
                let file_name = e.file_name();
                let name = file_name.to_string_lossy();
                !name.starts_with('.') && e.path().is_dir()
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        subdirs.sort_by_key(|a| a.to_lowercase());
        entries.extend(subdirs);
    }
    entries
}

fn default_dir_for_profile(profile: Option<&Profile>) -> String {
    profile
        .and_then(|p| p.default_dir.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".into())
        })
}

#[derive(Debug)]
pub struct DialogState {
    pub profile_idx: usize,
    pub dir: String,
    pub dir_edited: bool,
    pub field: DialogField,
    pub error: Option<String>,
    pub dir_entries: Vec<String>,
    pub dir_selected_idx: Option<usize>,
}

impl DialogState {
    pub fn new(profiles: &[Profile]) -> Self {
        let dir = default_dir_for_profile(profiles.first());
        let resolved = resolve_working_dir(&dir);
        let dir_entries = list_subdirectories(&resolved);
        DialogState {
            profile_idx: 0,
            dir,
            dir_edited: false,
            field: DialogField::Profile,
            error: None,
            dir_entries,
            dir_selected_idx: None,
        }
    }

    pub fn refresh_dir_entries(&mut self) {
        let resolved = resolve_working_dir(&self.dir);
        self.dir_entries = list_subdirectories(&resolved);
        if let Some(idx) = self.dir_selected_idx
            && idx >= self.dir_entries.len()
        {
            self.dir_selected_idx = if self.dir_entries.is_empty() {
                None
            } else {
                Some(self.dir_entries.len() - 1)
            };
        }
    }

    fn set_profile(&mut self, idx: usize, profiles: &[Profile]) {
        self.profile_idx = idx;
        if !self.dir_edited {
            self.dir = default_dir_for_profile(profiles.get(idx));
            self.refresh_dir_entries();
        }
    }

    pub fn navigate_to_parent(&mut self) {
        let resolved = resolve_working_dir(&self.dir);
        if let Some(parent) = resolved.parent() {
            self.dir = parent.to_string_lossy().into_owned();
            self.dir_edited = true;
            self.dir_selected_idx = None;
            self.refresh_dir_entries();
        }
    }

    pub fn navigate_into(&mut self, sub: &str) {
        let resolved = resolve_working_dir(&self.dir);
        let target = resolved.join(sub);
        self.dir = target.to_string_lossy().into_owned();
        self.dir_edited = true;
        self.dir_selected_idx = None;
        self.refresh_dir_entries();
    }

    pub fn handle_key(&mut self, key: &KeyEvent, profiles: &[Profile]) -> DialogResult {
        match key.code {
            KeyCode::Esc => return DialogResult::Cancel,
            KeyCode::Tab | KeyCode::BackTab => {
                self.field = match self.field {
                    DialogField::Profile => DialogField::Dir,
                    DialogField::Dir => DialogField::Profile,
                };
                self.dir_selected_idx = None;
                return DialogResult::Consumed;
            }
            _ => {}
        }

        match self.field {
            DialogField::Profile => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Down | KeyCode::Char('j') => {
                    let next = (self.profile_idx + 1) % profiles.len().max(1);
                    self.set_profile(next, profiles);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let len = profiles.len().max(1);
                    self.set_profile((self.profile_idx + len - 1) % len, profiles);
                }
                _ => {}
            },
            DialogField::Dir => match key.code {
                KeyCode::Enter => {
                    if let Some(idx) = self.dir_selected_idx
                        && let Some(entry) = self.dir_entries.get(idx).cloned()
                    {
                        if entry == ".." {
                            self.navigate_to_parent();
                            return DialogResult::Consumed;
                        } else {
                            self.navigate_into(&entry);
                            return DialogResult::Submit;
                        }
                    }
                    return DialogResult::Submit;
                }
                KeyCode::Right => {
                    if let Some(idx) = self.dir_selected_idx
                        && let Some(entry) = self.dir_entries.get(idx).cloned()
                    {
                        if entry == ".." {
                            self.navigate_to_parent();
                        } else {
                            self.navigate_into(&entry);
                        }
                    } else if !self.dir_entries.is_empty() {
                        self.dir_selected_idx = Some(0);
                    }
                }
                KeyCode::Left => {
                    self.navigate_to_parent();
                }
                KeyCode::Down => {
                    if self.dir_entries.is_empty() {
                        self.dir_selected_idx = None;
                    } else {
                        self.dir_selected_idx = match self.dir_selected_idx {
                            None => Some(0),
                            Some(i) => {
                                Some((i + 1).min(self.dir_entries.len().saturating_sub(1)))
                            }
                        };
                    }
                }
                KeyCode::Up => {
                    if let Some(i) = self.dir_selected_idx {
                        if i == 0 {
                            self.dir_selected_idx = None;
                        } else {
                            self.dir_selected_idx = Some(i - 1);
                        }
                    }
                }
                KeyCode::Char(c) => {
                    self.dir.push(c);
                    self.dir_edited = true;
                    self.dir_selected_idx = None;
                    self.refresh_dir_entries();
                }
                KeyCode::Backspace => {
                    self.dir.pop();
                    self.dir_edited = true;
                    self.dir_selected_idx = None;
                    self.refresh_dir_entries();
                }
                _ => {}
            },
        }
        DialogResult::Consumed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryPane {
    SessionsList,
    LogDetail,
}

#[derive(Debug)]
pub struct HistoryState {
    pub sessions: Vec<SessionSummary>,
    pub selected_session_idx: usize,
    pub log_lines: Vec<ratatui::text::Line<'static>>,
    pub scroll_offset: usize,
    pub focused_pane: HistoryPane,
    pub all_projects: bool,
    pub error: Option<String>,
    pub base_dir: Option<std::path::PathBuf>,
}

impl HistoryState {
    pub fn new(current_dir: Option<&std::path::Path>) -> Self {
        let base_dir = current_dir.map(|p| p.to_path_buf());
        let sessions = history::discover_sessions(None, None, current_dir, false);
        let mut state = HistoryState {
            sessions,
            selected_session_idx: 0,
            log_lines: Vec::new(),
            scroll_offset: 0,
            focused_pane: HistoryPane::SessionsList,
            all_projects: false,
            error: None,
            base_dir,
        };
        state.load_selected_log();
        state
    }

    pub fn load_selected_log(&mut self) {
        self.scroll_offset = 0;
        if let Some(summary) = self.sessions.get(self.selected_session_idx) {
            match history::load_session_log(&summary.file_path) {
                Ok(entries) => {
                    self.log_lines = history::render_log_lines(&entries);
                    self.error = None;
                }
                Err(e) => {
                    self.log_lines = Vec::new();
                    self.error = Some(format!("Failed to load session log: {e}"));
                }
            }
        } else {
            self.log_lines = Vec::new();
        }
    }

    pub fn reload_sessions(&mut self) {
        let cur_ref = self.base_dir.as_deref();
        self.sessions = history::discover_sessions(None, None, cur_ref, self.all_projects);
        self.selected_session_idx = 0;
        self.load_selected_log();
    }
}

/// A local (non-agent) text selection being dragged or held over one
/// session's screen. `session_id` (not an index) so a selection can be
/// recognized as stale once its session is removed/replaced/reordered.
#[derive(Debug)]
pub struct ActiveSelection {
    pub session_id: usize,
    pub sel: Selection,
    pub dragging: bool,
}

/// Who owns an in-progress left-button drag, latched at `Down(Left)` so a
/// modifier change mid-drag (e.g. releasing Shift) can't retarget its later
/// Drag/Up events -- see `handle_mouse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOwner {
    Local,
    Agent,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    pub error: Option<String>,
    pub profiles: Vec<Profile>,
    pub pane_size: (u16, u16), // (rows, cols)
    pub selection: Option<ActiveSelection>,
    pub search: Option<SearchState>,
    /// When false, `copy_to_clipboard`/`paste_into_attached` are no-ops.
    /// Tests that don't specifically exercise the clipboard round-trip set
    /// this to `false` so `cargo test` never touches the real system
    /// clipboard.
    pub clipboard_enabled: bool,
    drag_owner: Option<DragOwner>,
    just_detached: bool,
    next_id: usize,
    tx: Sender<AppEvent>,
    langfuse: Option<crate::langfuse::LangfuseRuntime>,
}

impl App {
    pub fn new(
        profiles: Vec<Profile>,
        langfuse: Option<crate::langfuse::LangfuseRuntime>,
        tx: Sender<AppEvent>,
    ) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            mode: Mode::Control,
            should_quit: false,
            error: None,
            profiles,
            pane_size: (24, 80),
            selection: None,
            search: None,
            clipboard_enabled: true,
            drag_owner: None,
            just_detached: false,
            next_id: 0,
            tx,
            langfuse,
        }
    }

    /// Spawns a session with Langfuse launch extras applied and the tracing
    /// pipeline attached on success. The single spawn path for all three
    /// call sites (dialog, resume, respawn); with tracing off it degrades to
    /// a plain `Session::spawn`.
    fn spawn_traced(
        &mut self,
        id: usize,
        profile: Profile,
        dir: std::path::PathBuf,
    ) -> anyhow::Result<Session> {
        let (rows, cols) = self.pane_size;
        let plan = self
            .langfuse
            .as_ref()
            .and_then(|rt| rt.plan_launch(&profile, &dir));
        let (extra_args, extra_env): (&[String], &[(String, String)]) = match &plan {
            Some(p) => (&p.extra_args, &p.extra_env),
            None => (&[], &[]),
        };
        let mut session =
            Session::spawn(id, profile, dir, rows, cols, self.tx.clone(), extra_args, extra_env)?;
        if let (Some(rt), Some(plan)) = (self.langfuse.as_mut(), plan) {
            session.trace = Some(rt.start_session(id, plan));
        }
        Ok(session)
    }

    /// Hands the runtime back to `main` for the post-`kill_all` bounded
    /// shutdown flush.
    pub fn take_langfuse(&mut self) -> Option<crate::langfuse::LangfuseRuntime> {
        self.langfuse.take()
    }

    pub fn attached(&self) -> Option<usize> {
        matches!(self.mode, Mode::Attached).then_some(self.selected)
    }

    fn session_index(&self, id: usize) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// The selection, but only if it belongs to the session currently shown
    /// in the main pane -- selections on removed/replaced/other sessions
    /// are treated as gone.
    pub fn displayed_selection(&self) -> Option<&Selection> {
        let shown = self.sessions.get(self.selected)?.id;
        self.selection
            .as_ref()
            .filter(|a| a.session_id == shown)
            .map(|a| &a.sel)
    }

    fn copy_to_clipboard(&mut self, text: String) {
        if text.is_empty() || !self.clipboard_enabled {
            return;
        }
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            self.error = Some(format!("clipboard: {e}"));
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.displayed_selection().copied() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        let text = selection::extract_text(&mut s.parser, len, &sel);
        self.copy_to_clipboard(text);
    }

    fn paste_into_attached(&mut self) {
        if !matches!(self.mode, Mode::Attached) {
            return;
        }
        if !self.clipboard_enabled {
            return;
        }
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(format!("clipboard: {e}"));
                return;
            }
        };
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        let bytes = paste_bytes(&text, s.parser.screen().bracketed_paste());
        self.snap_selected_to_live();
        self.forward_bytes(&bytes);
    }

    pub fn handle_key(&mut self, key: &KeyEvent, now: Instant) {
        self.error = None;
        if self.search.is_some() {
            self.handle_search_key(key);
            return;
        }
        if self.handle_ux_key(key) {
            self.just_detached = false;
            return;
        }
        let ctx = DispatchCtx {
            selected_status: self.sessions.get(self.selected).map(|s| s.status(now)),
            any_working: self
                .sessions
                .iter()
                .any(|s| matches!(s.status(now), Status::Working)),
            just_detached: self.just_detached,
        };
        let action = dispatch(&self.mode, key, &ctx);
        // any Control-mode key other than the literal-send consumes the flag
        if !matches!(action, Action::Detach | Action::SendLiteralDetachKey) {
            self.just_detached = false;
        }
        self.apply(action, key, now);
    }

    /// Terminal-emulator chords intercepted before v1 dispatch (Ghostty /
    /// Windows Terminal convention: the app reserves Ctrl+Shift and
    /// Shift+navigation for itself; everything else still reaches the
    /// agent). Returns true if the key was consumed. Tasks: selection
    /// (Ctrl+Shift+C/V) and search (Ctrl+Shift+F) add arms here.
    fn handle_ux_key(&mut self, key: &KeyEvent) -> bool {
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        enum Chord {
            PageUp,
            PageDown,
            Top,
            Bottom,
            Copy,
            Paste,
            OpenSearch,
        }
        let chord = match (key.code, shift, ctrl) {
            (KeyCode::PageUp, true, _) => Chord::PageUp,
            (KeyCode::PageDown, true, _) => Chord::PageDown,
            (KeyCode::Home, true, _) => Chord::Top,
            (KeyCode::End, true, _) => Chord::Bottom,
            (KeyCode::Char('c') | KeyCode::Char('C'), true, true) => Chord::Copy,
            (KeyCode::Char('v') | KeyCode::Char('V'), true, true) => Chord::Paste,
            (KeyCode::Char('f') | KeyCode::Char('F'), true, true) => Chord::OpenSearch,
            // plain Ctrl+F only when nothing is forwarded (Control mode)
            (KeyCode::Char('f') | KeyCode::Char('F'), false, true)
                if matches!(self.mode, Mode::Control) =>
            {
                Chord::OpenSearch
            }
            _ => return false,
        };
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        match chord {
            Chord::PageUp => self.scroll_selected(page),
            Chord::PageDown => self.scroll_selected(-page),
            Chord::Top => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_top();
                }
            }
            Chord::Bottom => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            Chord::Copy => self.copy_selection(),
            Chord::Paste => self.paste_into_attached(),
            Chord::OpenSearch => {
                self.search = Some(SearchState::new());
            }
        }
        true
    }

    fn handle_search_key(&mut self, key: &KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => {
                self.search = None;
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            KeyCode::Enter => {
                if let Some(st) = self.search.as_mut() {
                    if shift {
                        st.prev();
                    } else {
                        st.next();
                    }
                }
                self.scroll_to_current_match();
            }
            KeyCode::Backspace => {
                if let Some(st) = self.search.as_mut() {
                    st.query.pop();
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            KeyCode::Char(c) => {
                // Ctrl-modified chars (e.g. Ctrl+Shift+F reopening the
                // chord, or any other Ctrl+letter) must not pollute the
                // query -- only plain/Shift'd character input edits it.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                if let Some(st) = self.search.as_mut() {
                    st.query.push(c);
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            _ => {}
        }
    }

    fn rerun_search(&mut self) {
        let Some(st) = self.search.as_mut() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        st.run(&mut s.parser, len);
    }

    fn scroll_to_current_match(&mut self) {
        let Some(row) = self
            .search
            .as_ref()
            .and_then(|st| st.current_match().map(|m| m.row))
        else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        // live rows need no scrolling; scrollback rows go to the top of view
        let offset = len.saturating_sub(row);
        s.set_scroll(offset);
    }

    fn scroll_selected(&mut self, delta: i32) {
        if let Some(s) = self.sessions.get_mut(self.selected) {
            s.scroll_by(delta);
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, _now: Instant) {
        if let Mode::SessionHistory(ref mut history) = self.mode {
            if matches!(ev.kind, MouseEventKind::ScrollUp) {
                history.scroll_offset = history.scroll_offset.saturating_sub(3);
            } else if matches!(ev.kind, MouseEventKind::ScrollDown) {
                history.scroll_offset = history.scroll_offset.saturating_add(3);
            }
            return;
        }
        // Mouse handling is scoped to Control (sidebar preview scroll) and
        // Attached (the pane showing a live child): during the NewSession
        // dialog or a Confirm prompt the pane underneath must not react to
        // clicks/drags/wheel meant for the dialog.
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return;
        }
        // A live left-button drag must be able to finish even if the
        // terminating Drag/Up event lands outside the pane (e.g. the mouse
        // slid into the adjacent sidebar): clamp into the pane instead of
        // dropping the event, so the drag can't get stranded with
        // `dragging: true` and a frozen highlight. Any other out-of-pane
        // event is still dropped -- the sidebar stays keyboard-driven.
        // Keyed to an active LOCAL drag: `self.selection`'s `dragging` flag
        // is only ever set true from the local (non-agent-owned) branch
        // below, so this is already local-drag-specific.
        let dragging = self.selection.as_ref().is_some_and(|a| a.dragging);
        let finalizing_drag = dragging
            && matches!(
                ev.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            );
        let (lcol, lrow) = match ui::pane_local(ev.column, ev.row, self.pane_size) {
            Some(p) => p,
            None if finalizing_drag => ui::pane_clamped(ev.column, ev.row, self.pane_size),
            None => return,
        };
        let attached = matches!(self.mode, Mode::Attached);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        // read child terminal state up front so no borrow is held across
        // the mutating calls below
        let Some((mouse_mode, enc, alt, app_cursor)) = self.sessions.get(self.selected).map(|s| {
            let sc = s.parser.screen();
            (
                sc.mouse_protocol_mode(),
                sc.mouse_protocol_encoding(),
                sc.alternate_screen(),
                sc.application_cursor(),
            )
        }) else {
            return;
        };
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                match route_wheel(shift, attached, mouse_mode, alt) {
                    WheelRoute::Local => {
                        if let Some(s) = self.sessions.get_mut(self.selected) {
                            s.scroll_by(if up { 3 } else { -3 });
                        }
                    }
                    WheelRoute::Forward => {
                        if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                            self.forward_bytes(&bytes);
                        }
                    }
                    WheelRoute::Arrows => {
                        let seq: &[u8] = match (up, app_cursor) {
                            (true, false) => b"\x1b[A",
                            (true, true) => b"\x1bOA",
                            (false, false) => b"\x1b[B",
                            (false, true) => b"\x1bOB",
                        };
                        self.forward_bytes(&seq.repeat(3));
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left) => {
                let is_up = matches!(ev.kind, MouseEventKind::Up(_));
                // Ownership is decided fresh at Down (agent owns the mouse
                // when attached + it asked for events, unless Shift forces
                // local selection -- the iTerm2 rule) and then latched in
                // `drag_owner` for the rest of the drag. Without this, a
                // modifier change mid-drag (e.g. releasing Shift) would let
                // this same computation -- re-run per event, from that
                // event's own shift flag -- retarget the Drag/Up to the
                // child, stranding a local drag with `dragging: true` still
                // set (and, worse, a later unrelated out-of-pane Up could
                // then clamp and finalize that stale selection). Latching
                // at Down means Drag/Up always follow where the drag
                // started, regardless of the shift flag on those events.
                let agent_owns = match ev.kind {
                    MouseEventKind::Down(_) => {
                        let owns =
                            attached && !shift && mouse_mode != vt100::MouseProtocolMode::None;
                        self.drag_owner = Some(if owns {
                            DragOwner::Agent
                        } else {
                            DragOwner::Local
                        });
                        owns
                    }
                    _ => matches!(self.drag_owner, Some(DragOwner::Agent)),
                };
                if agent_owns {
                    if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                        self.forward_bytes(&bytes);
                    }
                } else if let Some(s) = self.sessions.get(self.selected) {
                    let (len, offset) = s.scroll_view();
                    let session_id = s.id;
                    let pos = Pos {
                        row: selection::abs_row(len, offset, lrow),
                        col: lcol,
                    };
                    match ev.kind {
                        MouseEventKind::Down(_) => {
                            self.selection = Some(ActiveSelection {
                                session_id,
                                sel: Selection::new(pos),
                                dragging: true,
                            });
                        }
                        MouseEventKind::Drag(_) => {
                            if let Some(a) = self.selection.as_mut().filter(|a| a.dragging) {
                                a.sel.head = pos;
                            }
                        }
                        _ => {
                            // release: apply the final position (a
                            // throttled terminal may not have sent a Drag
                            // for it), finish the drag, then copy-on-select
                            // or clear
                            let finished = self.selection.take_if(|a| a.dragging);
                            if let Some(mut a) = finished {
                                a.sel.head = pos;
                                a.dragging = false;
                                if a.sel.is_empty() {
                                    // plain click: selection stays cleared
                                } else {
                                    self.selection = Some(a);
                                    self.copy_selection();
                                }
                            }
                        }
                    }
                }
                if is_up {
                    self.drag_owner = None;
                }
            }
            _ => {
                // other buttons: forward when the agent owns the mouse
                if attached
                    && !shift
                    && let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc)
                {
                    self.forward_bytes(&bytes);
                }
            }
        }
    }

    fn apply(&mut self, action: Action, key: &KeyEvent, _now: Instant) {
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::EnterConfirmQuit => self.mode = Mode::ConfirmQuit,
            Action::MoveDown => {
                self.selection = None;
                if !self.sessions.is_empty() {
                    self.selected = (self.selected + 1).min(self.sessions.len() - 1);
                }
            }
            Action::MoveUp => {
                self.selection = None;
                self.selected = self.selected.saturating_sub(1);
            }
            Action::Attach => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.tracker.on_attach();
                    self.mode = Mode::Attached;
                }
            }
            Action::Detach => {
                self.mode = Mode::Control;
                self.just_detached = true;
            }
            Action::SendLiteralDetachKey => {
                self.just_detached = false;
                self.snap_selected_to_live();
                // Only re-attach if the literal Ctrl+Q actually made it to
                // the pty -- a failed write already dropped us to Control
                // via forward_bytes (spec: write failure -> error + Exited
                // + Control), and re-attaching here would override that.
                if self.forward_bytes(&[0x11])
                    && let Some(s) = self.sessions.get_mut(self.selected)
                {
                    s.tracker.on_attach();
                    self.mode = Mode::Attached;
                }
            }
            Action::ForwardBytes(bytes) => {
                self.snap_selected_to_live();
                self.forward_bytes(&bytes);
            }
            Action::OpenNewSession => {
                self.mode = Mode::NewSession(DialogState::new(&self.profiles));
            }
            Action::EnterConfirmKill => self.mode = Mode::ConfirmKill,
            Action::KillSelected => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.kill();
                }
                self.mode = Mode::Control;
            }
            Action::RemoveSelected => {
                self.selection = None;
                if self.selected < self.sessions.len() {
                    self.sessions.remove(self.selected);
                    if self.selected >= self.sessions.len() {
                        self.selected = self.sessions.len().saturating_sub(1);
                    }
                }
            }
            Action::OpenSessionHistory => {
                let cur_dir = self
                    .sessions
                    .get(self.selected)
                    .map(|s| s.dir.clone())
                    .or_else(|| std::env::current_dir().ok());
                self.mode = Mode::SessionHistory(HistoryState::new(cur_dir.as_deref()));
            }
            Action::HistoryKey => self.handle_history_key(key),
            Action::RespawnSelected => {
                self.selection = None;
                self.respawn_selected();
            }
            Action::CancelToControl => self.mode = Mode::Control,
            Action::DialogKey => self.handle_dialog_key(key),
        }
    }

    /// Writes `bytes` to the selected session. Returns `true` if the write
    /// succeeded (or there was no selected session to write to -- a no-op
    /// counts as success for callers deciding whether to proceed), `false`
    /// if the write failed. On failure this already applies the full spec
    /// consequence (status-bar error, session marked Exited, drop to
    /// Control) -- callers must not re-attach or otherwise treat the
    /// session as still live when this returns `false`.
    fn forward_bytes(&mut self, bytes: &[u8]) -> bool {
        if let Some(s) = self.sessions.get_mut(self.selected)
            && let Err(e) = s.write_bytes(bytes)
        {
            // spec: write failure -> status-bar error, session Exited
            self.error = Some(format!("write to '{}' failed: {e}", s.profile.name));
            s.tracker.on_exit(None);
            // this path marks Exited with no PtyExit event ever arriving —
            // the tracing pipeline must still learn about it
            if let Some(trace) = &s.trace {
                trace.mark_exited(None);
            }
            self.mode = Mode::Control;
            return false;
        }
        true
    }

    /// Spec: any keystroke forwarded while scrolled first snaps the view
    /// back to the live bottom, like every terminal emulator.
    fn snap_selected_to_live(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.selected)
            && s.scrolled() > 0
        {
            s.scroll_to_bottom();
        }
    }

    fn handle_dialog_key(&mut self, key: &KeyEvent) {
        let Mode::NewSession(dialog) = &mut self.mode else {
            return;
        };
        match dialog.handle_key(key, &self.profiles) {
            DialogResult::Consumed => {}
            DialogResult::Cancel => self.mode = Mode::Control,
            DialogResult::Submit => {
                let profile = match self.profiles.get(dialog.profile_idx) {
                    Some(p) => p.clone(),
                    None => return,
                };
                let dir = resolve_working_dir(&dialog.dir);
                let id = self.next_id;
                match self.spawn_traced(id, profile, dir) {
                    Ok(session) => {
                        self.next_id += 1;
                        self.sessions.push(session);
                        self.selected = self.sessions.len() - 1;
                        self.mode = Mode::Control;
                    }
                    Err(e) => {
                        let Mode::NewSession(dialog) = &mut self.mode else {
                            return;
                        };
                        dialog.error = Some(e.to_string());
                    }
                }
            }
        }
    }

    fn handle_history_key(&mut self, key: &KeyEvent) {
        let Mode::SessionHistory(history) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Control;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                history.focused_pane = match history.focused_pane {
                    HistoryPane::SessionsList => HistoryPane::LogDetail,
                    HistoryPane::LogDetail => HistoryPane::SessionsList,
                };
            }
            KeyCode::Left => {
                history.focused_pane = HistoryPane::SessionsList;
            }
            KeyCode::Right => {
                history.focused_pane = HistoryPane::LogDetail;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                history.all_projects = !history.all_projects;
                history.reload_sessions();
            }
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                if let Some(summary) = history.sessions.get(history.selected_session_idx).cloned() {
                    self.resume_history_session(&summary);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => match history.focused_pane {
                HistoryPane::SessionsList => {
                    if !history.sessions.is_empty() {
                        let next = (history.selected_session_idx + 1).min(history.sessions.len() - 1);
                        if next != history.selected_session_idx {
                            history.selected_session_idx = next;
                            history.load_selected_log();
                        }
                    }
                }
                HistoryPane::LogDetail => {
                    if !history.log_lines.is_empty() {
                        history.scroll_offset = history.scroll_offset.saturating_add(1);
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match history.focused_pane {
                HistoryPane::SessionsList => {
                    if history.selected_session_idx > 0 {
                        history.selected_session_idx -= 1;
                        history.load_selected_log();
                    }
                }
                HistoryPane::LogDetail => {
                    history.scroll_offset = history.scroll_offset.saturating_sub(1);
                }
            },
            KeyCode::PageDown => {
                history.scroll_offset = history.scroll_offset.saturating_add(15);
            }
            KeyCode::PageUp => {
                history.scroll_offset = history.scroll_offset.saturating_sub(15);
            }
            KeyCode::Home => {
                history.scroll_offset = 0;
            }
            KeyCode::End => {
                history.scroll_offset = history.log_lines.len().saturating_sub(1);
            }
            _ => {}
        }
    }

    fn resume_history_session(&mut self, summary: &SessionSummary) {
        let profile = match summary.provider {
            crate::history::AgentProvider::Claude => {
                let p = self
                    .profiles
                    .iter()
                    .find(|p| p.command == "claude" || p.name.to_lowercase().contains("claude"))
                    .cloned()
                    .unwrap_or_else(|| Profile {
                        name: "Claude Code".into(),
                        command: "claude".into(),
                        args: vec![],
                        default_dir: None,
                        langfuse: None,
                    });
                let mut resume_profile = p;
                resume_profile.args = vec!["--resume".into(), summary.session_id.clone()];
                resume_profile
            }
            crate::history::AgentProvider::Antigravity => {
                let p = self
                    .profiles
                    .iter()
                    .find(|p| {
                        p.command == "agy"
                            || p.name.to_lowercase().contains("antigravity")
                            || p.name.to_lowercase().contains("agy")
                    })
                    .cloned()
                    .unwrap_or_else(|| Profile {
                        name: "Antigravity".into(),
                        command: "agy".into(),
                        args: vec![],
                        default_dir: None,
                        langfuse: None,
                    });
                // `agy` alone starts a NEW conversation; resuming requires
                // the id (this also makes resume correlation deterministic
                // for Langfuse).
                let mut resume_profile = p;
                resume_profile.args = vec!["--conversation".into(), summary.session_id.clone()];
                resume_profile
            }
        };

        let dir = self
            .sessions
            .get(self.selected)
            .map(|s| s.dir.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let id = self.next_id;
        match self.spawn_traced(id, profile, dir) {
            Ok(session) => {
                self.next_id += 1;
                self.sessions.push(session);
                self.selected = self.sessions.len() - 1;
                self.mode = Mode::Control;
            }
            Err(e) => {
                self.error = Some(format!("Failed to resume session: {e}"));
                self.mode = Mode::Control;
            }
        }
    }

    fn respawn_selected(&mut self) {
        let Some(old) = self.sessions.get(self.selected) else {
            return;
        };
        let profile = old.profile.clone();
        let dir = old.dir.clone();
        // Fresh id: a stale reader thread from the dead session may still
        // send PtyExit for the old id; it must not find the new session.
        // Launch extras are re-planned fresh too (never stored in the
        // profile), so a respawned Claude tab gets a NEW session uuid.
        let id = self.next_id;
        match self.spawn_traced(id, profile, dir) {
            Ok(session) => {
                self.next_id += 1;
                self.sessions[self.selected] = session;
            }
            Err(e) => self.error = Some(format!("respawn failed: {e}")),
        }
    }

    pub fn handle_pty_output(&mut self, id: usize, bytes: &[u8], now: Instant) {
        let focused = self
            .attached()
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.id)
            == Some(id);
        if let Some(i) = self.session_index(id) {
            self.sessions[i].process_output(bytes, now, focused);
        }
        if self.search.is_some() && self.sessions.get(self.selected).map(|s| s.id) == Some(id) {
            self.rerun_search();
        }
    }

    pub fn handle_pty_exit(&mut self, id: usize) {
        if let Some(i) = self.session_index(id) {
            self.sessions[i].mark_exited();
            // notify the tracing pipeline (idempotent against the
            // documented duplicate PtyExit) with the exit code now known
            if let Some(trace) = &self.sessions[i].trace {
                let code = match self.sessions[i].status(Instant::now()) {
                    Status::Exited(code) => code,
                    _ => None,
                };
                trace.mark_exited(code);
            }
            // if we were attached to it, drop back to Control
            if self.attached() == Some(i) {
                self.mode = Mode::Control;
            }
        }
    }

    pub fn set_pane_size(&mut self, rows: u16, cols: u16) {
        if self.pane_size == (rows, cols) {
            return;
        }
        self.pane_size = (rows, cols);
        for s in &mut self.sessions {
            s.resize(rows, cols);
        }
    }

    pub fn kill_all(&mut self) {
        // Kill unconditionally, even for sessions whose status is already
        // Exited: forward_bytes marks a session Exited(None) on a write
        // failure without killing the child, so skipping "Exited" sessions
        // here could leave a still-live child running past app quit.
        // Session::kill() already swallows errors on a dead/already-exited
        // child, so calling it again here is harmless.
        for s in &mut self.sessions {
            s.kill();
        }
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::status::Status;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_q() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
    }

    fn ctx(selected: Option<Status>) -> DispatchCtx {
        DispatchCtx {
            selected_status: selected,
            any_working: false,
            just_detached: false,
        }
    }

    #[test]
    fn control_navigation() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('j')), &c),
            Action::MoveDown
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Down), &c),
            Action::MoveDown
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('k')), &c),
            Action::MoveUp
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Up), &c),
            Action::MoveUp
        ));
    }

    #[test]
    fn control_attach_and_new() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Enter), &c),
            Action::Attach
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('n')), &c),
            Action::OpenNewSession
        ));
    }

    #[test]
    fn enter_with_no_sessions_is_noop() {
        let c = ctx(None);
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Enter), &c),
            Action::None
        ));
    }

    #[test]
    fn x_confirms_kill_when_running_removes_when_exited() {
        let running = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('x')), &running),
            Action::EnterConfirmKill
        ));
        let exited = ctx(Some(Status::Exited(Some(0))));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('x')), &exited),
            Action::RemoveSelected
        ));
    }

    #[test]
    fn r_respawns_only_exited() {
        let exited = ctx(Some(Status::Exited(Some(1))));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('r')), &exited),
            Action::RespawnSelected
        ));
        let running = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('r')), &running),
            Action::None
        ));
    }

    #[test]
    fn q_quits_directly_unless_something_is_working() {
        let quiet = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('q')), &quiet),
            Action::Quit
        ));
        let mut busy = ctx(Some(Status::Idle));
        busy.any_working = true;
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('q')), &busy),
            Action::EnterConfirmQuit
        ));
    }

    #[test]
    fn attached_ctrl_q_detaches_everything_else_forwards() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Attached, &ctrl_q(), &c),
            Action::Detach
        ));
        match dispatch(&Mode::Attached, &key(KeyCode::Char('a')), &c) {
            Action::ForwardBytes(b) => assert_eq!(b, b"a".to_vec()),
            other => panic!("expected ForwardBytes, got {other:?}"),
        }
        // unencodable keys are swallowed, not errors
        assert!(matches!(
            dispatch(&Mode::Attached, &key(KeyCode::CapsLock), &c),
            Action::None
        ));
    }

    #[test]
    fn double_ctrl_q_sends_literal() {
        let mut c = ctx(Some(Status::Working));
        c.just_detached = true;
        assert!(matches!(
            dispatch(&Mode::Control, &ctrl_q(), &c),
            Action::SendLiteralDetachKey
        ));
        // without the flag, Ctrl+Q in Control does nothing
        c.just_detached = false;
        assert!(matches!(
            dispatch(&Mode::Control, &ctrl_q(), &c),
            Action::None
        ));
    }

    #[test]
    fn confirm_modes() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('y')), &c),
            Action::KillSelected
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Enter), &c),
            Action::KillSelected
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Esc), &c),
            Action::CancelToControl
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('n')), &c),
            Action::CancelToControl
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmQuit, &key(KeyCode::Char('y')), &c),
            Action::Quit
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmQuit, &key(KeyCode::Esc), &c),
            Action::CancelToControl
        ));
    }

    #[test]
    fn new_session_mode_routes_to_dialog() {
        let mode = Mode::NewSession(DialogState::new(&crate::config::Config::default_profiles()));
        let c = ctx(None);
        assert!(matches!(
            dispatch(&mode, &key(KeyCode::Char('a')), &c),
            Action::DialogKey
        ));
    }
}

#[cfg(test)]
mod dialog_tests {
    use super::*;
    use crate::config::Profile;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "A".into(),
                command: "a".into(),
                args: vec![],
                default_dir: Some("C:\\one".into()),
                langfuse: None,
            },
            Profile {
                name: "B".into(),
                command: "b".into(),
                args: vec![],
                default_dir: Some("C:\\two".into()),
                langfuse: None,
            },
        ]
    }

    #[test]
    fn new_dialog_prefills_first_profiles_default_dir() {
        let d = DialogState::new(&profiles());
        assert_eq!(d.profile_idx, 0);
        assert_eq!(d.dir, "C:\\one");
        assert!(matches!(d.field, DialogField::Profile));
    }

    #[test]
    fn cycling_profile_updates_untouched_dir() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.profile_idx, 1);
        assert_eq!(d.dir, "C:\\two");
    }

    #[test]
    fn edited_dir_survives_profile_cycling() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Tab), &ps); // to Dir field
        d.handle_key(&key(KeyCode::Char('X')), &ps);
        d.handle_key(&key(KeyCode::Tab), &ps); // back to Profile
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.dir, "C:\\oneX");
    }

    #[test]
    fn typing_and_backspace_edit_dir() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Tab), &ps);
        d.handle_key(&key(KeyCode::Char('Z')), &ps);
        d.handle_key(&key(KeyCode::Backspace), &ps);
        assert_eq!(d.dir, "C:\\one");
    }

    #[test]
    fn enter_submits_esc_cancels() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        assert!(matches!(
            d.handle_key(&key(KeyCode::Enter), &ps),
            DialogResult::Submit
        ));
        assert!(matches!(
            d.handle_key(&key(KeyCode::Esc), &ps),
            DialogResult::Cancel
        ));
        assert!(matches!(
            d.handle_key(&key(KeyCode::Char('q')), &ps),
            DialogResult::Consumed
        ));
    }

    #[test]
    fn new_dialog_prefills_current_dir_when_no_default_dir() {
        let profiles = vec![Profile {
            name: "Default".into(),
            command: "claude".into(),
            args: vec![],
            default_dir: None,
            langfuse: None,
        }];
        let d = DialogState::new(&profiles);
        assert_eq!(d.profile_idx, 0);
        let expected = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into());
        assert_eq!(d.dir, expected);
    }

    #[test]
    fn resolve_working_dir_empty_and_tilde() {
        let cur = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        assert_eq!(resolve_working_dir(""), cur);
        assert_eq!(resolve_working_dir("   "), cur);
        assert_eq!(resolve_working_dir("."), cur);
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(resolve_working_dir("~"), std::path::PathBuf::from(&home));
            assert_eq!(
                resolve_working_dir("~/test"),
                std::path::PathBuf::from(&home).join("test")
            );
        }
    }

    #[test]
    fn directory_navigation_and_selection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        std::fs::create_dir(root.join("sub_a")).unwrap();
        std::fs::create_dir(root.join("sub_b")).unwrap();

        let profiles = vec![Profile {
            name: "Test".into(),
            command: "sh".into(),
            args: vec![],
            default_dir: Some(root.to_string_lossy().into_owned()),
            langfuse: None,
        }];

        let mut d = DialogState::new(&profiles);
        d.handle_key(&key(KeyCode::Tab), &profiles); // switch to Dir field
        assert!(matches!(d.field, DialogField::Dir));
        assert!(d.dir_entries.contains(&"sub_a".to_string()));
        assert!(d.dir_entries.contains(&"sub_b".to_string()));

        // Down arrow selects first entry
        d.handle_key(&key(KeyCode::Down), &profiles);
        assert_eq!(d.dir_selected_idx, Some(0));

        // Up arrow goes back to text field
        d.handle_key(&key(KeyCode::Up), &profiles);
        assert_eq!(d.dir_selected_idx, None);

        // Find index of sub_a
        let sub_a_idx = d.dir_entries.iter().position(|e| e == "sub_a").unwrap();
        d.dir_selected_idx = Some(sub_a_idx);

        // Right arrow descends into sub_a
        d.handle_key(&key(KeyCode::Right), &profiles);
        assert_eq!(
            std::path::PathBuf::from(&d.dir),
            root.join("sub_a")
        );

        // Left arrow goes back up to root
        d.handle_key(&key(KeyCode::Left), &profiles);
        assert_eq!(
            std::path::PathBuf::from(&d.dir),
            root
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctx() -> DispatchCtx {
        DispatchCtx {
            selected_status: None,
            any_working: false,
            just_detached: false,
        }
    }

    #[test]
    fn l_in_control_opens_history() {
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('l')), &ctx()),
            Action::OpenSessionHistory
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('L')), &ctx()),
            Action::OpenSessionHistory
        ));
    }

    #[test]
    fn history_mode_dispatches_history_key() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        app.mode = Mode::SessionHistory(HistoryState::new(None));
        assert!(matches!(
            dispatch(&app.mode, &key(KeyCode::Tab), &ctx()),
            Action::HistoryKey
        ));
    }

    #[test]
    fn history_navigation_and_tab() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        let mut hist = HistoryState::new(None);
        hist.log_lines = vec![ratatui::text::Line::raw("line1"), ratatui::text::Line::raw("line2")];
        app.mode = Mode::SessionHistory(hist);

        // Tab toggles to LogDetail
        app.handle_key(&key(KeyCode::Tab), Instant::now());
        if let Mode::SessionHistory(ref h) = app.mode {
            assert_eq!(h.focused_pane, HistoryPane::LogDetail);
        } else {
            panic!("Expected SessionHistory mode");
        }

        // Down in LogDetail scrolls
        app.handle_key(&key(KeyCode::Down), Instant::now());
        if let Mode::SessionHistory(ref h) = app.mode {
            assert_eq!(h.scroll_offset, 1);
        }

        // Esc returns to Control
        app.handle_key(&key(KeyCode::Esc), Instant::now());
        assert!(matches!(app.mode, Mode::Control));
    }
}

