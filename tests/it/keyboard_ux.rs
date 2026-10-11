use agent_mux::app::{App, Focus, Mode};
use agent_mux::keymap::ActionId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _) = mpsc::channel(64);
    App::new(vec![], None, tx)
}
fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }

#[test]
fn command_availability_tracks_current_context() {
    let mut a = app();
    assert!(!a.command_availability(ActionId::ReturnToSession).enabled());
    assert!(!a.command_availability(ActionId::Save).enabled());
    a.sidebar_hidden = true;
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(!a.sidebar_hidden);
    assert_eq!(a.focus, Focus::List);
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(!a.sidebar_hidden);
    assert!(matches!(a.mode, Mode::Main));
}

#[test]
fn commands_open_on_every_screen_and_preserve_search() {
    let mut a = app();
    a.search = Some(agent_mux::search::SearchState::new());
    a.search.as_mut().unwrap().query = "preserve this".into();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert!(a.command_overlay.is_some());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(a.command_overlay.is_none());
    assert_eq!(a.search.as_ref().unwrap().query, "preserve this");
    a.search = None;
    a.execute_command(ActionId::Runs, Instant::now());
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    assert!(a.command_overlay.is_some());
    assert!(matches!(a.mode, Mode::RunsView(_)));
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(a.mode, Mode::RunsView(_)));
}

#[test]
fn disabled_command_cannot_execute() {
    let mut a = app();
    a.execute_command(ActionId::ReturnToSession, Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert!(a.notice.as_ref().unwrap().text.contains("No session"));
}

#[test]
fn dirty_navigation_cancel_preserves_editor() {
    let mut a = app();
    a.new_persona_editor("blank", "keyboard-test");
    assert!(matches!(a.mode, Mode::AgentEditor(_)));
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(matches!(a.mode, Mode::AgentEditor(_)), "dirty editor must not be discarded");
    assert!(a.command_overlay.is_some());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(a.mode, Mode::AgentEditor(_)));
    assert!(a.command_overlay.is_none());
    a.execute_command(ActionId::FocusAgents, Instant::now());
    a.handle_key(&key(KeyCode::Char('y')), Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert_eq!(a.focus, Focus::List);
}

#[test]
fn hidden_sidebar_transfers_focus() {
    let mut a = app();
    a.focus = Focus::List;
    a.execute_command(ActionId::ToggleSidebar, Instant::now());
    assert!(a.sidebar_hidden);
    assert_eq!(a.focus, Focus::Pane);
}
