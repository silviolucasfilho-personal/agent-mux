//! The Skills view (`S`): grouping by harness, navigation, the harness
//! filter, install and uninstall from the view, launching on the row's
//! harness, and the one-session rule per skill.

use agent_mux::app::{App, Mode, SidebarSection, SkillRow, SkillsPane, SkillsTab};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
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

/// An app whose skill installs and harness definitions live under a
/// temporary home, and whose user packages come from an empty directory
/// (so only the compiled-in Heimdall is listed).
fn app_in(home: &Path, profiles: Vec<Profile>) -> App {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(profiles, None, tx);
    app.set_pane_size(24, 80);
    let skills_dir = home.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    app.skill_install_home = Some(home.to_path_buf());
    app.skills_dir = Some(skills_dir);
    app.reload_skills();
    app
}

fn push_skill_session(app: &mut App, skill_id: &str) -> usize {
    let idx = app
        .launch(shell_profile("skill-shell"), std::env::temp_dir())
        .expect("spawn shell");
    app.sessions[idx].skill_id = Some(skill_id.to_string());
    idx
}

/// A `claude` that just stays alive, so a launch has a child to attach to.
#[cfg(unix)]
fn fake_claude(bin_dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin_dir).unwrap();
    let script = bin_dir.join("claude");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
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

fn shape(app: &App) -> Vec<String> {
    let Mode::SkillsView(view) = &app.mode else {
        panic!("not in the skills view: {:?}", app.mode);
    };
    view.rows
        .iter()
        .map(|r| match r {
            SkillRow::Header(h) => format!("# {}", h.as_str()),
            SkillRow::Package { index, harness } => {
                format!("{}@{}", view.packages[*index].id, harness.as_str())
            }
            SkillRow::Native { index } => format!("native {}", view.native[*index].name),
        })
        .collect()
}

fn selected(app: &App) -> Option<(String, Harness)> {
    let Mode::SkillsView(view) = &app.mode else {
        return None;
    };
    view.selected_package().map(|(p, h)| (p.id.clone(), h))
}

#[tokio::test]
async fn s_opens_the_view_grouped_by_harness_and_the_sidebar_has_no_skills_section() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(
        app.sidebar_section,
        SidebarSection::History,
        "Tab goes straight to History"
    );

    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert_eq!(
        shape(&app),
        vec![
            "# claude",
            "heimdall@claude",
            "# codex",
            "heimdall@codex",
            "# agy",
            "heimdall@agy"
        ]
    );
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Claude)));

    // j/k skip the headers and clamp at both ends
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Codex)));
    app.handle_key(&key(KeyCode::Char('k')), Instant::now());
    app.handle_key(&key(KeyCode::Char('k')), Instant::now());
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Claude)));

    let text = screen(&app);
    assert!(text.contains("Skills (3) [harness: all]"), "{text}");
    assert!(
        text.contains("Claude Code") && text.contains("Codex CLI"),
        "{text}"
    );
    assert!(text.contains("not installed"), "{text}");
    assert!(text.contains("Details"), "the detail tab is titled: {text}");

    // 2 narrows to Codex, 2 again clears; Esc clears a filter before closing
    app.handle_key(&key(KeyCode::Char('2')), Instant::now());
    assert_eq!(shape(&app), vec!["# codex", "heimdall@codex"]);
    assert!(screen(&app).contains("[harness: codex]"));
    app.handle_key(&key(KeyCode::Char('2')), Instant::now());
    assert_eq!(shape(&app).len(), 6);
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(
        matches!(app.mode, Mode::SkillsView(_)),
        "Esc cleared the filter first"
    );
    assert_eq!(shape(&app).len(), 6);
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
    app.kill_all();
}

#[tokio::test]
async fn tabs_follow_the_selected_rows_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    std::fs::create_dir_all(temp.path().join("skills/plain")).unwrap();
    std::fs::write(
        temp.path().join("skills/plain/SKILL.md"),
        "---\nname: plain\ndescription: A plain skill with no telemetry.\n---\nBody.\n",
    )
    .unwrap();
    app.reload_skills();
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert_eq!(
        shape(&app)[..3],
        ["# claude", "heimdall@claude", "plain@claude"]
    );

    // Heimdall reads traces: three tabs, cycling with Tab / BackTab
    let tabs = |app: &App| match &app.mode {
        Mode::SkillsView(v) => (v.tabs(), v.tab),
        _ => panic!(),
    };
    assert_eq!(
        tabs(&app).0,
        vec![
            SkillsTab::Details,
            SkillsTab::Executions,
            SkillsTab::Briefing
        ]
    );
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Executions);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Briefing);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Details);
    app.handle_key(&key(KeyCode::BackTab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Briefing);

    // moving onto a plain skill drops the Briefing tab and lands on Details
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    assert_eq!(selected(&app), Some(("plain".into(), Harness::Claude)));
    assert_eq!(
        tabs(&app),
        (
            vec![SkillsTab::Details, SkillsTab::Executions],
            SkillsTab::Details
        )
    );

    // → focuses the detail pane, ← comes back, Esc from the detail pane
    // returns to the list before closing
    app.handle_key(&key(KeyCode::Right), Instant::now());
    let Mode::SkillsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.focus, SkillsPane::Detail);
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    let Mode::SkillsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.focus, SkillsPane::Skills);
    app.kill_all();
}

