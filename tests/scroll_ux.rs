use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::session::Session;
use agent_mux::status::Status;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub fn shell_profile() -> Profile {
    #[cfg(windows)]
    let command = "cmd.exe";
    #[cfg(not(windows))]
    let command = "sh";
    Profile {
        name: "shell".into(),
        command: command.into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
    }
}

/// Command that prints numbered lines 1..=n, then exits.
pub fn print_lines_args(n: u32) -> Vec<String> {
    #[cfg(windows)]
    return vec!["/c".into(), format!("for /L %i in (1,1,{n}) do @echo line-%i")];
    #[cfg(not(windows))]
    return vec!["-c".into(), format!("i=1; while [ $i -le {n} ]; do echo line-$i; i=$((i+1)); done")];
}

pub async fn pump_session(
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
            Ok(Some(AppEvent::PtyExit { id })) if id == session.id => session.mark_exited(),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(session)
}

#[tokio::test]
async fn scrolling_reveals_history_and_snaps_back() {
    let (tx, mut rx) = mpsc::channel(256);
    let profile = Profile { args: print_lines_args(100), ..shell_profile() };
    let mut s = Session::spawn(1, profile, std::env::temp_dir(), 10, 80, tx).unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
            && s.parser.screen().contents().contains("line-100")
    })
    .await;
    assert!(ok, "screen: {:?}", s.parser.screen().contents());
    assert_eq!(s.scrolled(), 0);
    // line-1 scrolled out of the 10-row screen; scroll to top reveals it
    // (exact-line match: "line-1" is a substring of the visible "line-100")
    assert!(!s.parser.screen().contents().lines().any(|l| l.trim() == "line-1"));
    s.scroll_to_top();
    assert!(s.scrolled() > 0);
    assert!(s.parser.screen().contents().lines().any(|l| l.trim() == "line-1"));
    // scroll_by moves relative; scroll_to_bottom restores live view
    let at_top = s.scrolled();
    s.scroll_by(-3);
    assert_eq!(s.scrolled(), at_top - 3);
    s.scroll_to_bottom();
    assert_eq!(s.scrolled(), 0);
    assert!(s.parser.screen().contents().contains("line-100"));
    let (len, offset) = s.scroll_view();
    assert_eq!(offset, 0);
    assert!(len >= 90, "cached scrollback_len should be large, got {len}");
}

#[tokio::test]
async fn view_stays_anchored_while_new_output_arrives() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut s = Session::spawn(1, shell_profile(), std::env::temp_dir(), 10, 80, tx).unwrap();
    // interactive shell: print 50 lines, scroll up, then print 20 more
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        !s.parser.screen().contents().trim().is_empty()
    })
    .await;
    assert!(ok);
    #[cfg(windows)]
    s.write_bytes(b"for /L %i in (1,1,50) do @echo first-%i\r").unwrap();
    #[cfg(not(windows))]
    s.write_bytes(b"i=1; while [ $i -le 50 ]; do echo first-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        s.parser.screen().contents().contains("first-50")
    })
    .await;
    assert!(ok);
    s.scroll_by(20);
    let top_line_before = s.parser.screen().contents().lines().next().unwrap_or_default().to_string();
    let offset_before = s.scrolled();
    #[cfg(windows)]
    s.write_bytes(b"for /L %i in (1,1,20) do @echo second-%i\r").unwrap();
    #[cfg(not(windows))]
    s.write_bytes(b"i=1; while [ $i -le 20 ]; do echo second-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        s.scrolled() > offset_before // anchoring grew the offset
    })
    .await;
    assert!(ok, "offset never grew: still {}", s.scrolled());
    let top_line_after = s.parser.screen().contents().lines().next().unwrap_or_default().to_string();
    assert_eq!(top_line_before, top_line_after, "view moved while scrolled");
    s.kill();
    let _ = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
}
