use crate::config::Profile;
use crate::events::AppEvent;
use crate::keys::encode_key;
use crate::mouse::{WheelRoute, encode_mouse, route_wheel};
use crate::session::Session;
use crate::status::Status;
use crate::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use std::time::Instant;
use tokio::sync::mpsc::Sender;

#[derive(Debug)]
pub enum Mode {
    Control,
    Attached,
    NewSession(DialogState),
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
    KillSelected,
    EnterConfirmKill,
    RemoveSelected,
    RespawnSelected,
    CancelToControl,
    ForwardBytes(Vec<u8>),
    SendLiteralDetachKey,
    /// NewSession mode: App routes the key to the DialogState it owns.
    DialogKey,
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

#[derive(Debug)]
pub struct DialogState {
    pub profile_idx: usize,
    pub dir: String,
    pub dir_edited: bool,
    pub field: DialogField,
    pub error: Option<String>,
}

impl DialogState {
    pub fn new(profiles: &[Profile]) -> Self {
        let dir = profiles
            .first()
            .and_then(|p| p.default_dir.clone())
            .unwrap_or_default();
        DialogState {
            profile_idx: 0,
            dir,
            dir_edited: false,
            field: DialogField::Profile,
            error: None,
        }
    }

    fn set_profile(&mut self, idx: usize, profiles: &[Profile]) {
        self.profile_idx = idx;
        if !self.dir_edited
            && let Some(d) = profiles.get(idx).and_then(|p| p.default_dir.clone())
        {
            self.dir = d;
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent, profiles: &[Profile]) -> DialogResult {
        match key.code {
            KeyCode::Enter => return DialogResult::Submit,
            KeyCode::Esc => return DialogResult::Cancel,
            KeyCode::Tab | KeyCode::BackTab => {
                self.field = match self.field {
                    DialogField::Profile => DialogField::Dir,
                    DialogField::Dir => DialogField::Profile,
                };
            }
            _ => match self.field {
                DialogField::Profile => match key.code {
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
                    KeyCode::Char(c) => {
                        self.dir.push(c);
                        self.dir_edited = true;
                    }
                    KeyCode::Backspace => {
                        self.dir.pop();
                        self.dir_edited = true;
                    }
                    _ => {}
                },
            },
        }
        DialogResult::Consumed
    }
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    pub error: Option<String>,
    pub profiles: Vec<Profile>,
    pub pane_size: (u16, u16), // (rows, cols)
    just_detached: bool,
    next_id: usize,
    tx: Sender<AppEvent>,
}

impl App {
    pub fn new(profiles: Vec<Profile>, tx: Sender<AppEvent>) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            mode: Mode::Control,
            should_quit: false,
            error: None,
            profiles,
            pane_size: (24, 80),
            just_detached: false,
            next_id: 0,
            tx,
        }
    }

    pub fn attached(&self) -> Option<usize> {
        matches!(self.mode, Mode::Attached).then_some(self.selected)
    }

    fn session_index(&self, id: usize) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    pub fn handle_key(&mut self, key: &KeyEvent, now: Instant) {
        self.error = None;
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
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return false;
        };
        match (key.code, shift) {
            (KeyCode::PageUp, true) => {
                s.scroll_by(page);
                true
            }
            (KeyCode::PageDown, true) => {
                s.scroll_by(-page);
                true
            }
            (KeyCode::Home, true) => {
                s.scroll_to_top();
                true
            }
            (KeyCode::End, true) => {
                s.scroll_to_bottom();
                true
            }
            _ => false,
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, _now: Instant) {
        let Some((lcol, lrow)) = ui::pane_local(ev.column, ev.row, self.pane_size) else {
            return; // outside the main pane: sidebar stays keyboard-driven in this iteration
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
            _ => {
                // Press/drag/release: when attached and the agent asked for
                // mouse events (and Shift isn't overriding), the agent owns
                // the mouse. Local selection arrives in a later task.
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
                if !self.sessions.is_empty() {
                    self.selected = (self.selected + 1).min(self.sessions.len() - 1);
                }
            }
            Action::MoveUp => self.selected = self.selected.saturating_sub(1),
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
                if self.selected < self.sessions.len() {
                    self.sessions.remove(self.selected);
                    if self.selected >= self.sessions.len() {
                        self.selected = self.sessions.len().saturating_sub(1);
                    }
                }
            }
            Action::RespawnSelected => self.respawn_selected(),
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
                let dir = std::path::PathBuf::from(dialog.dir.clone());
                let (rows, cols) = self.pane_size;
                let id = self.next_id;
                match Session::spawn(id, profile, dir, rows, cols, self.tx.clone()) {
                    Ok(session) => {
                        self.next_id += 1;
                        self.sessions.push(session);
                        self.selected = self.sessions.len() - 1;
                        self.mode = Mode::Control;
                    }
                    Err(e) => dialog.error = Some(e.to_string()),
                }
            }
        }
    }

    fn respawn_selected(&mut self) {
        let Some(old) = self.sessions.get(self.selected) else {
            return;
        };
        let (rows, cols) = self.pane_size;
        // Fresh id: a stale reader thread from the dead session may still
        // send PtyExit for the old id; it must not find the new session.
        let id = self.next_id;
        match Session::spawn(
            id,
            old.profile.clone(),
            old.dir.clone(),
            rows,
            cols,
            self.tx.clone(),
        ) {
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
    }

    pub fn handle_pty_exit(&mut self, id: usize) {
        if let Some(i) = self.session_index(id) {
            self.sessions[i].mark_exited();
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
            },
            Profile {
                name: "B".into(),
                command: "b".into(),
                args: vec![],
                default_dir: Some("C:\\two".into()),
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
}
