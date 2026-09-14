use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::history::SessionSummary;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_echo_profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        command: "sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

#[tokio::test]
async fn test_persistence_roundtrip_across_restarts() {
    let temp_dir = tempfile::tempdir().unwrap();
    let sessions_path = temp_dir.path().join("saved_sessions.json");

    let (tx, _rx) = mpsc::channel(32);
    let mut app1 = App::new(vec![make_echo_profile("test-agent")], None, tx.clone());
    app1.set_pane_size(24, 80);
    app1.set_sessions_file(&sessions_path);

    // Dialog submit simulation or resume
    let dialog_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app1.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app1.profiles));
    app1.handle_key(&dialog_key, Instant::now());

    assert_eq!(app1.sessions.len(), 1);
    assert_eq!(app1.sessions[0].profile.name, "test-agent");

    // Save active sessions (as done on exit)
    app1.save_active_sessions().unwrap();
    app1.kill_all();

    // Now simulate restarting agent-mux
    let mut app2 = App::new(vec![make_echo_profile("test-agent")], None, tx);
    app2.set_pane_size(24, 80);
    app2.set_sessions_file(&sessions_path);
    assert_eq!(app2.sessions.len(), 0);

    // Call restore_saved_sessions (as done in main on startup)
    app2.restore_saved_sessions();

    assert_eq!(app2.sessions.len(), 1);
    assert_eq!(app2.sessions[0].profile.name, "test-agent");
    assert_eq!(app2.selected, 0);
    assert_eq!(app2.sidebar_section, SidebarSection::Active);

    app2.kill_all();
}

#[tokio::test]
async fn test_sidebar_split_navigation() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_echo_profile("echo1")], None, tx);
    app.set_pane_size(24, 80);

    // Mock 1 active session
    let dialog_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app.profiles));
    app.handle_key(&dialog_key, Instant::now());

    // Mock history sessions
    app.history_sessions = vec![
        SessionSummary {
            session_id: "hist-1".into(),
            title: "First history task".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: PathBuf::from("/tmp/hist1.jsonl"),
            turn_count: 5,
            project_slug: "-test".into(),
            timestamp_str: "2026-09-01 12:00".into(),
            provider: agent_mux::history::AgentProvider::Claude,
            cwd: Some(PathBuf::from("/tmp")),
        },
        SessionSummary {
            session_id: "hist-2".into(),
            title: "Second history task".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: PathBuf::from("/tmp/hist2.jsonl"),
            turn_count: 8,
            project_slug: "-test".into(),
            timestamp_str: "2026-09-02 14:00".into(),
            provider: agent_mux::history::AgentProvider::Antigravity,
            cwd: Some(PathBuf::from("/tmp")),
        },
    ];

    // Starts in Active section
    assert_eq!(app.sidebar_section, SidebarSection::Active);

    // Tab cycles: Active -> Agents -> History -> Active
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::History);

    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Active);

    // Down at the end of active sessions transitions into Agents
    app.handle_key(&key(KeyCode::Down), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    // Down in Agents transitions into History
    app.handle_key(&key(KeyCode::Down), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::History);
    assert_eq!(app.selected_history, 0);

    // Down in History navigates history items
    app.handle_key(&key(KeyCode::Down), Instant::now());
    assert_eq!(app.selected_history, 1);

    // Up in History navigates back up
    app.handle_key(&key(KeyCode::Up), Instant::now());
    assert_eq!(app.selected_history, 0);

    // Up at top of History transitions to Agents
    app.handle_key(&key(KeyCode::Up), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    // Up in Agents transitions back to Active
    app.handle_key(&key(KeyCode::Up), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Active);

    app.kill_all();
}

#[tokio::test]
async fn test_restart_history_session_from_sidebar() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_echo_profile("claude")], None, tx);
    app.set_pane_size(24, 80);

    app.history_sessions = vec![SessionSummary {
        session_id: "uuid-abc".into(),
        title: "Fix compiler bug".into(),
        modified: std::time::SystemTime::UNIX_EPOCH,
        file_path: PathBuf::from("/tmp/conv.jsonl"),
        turn_count: 10,
        project_slug: "-test".into(),
        timestamp_str: "2026-09-01".into(),
        provider: agent_mux::history::AgentProvider::Claude,
        cwd: Some(std::env::current_dir().unwrap()),
    }];

    app.sidebar_section = SidebarSection::History;
    app.selected_history = 0;

    // Pressing 'r' or 'Enter' in History restarts/resumes the session
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    assert_eq!(app.sessions.len(), 1);
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    assert_eq!(app.selected, 0);
    // Profile args for claude resume should contain --resume
    assert!(
        app.sessions[0]
            .profile
            .args
            .contains(&"--resume".to_string())
    );
    assert!(
        app.sessions[0]
            .profile
            .args
            .contains(&"uuid-abc".to_string())
    );

    app.kill_all();
}