#[tokio::test]
async fn a_running_skill_is_attached_to_instead_of_relaunched() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    let session_idx = push_skill_session(&mut app, "heimdall");
    app.sessions[session_idx].profile.command = "codex".to_string();
    assert_eq!(app.running_skill_session("heimdall"), Some(session_idx));
    assert_eq!(app.running_skill_harness("heimdall"), Some(Harness::Codex));

    app.mode = Mode::Control;
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    let text = screen(&app);
    assert!(
        text.contains("running [codex]"),
        "the Codex row shows it: {text}"
    );

    // Enter on any row of a running skill attaches (here the Claude row)
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Attached));
    assert_eq!(app.selected, session_idx);
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("already running on codex")),
        "asking for another harness warns: {:?}",
        app.notice
    );

    // Any harness asked for while it runs attaches; no second process.
    let before = app.sessions.len();
    for h in Harness::ALL {
        assert_eq!(app.launch_skill("heimdall", h).unwrap(), session_idx);
        assert_eq!(app.sessions.len(), before);
    }

    // Once it exits the row is no longer running
    app.sessions[session_idx].mark_exited();
    assert_eq!(app.running_skill_session("heimdall"), None);
    app.mode = Mode::Control;
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert!(!screen(&app).contains("running ["));
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn enter_installs_and_launches_on_the_rows_harness() {
    let temp = tempfile::tempdir().unwrap();
    let script = fake_claude(&temp.path().join("bin"));
    let profile = Profile {
        name: "Claude Code".into(),
        command: script.to_string_lossy().into_owned(),
        args: vec![],
        default_dir: Some(temp.path().to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut app = app_in(temp.path(), vec![profile]);
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Claude)));

    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Attached), "{:?}", app.notice);
    let session = &app.sessions[app.selected];
    assert_eq!(session.skill_id.as_deref(), Some("heimdall"));
    assert_eq!(session.profile.name, "Heimdall (claude)");
    assert_eq!(
        session.profile.command,
        script.to_string_lossy(),
        "the configured wrapper path is kept for the harness"
    );
    assert!(
        session
            .profile
            .args
            .last()
            .unwrap()
            .starts_with("/heimdall ")
    );
    let installed = temp.path().join(".claude/skills/heimdall/SKILL.md");
    assert!(
        installed.is_file(),
        "installed into the harness directory before launch"
    );
    assert!(
        temp.path()
            .join(".claude/skills/heimdall/.agent-mux.json")
            .is_file()
    );

    // reopening shows the new state on the Claude row only
    app.mode = Mode::Control;
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    let text = screen(&app);
    assert!(text.contains("running [claude]"), "{text}");
    assert_eq!(
        text.matches("not installed").count(),
        2,
        "codex and agy: {text}"
    );
    app.kill_all();
}

#[tokio::test]
async fn install_and_uninstall_from_the_view() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    let dir = temp.path().join(".claude/skills/heimdall");

    app.handle_key(&key(KeyCode::Char('i')), Instant::now());
    assert!(dir.join("SKILL.md").is_file());
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("installed on Claude Code"))
    );
    let Mode::SkillsView(v) = &app.mode else {
        panic!()
    };
    let st = &v.install[&("heimdall".to_string(), Harness::Claude)];
    assert!(st.installed && st.managed && st.current);
    assert!(screen(&app).contains("installed ✓"));

    // a second install is a no-op
    app.handle_key(&key(KeyCode::Char('i')), Instant::now());
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("is current on Claude Code"))
    );

    // u asks first; n keeps it, y removes it
    app.handle_key(&key(KeyCode::Char('u')), Instant::now());
    let text = screen(&app);
    assert!(
        text.contains("Uninstall Heimdall from Claude Code (claude)? [y/n]"),
        "{text}"
    );
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    assert!(dir.is_dir());
    app.handle_key(&key(KeyCode::Char('u')), Instant::now());
    app.handle_key(&key(KeyCode::Char('y')), Instant::now());
    assert!(!dir.exists(), "removed");
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("removed from Claude Code"))
    );
    assert!(screen(&app).contains("not installed"));

    // a foreign directory is refused unless forced
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "someone else's").unwrap();
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(screen(&app).contains("not managed"));
    app.handle_key(&key(KeyCode::Char('i')), Instant::now());
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.starts_with("install:"))
    );
    app.handle_key(&key(KeyCode::Char('I')), Instant::now());
    assert!(
        dir.join(".agent-mux.json").is_file(),
        "forced over the foreign directory"
    );
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
