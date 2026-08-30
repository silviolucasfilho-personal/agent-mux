use agent_mux::app::{App, Mode};
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::session::Session;
use agent_mux::status::Status;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

pub async fn pump_app(
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn wheel(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
    // (32, 2): inside the pane for any pane at least 2x2
    MouseEvent { kind, column: 32, row: 2, modifiers }
}

/// App with one spawned shell session that has printed `lines` lines and
/// exited, sized 10 rows x 80 cols.
async fn app_with_history(
    lines: u32,
) -> (App, mpsc::Receiver<AppEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(vec![shell_profile()], tx.clone());
    app.set_pane_size(10, 80);
    let profile = Profile { args: print_lines_args(lines), ..shell_profile() };
    let session = Session::spawn(900, profile, std::env::temp_dir(), 10, 80, tx).unwrap();
    app.sessions.push(session);
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        a.sessions[0]
            .parser
            .screen()
            .contents()
            .contains(&format!("line-{lines}"))
            && matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok, "history session never finished");
    (app, rx)
}

#[tokio::test]
async fn wheel_scrolls_control_preview_locally() {
    let (mut app, _rx) = app_with_history(100).await;
    for _ in 0..4 {
        app.handle_mouse(wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE), Instant::now());
    }
    assert_eq!(app.sessions[0].scrolled(), 12); // 4 ticks x 3 lines
    app.handle_mouse(wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 9);
}

#[tokio::test]
async fn wheel_outside_pane_is_ignored() {
    let (mut app, _rx) = app_with_history(100).await;
    let ev = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 5, // inside the sidebar
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(ev, Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
}

#[tokio::test]
async fn shift_paging_and_home_end() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_key(&shift_key(KeyCode::PageUp), Instant::now());
    let after_page = app.sessions[0].scrolled();
    assert!(after_page > 0);
    app.handle_key(&shift_key(KeyCode::Home), Instant::now());
    let (len, offset) = app.sessions[0].scroll_view();
    assert_eq!(offset, len);
    app.handle_key(&shift_key(KeyCode::End), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
    app.handle_key(&shift_key(KeyCode::PageUp), Instant::now());
    app.handle_key(&shift_key(KeyCode::PageDown), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
}

#[tokio::test]
async fn forwarded_key_snaps_to_live_when_attached() {
    // live interactive shell, filled with enough output to have scrollback
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(vec![shell_profile()], tx.clone());
    app.set_pane_size(10, 80);
    let session = Session::spawn(901, shell_profile(), std::env::temp_dir(), 10, 80, tx).unwrap();
    app.sessions.push(session);
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        !a.sessions[0].parser.screen().contents().trim().is_empty()
    })
    .await;
    assert!(ok, "shell never painted its prompt");
    #[cfg(windows)]
    app.sessions[0].write_bytes(b"for /L %i in (1,1,50) do @echo fill-%i\r").unwrap();
    #[cfg(not(windows))]
    app.sessions[0].write_bytes(b"i=1; while [ $i -le 50 ]; do echo fill-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        a.sessions[0].parser.screen().contents().contains("fill-50")
    })
    .await;
    assert!(ok);
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // attach
    assert!(matches!(app.mode, Mode::Attached));
    app.sessions[0].scroll_by(5);
    assert!(app.sessions[0].scrolled() > 0, "test setup: expected scrollback");
    app.handle_key(&key(KeyCode::Char('a')), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0, "forwarded key must snap to live");
    app.kill_all();
}

#[tokio::test]
async fn scroll_indicator_renders_in_title() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (mut app, _rx) = app_with_history(100).await;
    app.sessions[0].scroll_by(7);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, &app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains("SCROLL"), "no scroll indicator: {text}");
    assert!(text.contains('7'), "offset missing from indicator");
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
}

#[tokio::test]
async fn drag_selects_and_extracts_screen_text() {
    let (mut app, _rx) = app_with_history(100).await;
    // pane interior starts at (31, 1); drag across two rows
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31 + 7, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 31 + 7, 2), Instant::now());
    let active = app.selection.as_ref().expect("selection should exist");
    assert!(!active.sel.is_empty());
    assert!(!active.dragging, "release must end the drag");
    let sel = active.sel;
    let (len, _) = app.sessions[0].scroll_view();
    let text = agent_mux::selection::extract_text(&mut app.sessions[0].parser, len, &sel);
    // the visible screen at rows 0..=1 holds two of the trailing lines
    // (line-93.., depending on prompt rows); assert shape, not exact rows
    assert!(text.contains('\n'), "two-row drag extracts two rows: {text:?}");
    assert!(text.contains("line-9"), "extracted from visible tail: {text:?}");
}

#[tokio::test]
async fn plain_click_clears_selection() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 40, 2), Instant::now());
    assert!(app.selection.is_some());
    // click without drag clears
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 1), Instant::now());
    assert!(app.selection.is_none());
}

#[tokio::test]
async fn selection_does_not_leak_across_sessions() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 40, 2), Instant::now());
    assert!(app.displayed_selection().is_some());
    // respawn replaces the session (fresh id) -> selection no longer displayed
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(app.displayed_selection().is_none());
    app.kill_all();
}

#[tokio::test]
async fn release_applies_final_position() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 35, 1), Instant::now());
    // no Drag ever reports this final position -- only the Up does (e.g. a
    // terminal that throttles motion events)
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 31 + 7, 2), Instant::now());
    let active = app.selection.as_ref().expect("selection should exist");
    let (len, offset) = app.sessions[0].scroll_view();
    let expected_row = agent_mux::selection::abs_row(len, offset, 1); // lrow = 2 - 1
    assert_eq!(
        active.sel.head,
        agent_mux::selection::Pos { row: expected_row, col: 7 },
        "release must apply the Up event's own position, not just the last Drag's"
    );
}

#[tokio::test]
async fn release_outside_pane_finalizes_drag() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 2), Instant::now());
    // release lands in the sidebar, well outside the pane interior
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5, 2), Instant::now());
    let active = app.selection.as_ref().expect("selection should exist");
    assert!(!active.dragging, "release must end the drag even from outside the pane");
    assert_eq!(active.sel.head.col, 0, "head clamps to the pane's left edge");
    // no stranded state: a subsequent plain click still clears normally
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 1), Instant::now());
    assert!(app.selection.is_none());
}

#[tokio::test]
#[ignore = "mutates the real system clipboard"]
async fn copy_on_select_reaches_clipboard() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31 + 7, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 31 + 7, 1), Instant::now());
    let text = arboard::Clipboard::new().unwrap().get_text().unwrap();
    assert!(text.contains("line-9"), "clipboard got: {text:?}");
}
