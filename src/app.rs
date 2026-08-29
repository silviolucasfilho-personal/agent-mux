use crate::config::Profile;
use crate::keys::encode_key;
use crate::status::Status;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
        DialogState { profile_idx: 0, dir, dir_edited: false, field: DialogField::Profile, error: None }
    }

    fn set_profile(&mut self, idx: usize, profiles: &[Profile]) {
        self.profile_idx = idx;
        if !self.dir_edited {
            if let Some(d) = profiles.get(idx).and_then(|p| p.default_dir.clone()) {
                self.dir = d;
            }
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
        DispatchCtx { selected_status: selected, any_working: false, just_detached: false }
    }

    #[test]
    fn control_navigation() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('j')), &c), Action::MoveDown));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Down), &c), Action::MoveDown));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('k')), &c), Action::MoveUp));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Up), &c), Action::MoveUp));
    }

    #[test]
    fn control_attach_and_new() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Enter), &c), Action::Attach));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('n')), &c), Action::OpenNewSession));
    }

    #[test]
    fn enter_with_no_sessions_is_noop() {
        let c = ctx(None);
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Enter), &c), Action::None));
    }

    #[test]
    fn x_confirms_kill_when_running_removes_when_exited() {
        let running = ctx(Some(Status::Working));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('x')), &running), Action::EnterConfirmKill));
        let exited = ctx(Some(Status::Exited(Some(0))));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('x')), &exited), Action::RemoveSelected));
    }

    #[test]
    fn r_respawns_only_exited() {
        let exited = ctx(Some(Status::Exited(Some(1))));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('r')), &exited), Action::RespawnSelected));
        let running = ctx(Some(Status::Working));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('r')), &running), Action::None));
    }

    #[test]
    fn q_quits_directly_unless_something_is_working() {
        let quiet = ctx(Some(Status::Idle));
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('q')), &quiet), Action::Quit));
        let mut busy = ctx(Some(Status::Idle));
        busy.any_working = true;
        assert!(matches!(dispatch(&Mode::Control, &key(KeyCode::Char('q')), &busy), Action::EnterConfirmQuit));
    }

    #[test]
    fn attached_ctrl_q_detaches_everything_else_forwards() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(dispatch(&Mode::Attached, &ctrl_q(), &c), Action::Detach));
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
        assert!(matches!(dispatch(&Mode::Control, &ctrl_q(), &c), Action::SendLiteralDetachKey));
        // without the flag, Ctrl+Q in Control does nothing
        c.just_detached = false;
        assert!(matches!(dispatch(&Mode::Control, &ctrl_q(), &c), Action::None));
    }

    #[test]
    fn confirm_modes() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('y')), &c), Action::KillSelected));
        assert!(matches!(dispatch(&Mode::ConfirmKill, &key(KeyCode::Enter), &c), Action::KillSelected));
        assert!(matches!(dispatch(&Mode::ConfirmKill, &key(KeyCode::Esc), &c), Action::CancelToControl));
        assert!(matches!(dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('n')), &c), Action::CancelToControl));
        assert!(matches!(dispatch(&Mode::ConfirmQuit, &key(KeyCode::Char('y')), &c), Action::Quit));
        assert!(matches!(dispatch(&Mode::ConfirmQuit, &key(KeyCode::Esc), &c), Action::CancelToControl));
    }

    #[test]
    fn new_session_mode_routes_to_dialog() {
        let mode = Mode::NewSession(DialogState::new(&crate::config::Config::default_profiles()));
        let c = ctx(None);
        assert!(matches!(dispatch(&mode, &key(KeyCode::Char('a')), &c), Action::DialogKey));
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
            Profile { name: "A".into(), command: "a".into(), args: vec![], default_dir: Some("C:\\one".into()) },
            Profile { name: "B".into(), command: "b".into(), args: vec![], default_dir: Some("C:\\two".into()) },
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
        assert!(matches!(d.handle_key(&key(KeyCode::Enter), &ps), DialogResult::Submit));
        assert!(matches!(d.handle_key(&key(KeyCode::Esc), &ps), DialogResult::Cancel));
        assert!(matches!(d.handle_key(&key(KeyCode::Char('q')), &ps), DialogResult::Consumed));
    }
}
