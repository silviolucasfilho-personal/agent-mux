//! The Skills sidebar: navigation, the harness picker, and the one-session
//! rule per skill.

use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shell_profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        command: "sh".into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

fn app() -> App {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![shell_profile("test")], None, tx);
    app.set_pane_size(24, 80);
    app
}

fn push_skill_session(app: &mut App, skill_id: &str) -> usize {
    let idx = app
        .launch(shell_profile("skill-shell"), std::env::temp_dir())
        .expect("spawn shell");
    app.sessions[idx].skill_id = Some(skill_id.to_string());
    idx
}

fn screen(app: &App) -> String {
    let backend = TestBackend::new(112, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[tokio::test]
async fn heimdall_is_listed_and_the_picker_defaults_to_its_harness() {
    let mut app = app();
    let heimdall = app
        .skills
        .iter()
        .position(|s| s.id == "heimdall")
        .expect("compiled-in heimdall");
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Skills);
    app.selected_skill = heimdall;

    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::SkillLauncher(ref state) = app.mode else {
        panic!("Enter on a skill opens the harness picker");
    };
    assert_eq!(state.skill_id, "heimdall");
    assert_eq!(state.selected_harness(), Harness::Antigravity);
    assert_eq!(state.harnesses, Harness::ALL.to_vec());

    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    if let Mode::SkillLauncher(ref state) = app.mode {
        assert_eq!(state.selected_harness(), Harness::Claude);
    }
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));

    let text = screen(&app);
    assert!(text.contains("Skills ["), "sidebar section is named Skills");
    assert!(text.contains("Heimdall"));
    app.kill_all();
}

#[tokio::test]
async fn a_running_skill_is_attached_to_instead_of_relaunched() {
    let mut app = app();
    let heimdall = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    let session_idx = push_skill_session(&mut app, "heimdall");
    app.sessions[session_idx].profile.command = "codex".to_string();
    assert_eq!(app.running_skill_session("heimdall"), Some(session_idx));
    assert_eq!(app.running_skill_harness("heimdall"), Some(Harness::Codex));

    app.mode = Mode::Control;
    app.sidebar_section = SidebarSection::Skills;
    app.selected_skill = heimdall;
    let text = screen(&app);
    assert!(
        text.contains("[codex]"),
        "sidebar shows the running harness"
    );

    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Attached));
    assert_eq!(app.selected, session_idx);

    // Any harness asked for while it runs attaches; no second process.
    let before = app.sessions.len();
    for h in Harness::ALL {
        assert_eq!(app.launch_skill("heimdall", h).unwrap(), session_idx);
        assert_eq!(app.sessions.len(), before);
    }

    // Once it exits, the picker is available again.
    app.sessions[session_idx].mark_exited();
    assert_eq!(app.running_skill_session("heimdall"), None);
    app.mode = Mode::Control;
    app.sidebar_section = SidebarSection::Skills;
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::SkillLauncher(_)));
    app.kill_all();
}

#[tokio::test]
async fn skill_sessions_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sessions.json");
    let (tx, _rx) = mpsc::channel(32);
    let mut app1 = App::new(vec![shell_profile("test")], None, tx.clone());
    app1.set_sessions_file(&file);
    push_skill_session(&mut app1, "heimdall");
    app1.save_active_sessions().unwrap();
    app1.kill_all();

    let mut app2 = App::new(vec![shell_profile("test")], None, tx);
    app2.set_sessions_file(&file);
    app2.restore_saved_sessions();
    assert_eq!(app2.sessions.len(), 1);
    assert_eq!(app2.sessions[0].skill_id.as_deref(), Some("heimdall"));
    assert_eq!(app2.running_skill_session("heimdall"), Some(0));
    app2.kill_all();
}