#[tokio::test]
async fn test_sidebar_mouse_click_selection() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_echo_profile("echo1")], None, tx);
    app.set_pane_size(24, 80);

    // Spawn 1 active session
    let dialog_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app.profiles));
    app.handle_key(&dialog_key, Instant::now());

    app.history_sessions = vec![
        SessionSummary {
            session_id: "hist-1".into(),
            title: "Task 1".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: PathBuf::from("/tmp/1.jsonl"),
            turn_count: 1,
            project_slug: "-test".into(),
            timestamp_str: "2026".into(),
            provider: agent_mux::history::AgentProvider::Claude,
            cwd: None,
        },
        SessionSummary {
            session_id: "hist-2".into(),
            title: "Task 2".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: PathBuf::from("/tmp/2.jsonl"),
            turn_count: 2,
            project_slug: "-test".into(),
            timestamp_str: "2026".into(),
            provider: agent_mux::history::AgentProvider::Antigravity,
            cwd: None,
        },
    ];

    let (active_rect, agents_rect, history_rect) =
        agent_mux::ui::sidebar_areas(app.pane_size.0 + 3, app.agents.len());

    // Click in history area
    let click_hist = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: history_rect.y + 2, // second item
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click_hist, Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::History);
    assert_eq!(app.selected_history, 1);

    // Click in agents area
    let click_agents = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: agents_rect.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click_agents, Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    // Click in active area
    let click_active = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: active_rect.y + 1, // first item
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click_active, Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    assert_eq!(app.selected, 0);

    app.kill_all();
}

#[tokio::test]
async fn test_toggle_sidebar_and_fullscreen_harness() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_echo_profile("echo1")], None, tx);
    app.set_pane_size(24, 80);

    assert!(!app.sidebar_hidden);
    assert_eq!(app.pane_size, (24, 80));

    // Press 'b' in Control mode to hide the sidebar
    app.handle_key(&key(KeyCode::Char('b')), Instant::now());
    assert!(app.sidebar_hidden);
    // When sidebar is hidden, pane columns expand by SIDEBAR_WIDTH (30)
    assert_eq!(app.pane_size.1, 80 + agent_mux::ui::SIDEBAR_WIDTH);

    // Press 'b' again to show the sidebar
    app.handle_key(&key(KeyCode::Char('b')), Instant::now());
    assert!(!app.sidebar_hidden);
    assert_eq!(app.pane_size.1, 80);

    // Now test while in Attached mode with Ctrl+Shift+B chord
    let dialog_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app.profiles));
    app.handle_key(&dialog_key, Instant::now());
    app.mode = Mode::Attached;

    let toggle_chord = KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(&toggle_chord, Instant::now());
    assert!(app.sidebar_hidden);
    assert_eq!(app.pane_size.1, 80 + agent_mux::ui::SIDEBAR_WIDTH);

    // Toggle back in Attached mode
    app.handle_key(&toggle_chord, Instant::now());
    assert!(!app.sidebar_hidden);
    assert_eq!(app.pane_size.1, 80);

    app.kill_all();
}

#[tokio::test]
async fn test_sidebar_hidden_navigation_and_mouse_click() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(
        vec![make_echo_profile("agent1"), make_echo_profile("agent2")],
        None,
        tx,
    );
    app.set_pane_size(24, 80);

    // Spawn 2 sessions
    app.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app.profiles));
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    app.mode = Mode::NewSession(agent_mux::app::DialogState::new(&app.profiles));
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    assert_eq!(app.sessions.len(), 2);

    // Hide sidebar
    app.toggle_sidebar();
    assert!(app.sidebar_hidden);

    // In hidden sidebar, Tab cycles between active sessions
    app.selected = 0;
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.selected, 1);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.selected, 0);

    // Down and Up navigate active sessions
    app.handle_key(&key(KeyCode::Down), Instant::now());
    assert_eq!(app.selected, 1);
    app.handle_key(&key(KeyCode::Up), Instant::now());
    assert_eq!(app.selected, 0);

    // 1-9 jumps to session
    app.handle_key(&key(KeyCode::Char('2')), Instant::now());
    assert_eq!(app.selected, 1);
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    assert_eq!(app.selected, 0);

    // Click on status bar toggle (row >= terminal_size.0 - 1, col <= 18)
    let click_status = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: app.terminal_size.0.saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click_status, Instant::now());
    assert!(!app.sidebar_hidden);

    app.kill_all();
}
