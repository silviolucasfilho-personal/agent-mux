use agent_mux::app::{App, Mode};
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shell_profiles() -> Vec<Profile> {
    #[cfg(windows)]
    let command = "cmd.exe";
    #[cfg(not(windows))]
    let command = "sh";
    vec![Profile {
        name: "shell".into(),
        command: command.into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
    }]
}

/// Route channel events into the app until pred or deadline.
async fn pump_until(
    rx: &mut mpsc::Receiver<AppEvent>,
    app: &mut App,
    timeout: Duration,
    mut pred: impl FnMut(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(app) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(app)
}

/// n -> (accept defaults) -> Enter: creates a session via the dialog.
fn create_session_via_dialog(app: &mut App) {
    let now = Instant::now();
    app.handle_key(&key(KeyCode::Char('n')), now);
    assert!(matches!(app.mode, Mode::NewSession(_)));
    app.handle_key(&key(KeyCode::Enter), now);
}

#[tokio::test]
async fn dialog_submit_spawns_session_and_returns_to_control() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(), tx);
    create_session_via_dialog(&mut app);
    assert!(matches!(app.mode, Mode::Control));
    assert_eq!(app.sessions.len(), 1);
    assert_eq!(app.selected, 0);
    // it's alive: output arrives
    let ok = pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
        !a.sessions[0].parser.screen().contents().trim().is_empty()
    })
    .await;
    assert!(ok);
    app.kill_all();
}

#[tokio::test]
async fn dialog_submit_with_bad_directory_stays_open_with_error() {
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(), tx);
    let now = Instant::now();
    app.handle_key(&key(KeyCode::Char('n')), now);
    // wipe the prefilled dir and type a bad one
    if let Mode::NewSession(d) = &mut app.mode {
        d.dir = "Z:/no/such/dir".into();
        d.dir_edited = true;
    } else {
        panic!("not in dialog");
    }
    app.handle_key(&key(KeyCode::Enter), now);
    match &app.mode {
        Mode::NewSession(d) => assert!(d.error.is_some()),
        m => panic!("expected dialog to stay open, mode is {m:?}"),
    }
    assert!(app.sessions.is_empty());
}

#[tokio::test]
async fn attach_detach_and_literal_ctrl_q() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(), tx);
    create_session_via_dialog(&mut app);
    let now = Instant::now();
    app.handle_key(&key(KeyCode::Enter), now); // attach
    assert!(matches!(app.mode, Mode::Attached));
    assert_eq!(app.attached(), Some(0));
    let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    app.handle_key(&ctrl_q, now); // detach
    assert!(matches!(app.mode, Mode::Control));
    app.handle_key(&ctrl_q, now); // literal Ctrl+Q -> re-attached
    assert!(matches!(app.mode, Mode::Attached));
    app.kill_all();
    let _ = pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
}

#[tokio::test]
async fn kill_confirm_respawn_and_remove() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(), tx);
    create_session_via_dialog(&mut app);
    let now = Instant::now();
    // x on a running session -> confirm -> y kills it
    app.handle_key(&key(KeyCode::Char('x')), now);
    assert!(matches!(app.mode, Mode::ConfirmKill));
    app.handle_key(&key(KeyCode::Char('y')), now);
    let ok = pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok, "kill never produced Exited");
    // r respawns with same profile/dir, screen resets, session is live again
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(!matches!(
        app.sessions[0].status(Instant::now()),
        Status::Exited(_)
    ));
    // kill again, then x removes the exited session
    app.sessions[0].kill();
    let ok = pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok);
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    assert!(app.sessions.is_empty());
}

#[tokio::test]
async fn quit_asks_for_confirmation_while_working() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(), tx);
    create_session_via_dialog(&mut app);
    // make the session Working: pump until some output arrived just now
    let ok = pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Working)
    })
    .await;
    assert!(ok);
    let now = Instant::now();
    app.handle_key(&key(KeyCode::Char('q')), now);
    assert!(matches!(app.mode, Mode::ConfirmQuit));
    assert!(!app.should_quit);
    app.handle_key(&key(KeyCode::Char('y')), now);
    assert!(app.should_quit);
    app.kill_all();
}
