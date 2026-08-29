use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::session::Session;
use agent_mux::status::Status;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn shell_profile(args: &[&str]) -> Profile {
    #[cfg(windows)]
    let (command, base): (&str, Vec<String>) = ("cmd.exe", vec![]);
    #[cfg(not(windows))]
    let (command, base): (&str, Vec<String>) = ("sh", vec![]);
    let mut all = base;
    all.extend(args.iter().map(|s| s.to_string()));
    Profile { name: "test".into(), command: command.into(), args: all, default_dir: None }
}

fn echo_args(msg: &str) -> Vec<String> {
    #[cfg(windows)]
    return vec!["/c".into(), format!("echo {msg}")];
    #[cfg(not(windows))]
    return vec!["-c".into(), format!("echo {msg}")];
}

/// Pump events for `session` until `pred(&session)` or the deadline.
/// Returns true if pred was satisfied.
async fn pump_until(
    rx: &mut mpsc::Receiver<AppEvent>,
    session: &mut Session,
    timeout: Duration,
    mut pred: impl FnMut(&Session) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(session) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) if id == session.id => {
                session.process_output(&bytes, Instant::now(), false);
            }
            Ok(Some(AppEvent::PtyExit { id })) if id == session.id => {
                session.mark_exited();
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(session)
}

fn screen_text(session: &Session) -> String {
    session.parser.screen().contents()
}

#[tokio::test]
async fn spawn_echo_renders_output_and_exits_zero() {
    let (tx, mut rx) = mpsc::channel(256);
    let profile = shell_profile(&[]);
    let profile = Profile { args: echo_args("hello-agent-mux"), ..profile };
    let mut session =
        Session::spawn(1, profile, std::env::temp_dir(), 24, 80, tx).unwrap();
    let ok = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        screen_text(s).contains("hello-agent-mux")
            && matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok, "screen was: {:?}", screen_text(&session));
    assert_eq!(session.status(Instant::now()), Status::Exited(Some(0)));
}

#[tokio::test]
async fn spawn_in_missing_directory_fails_without_creating_session() {
    let (tx, _rx) = mpsc::channel(16);
    let result = Session::spawn(
        1,
        shell_profile(&[]),
        PathBuf::from("Z:/definitely/not/a/dir"),
        24,
        80,
        tx,
    );
    assert!(result.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn spawn_with_nonexistent_command_fails_without_creating_session() {
    let (tx, _rx) = mpsc::channel(16);
    let profile = Profile {
        name: "test".into(),
        command: "definitely-not-a-real-command-xyz123".into(),
        args: vec![],
        default_dir: None,
    };
    let result = Session::spawn(1, profile, std::env::temp_dir(), 24, 80, tx);
    assert!(result.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn interactive_input_roundtrip() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut session =
        Session::spawn(1, shell_profile(&[]), std::env::temp_dir(), 24, 80, tx).unwrap();
    // wait for the cmd prompt
    let ok = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        screen_text(s).contains('>')
    })
    .await;
    assert!(ok, "no prompt; screen: {:?}", screen_text(&session));
    session.write_bytes(b"echo marker-12345\r").unwrap();
    let ok = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        // appears twice: the echoed command line and its output
        screen_text(s).matches("marker-12345").count() >= 2
    })
    .await;
    assert!(ok, "screen: {:?}", screen_text(&session));
    session.write_bytes(b"exit\r").unwrap();
    let ok = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok);
}

#[tokio::test]
async fn resize_updates_parser_screen_size() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut session =
        Session::spawn(1, shell_profile(&[]), std::env::temp_dir(), 24, 80, tx).unwrap();
    session.resize(30, 100);
    assert_eq!(session.parser.screen().size(), (30, 100));
    session.kill();
    let _ = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
}

#[tokio::test]
async fn kill_terminates_long_running_child() {
    let (tx, mut rx) = mpsc::channel(256);
    #[cfg(windows)]
    let args = vec!["/c".to_string(), "ping -n 60 127.0.0.1 > NUL".to_string()];
    #[cfg(not(windows))]
    let args = vec!["-c".to_string(), "sleep 60".to_string()];
    let profile = Profile { args, ..shell_profile(&[]) };
    let mut session =
        Session::spawn(1, profile, std::env::temp_dir(), 24, 80, tx).unwrap();
    session.kill();
    let ok = pump_until(&mut rx, &mut session, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok, "killed child never produced PtyExit");
}
