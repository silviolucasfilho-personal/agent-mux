use agent_mux::app::{App, HistoryPane, Mode};
use agent_mux::config::Profile;
use agent_mux::history::{discover_sessions, load_session_log, render_log_lines};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn create_mock_claude_project_dir() -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().unwrap();
    let proj_dir = temp_dir.path().join("projects").join("-workspace-my-app");
    std::fs::create_dir_all(&proj_dir).unwrap();

    let session_1 = proj_dir.join("session-abc-123.jsonl");
    let content_1 = r#"{"type":"ai-title","aiTitle":"Add database migrations","sessionId":"session-abc-123"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Create migration table"}]},"timestamp":"2026-08-30T15:00:00.000Z"}
{"type":"assistant","message":{"role":"assistant","model":"claude-3-7-sonnet","content":[{"type":"text","text":"I will generate the SQL file."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"touch migration.sql"}}]},"timestamp":"2026-08-30T15:01:00.000Z"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"done","is_error":false}]},"timestamp":"2026-08-30T15:01:05.000Z"}
"#;
    std::fs::write(&session_1, content_1).unwrap();

    let session_2 = proj_dir.join("session-xyz-789.jsonl");
    let content_2 = r#"{"type":"ai-title","aiTitle":"Fix auth crash","sessionId":"session-xyz-789"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"The login endpoint panics"}]},"timestamp":"2026-08-30T16:00:00.000Z"}
"#;
    std::fs::write(&session_2, content_2).unwrap();

    temp_dir
}

#[test]
fn test_discovery_and_log_parsing() {
    let temp = create_mock_claude_project_dir();
    let empty_brain = temp.path().join("empty_brain");
    let sessions = discover_sessions(
        Some(temp.path()),
        Some(&empty_brain),
        Some(Path::new("/workspace/my-app")),
        false,
    );
    assert_eq!(sessions.len(), 2);

    let s1 = sessions
        .iter()
        .find(|s| s.session_id == "session-abc-123")
        .unwrap();
    assert_eq!(s1.title, "Add database migrations");

    let entries = load_session_log(&s1.file_path).unwrap();
    assert_eq!(entries.len(), 4);

    let rendered = render_log_lines(&entries);
    let full_text = rendered
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full_text.contains("USER"));
    assert!(full_text.contains("Create migration table"));
    assert!(full_text.contains("CLAUDE"));
    assert!(full_text.contains("I will generate the SQL file."));
    assert!(full_text.contains("TOOL: Bash"));
    assert!(full_text.contains("touch migration.sql"));
}

#[tokio::test]
async fn test_app_history_flow_and_navigation() {
    let temp = create_mock_claude_project_dir();
    let empty_brain = temp.path().join("empty_brain");
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(
        vec![Profile {
            name: "Claude".into(),
            command: "echo".into(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    );

    // Initial state: Control mode
    assert!(matches!(app.mode, Mode::Control));

    // Press 'l' -> opens SessionHistory
    app.handle_key(&key(KeyCode::Char('l')), Instant::now());
    assert!(matches!(app.mode, Mode::SessionHistory(_)));

    // Inject mock discovered sessions
    if let Mode::SessionHistory(ref mut hist) = app.mode {
        hist.sessions = discover_sessions(
            Some(temp.path()),
            Some(&empty_brain),
            Some(Path::new("/workspace/my-app")),
            false,
        );
        hist.selected_session_idx = 0;
        hist.load_selected_log();
        assert_eq!(hist.sessions.len(), 2);
        assert!(!hist.log_lines.is_empty());
        assert_eq!(hist.focused_pane, HistoryPane::SessionsList);
    }

    // Press 'j' in SessionList -> moves to session 2 and reloads log
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    if let Mode::SessionHistory(ref hist) = app.mode {
        assert_eq!(hist.selected_session_idx, 1);
        let log_text = hist
            .log_lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            log_text.contains("The login endpoint panics")
                || log_text.contains("Create migration table")
        );
    }

    // Press 'Tab' -> switches focus to LogDetail
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    if let Mode::SessionHistory(ref hist) = app.mode {
        assert_eq!(hist.focused_pane, HistoryPane::LogDetail);
    }

    // Press 'j' in LogDetail -> scrolls log (shrink the viewport below the
    // content first: scrolling clamps to the last full page)
    if let Mode::SessionHistory(ref mut hist) = app.mode {
        hist.viewport_rows.set(1);
        hist.scroll_offset = 0;
    }
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    if let Mode::SessionHistory(ref hist) = app.mode {
        assert_eq!(hist.scroll_offset, 1);
    }

    // Press 'Esc' -> returns to Control mode
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
}

#[tokio::test]
async fn test_app_history_resume_spawns_session() {
    let temp = create_mock_claude_project_dir();
    let empty_brain = temp.path().join("empty_brain");
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(
        vec![Profile {
            name: "Claude Code".into(),
            command: "echo".into(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    );

    app.handle_key(&key(KeyCode::Char('l')), Instant::now());
    if let Mode::SessionHistory(ref mut hist) = app.mode {
        hist.sessions = discover_sessions(
            Some(temp.path()),
            Some(&empty_brain),
            Some(Path::new("/workspace/my-app")),
            false,
        );
        hist.selected_session_idx = 0;
        hist.load_selected_log();
    }

    // Press 'r' to resume selected session
    let selected_id = if let Mode::SessionHistory(ref hist) = app.mode {
        hist.sessions[hist.selected_session_idx].session_id.clone()
    } else {
        panic!("expected SessionHistory mode");
    };

    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
    assert_eq!(app.sessions.len(), 1);
    let spawned = &app.sessions[0];
    assert_eq!(spawned.profile.args, vec!["--resume", &selected_id]);
}

#[tokio::test]
async fn test_antigravity_discovery_and_resume() {
    let temp = tempfile::tempdir().unwrap();
    let empty_claude = temp.path().join("empty_claude");
    let brain_dir = temp.path().join("brain").join("conv-12345");
    let logs_dir = brain_dir.join(".system_generated").join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let transcript = logs_dir.join("transcript.jsonl");
    let content = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-30T17:00:00Z","content":"<USER_REQUEST>\nBuild the parser\n</USER_REQUEST>"}
{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-30T17:01:00Z","thinking":"Writing code","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"cargo test\"","Cwd":"\"/workspace/my-app\""}}]}
"#;
    std::fs::write(&transcript, content).unwrap();

    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(
        vec![Profile {
            name: "Antigravity".into(),
            command: "agy".into(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    );

    let summaries = discover_sessions(
        Some(&empty_claude),
        Some(&temp.path().join("brain")),
        Some(Path::new("/workspace/my-app")),
        false,
    );
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].title, "Build the parser");
    assert_eq!(
        summaries[0].provider,
        agent_mux::history::AgentProvider::Antigravity
    );

    let entries = load_session_log(&summaries[0].file_path).unwrap();
    assert_eq!(entries.len(), 3);
    let rendered = render_log_lines(&entries);
    let text = rendered
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("ANTIGRAVITY"));
    assert!(text.contains("Build the parser"));
    assert!(text.contains("cargo test"));

    app.handle_key(&key(KeyCode::Char('l')), Instant::now());
    if let Mode::SessionHistory(ref mut hist) = app.mode {
        hist.sessions = summaries;
        hist.selected_session_idx = 0;
        hist.load_selected_log();
    }

    // Press 'r' to launch Antigravity session
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
    assert_eq!(app.sessions.len(), 1);
    assert_eq!(app.sessions[0].profile.command, "agy");
}
