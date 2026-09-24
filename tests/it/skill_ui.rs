//! The Agents sidebar (Heimdall on the main screen: navigation, the harness
//! picker, the one-session rule) and the Skills workbench (`S`): grouping by
//! harness, editing, validation, launching, navigation, and install state.

use agent_mux::app::{
    App, Mode, SidebarSection, SkillLauncherField, SkillRow, SkillsPane, SkillsTab,
};
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

// ---------------------------------------------------------------- Agents

#[tokio::test]
async fn heimdall_is_listed_in_the_agents_section_and_the_picker_defaults_to_its_harness() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    let heimdall = app
        .skills
        .iter()
        .position(|s| s.id == "heimdall")
        .expect("compiled-in heimdall");
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);
    app.selected_agent = heimdall;

    // the agent owns the main pane: its telemetry briefing, not a terminal
    let text = screen(&app);
    assert!(text.contains("Agents ["), "sidebar section is named Agents");
    assert!(text.contains("Heimdall"));
    assert!(
        text.contains("Executive Briefing"),
        "trace.read agents preview the briefing: {text}"
    );

    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::SkillLauncher(ref state) = app.mode else {
        panic!("Enter on an agent opens the harness picker");
    };
    assert_eq!(state.skill_id, "heimdall");
    assert_eq!(state.selected_harness(), Harness::Antigravity);
    assert_eq!(state.harnesses, Harness::ALL.to_vec());

    // Tab leaves the Workspace text field; the shortcut then selects.
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    if let Mode::SkillLauncher(ref state) = app.mode {
        assert_eq!(state.selected_harness(), Harness::Claude);
    }
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));

    // h also opens it while Agents is focused
    app.handle_key(&key(KeyCode::Char('h')), Instant::now());
    assert!(matches!(app.mode, Mode::SkillLauncher(_)));
    app.kill_all();
}

#[tokio::test]
async fn a_running_agent_is_attached_to_instead_of_relaunched() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    let heimdall = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    let session_idx = push_skill_session(&mut app, "heimdall");
    app.sessions[session_idx].profile.command = "codex".to_string();
    assert_eq!(app.running_skill_session("heimdall"), Some(session_idx));
    assert_eq!(app.running_skill_harness("heimdall"), Some(Harness::Codex));

    app.mode = Mode::Control;
    app.sidebar_section = SidebarSection::Agents;
    app.selected_agent = heimdall;
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
    app.sidebar_section = SidebarSection::Agents;
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::SkillLauncher(_)));
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn the_picker_installs_and_launches_on_the_chosen_harness() {
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
    app.sidebar_section = SidebarSection::Agents;
    app.selected_agent = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
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
    let installed = temp.path().join(".claude/skills/heimdall");
    assert!(
        installed.join("SKILL.md").is_file(),
        "installed before launch"
    );
    assert!(installed.join(".agent-mux.json").is_file());

    // the Skills view reports the same launch on the Claude row only
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

#[cfg(unix)]
#[tokio::test]
async fn the_skill_launcher_selects_a_workspace_before_launch() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let selected = workspace.join("selected");
    std::fs::create_dir_all(&selected).unwrap();
    let script = fake_claude(&temp.path().join("bin"));
    let profile = Profile {
        name: "Claude Code".into(),
        command: script.to_string_lossy().into_owned(),
        args: vec![],
        default_dir: Some(workspace.to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut app = app_in(temp.path(), vec![profile]);
    app.sidebar_section = SidebarSection::Agents;
    app.selected_agent = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    assert!(matches!(app.mode, Mode::SkillLauncher(_)));
    let text = screen(&app);
    assert!(text.contains("Workspace"), "{text}");
    assert!(text.contains("Select subfolder"), "{text}");

    app.handle_key(&key(KeyCode::Down), Instant::now());
    for c in "selected".chars() {
        app.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    assert!(matches!(app.mode, Mode::Attached), "{:?}", app.notice);
    assert_eq!(app.sessions[app.selected].dir, selected);
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

// ------------------------------------------------------------ Skills view

#[test]
fn workbench_restores_the_selected_skill_and_harness() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    std::fs::create_dir_all(temp.path().join("skills/plain")).unwrap();
    std::fs::write(
        temp.path().join("skills/plain/SKILL.md"),
        "---\nname: plain\ndescription: Plain test skill.\n---\nBody.\n",
    )
    .unwrap();
    app.reload_skills();
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());

    let Mode::SkillsView(view) = &mut app.mode else {
        panic!()
    };
    assert!(view.select_package("plain", Harness::Codex));
    assert_eq!(
        view.selected_identity(),
        Some(("plain".to_string(), Harness::Codex))
    );
}

#[test]
fn e_edits_a_managed_package_and_keeps_workbench_context() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    app.library_root = Some(temp.path().to_path_buf());
    app.editor = Some("true".into());
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());

    app.handle_key(&key(KeyCode::Char('e')), Instant::now());
    let request = app.take_editor_request().expect("skill editor request");
    assert_eq!(request.asset_id, "skills/heimdall/SKILL.md");
    assert_eq!(request.path, temp.path().join("skills/heimdall/SKILL.md"));
    assert!(request.path.is_file(), "built-in text is materialized");
    assert!(matches!(app.mode, Mode::SkillsView(_)));

    std::fs::write(
        &request.path,
        "---\nname: heimdall\ndescription: Edited test package.\n---\nBody.\n",
    )
    .unwrap();
    app.editor_finished(request, Ok(()));
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Claude)));
    let Mode::SkillsView(view) = &app.mode else {
        panic!()
    };
    assert_eq!(
        view.selected_package().unwrap().0.description,
        "Edited test package."
    );
    assert!(app.notice.as_ref().unwrap().text.contains("saved"));

    app.handle_key(&key(KeyCode::Char('e')), Instant::now());
    let request = app.take_editor_request().unwrap();
    std::fs::write(
        &request.path,
        "---\nname: other\ndescription: Wrong package id.\n---\nBody.\n",
    )
    .unwrap();
    app.editor_finished(request, Ok(()));
    assert_eq!(
        selected(&app),
        Some(("heimdall".into(), Harness::Claude)),
        "an invalid edit remains selected so it can be repaired"
    );
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("frontmatter name"),
        "{:?}",
        app.notice
    );
    app.handle_key(&key(KeyCode::Char('v')), Instant::now());
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("frontmatter name")
    );
    app.handle_key(&key(KeyCode::Char('l')), Instant::now());
    assert!(app.sessions.is_empty());
    let notice = &app.notice.as_ref().unwrap().text;
    assert!(
        notice.contains("invalid") && notice.contains("not launched"),
        "{notice}"
    );
}

#[test]
fn v_validates_the_current_package_content() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    let dir = temp.path().join("skills/plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: plain\ndescription: Valid package.\n---\nBody.\n",
    )
    .unwrap();
    app.reload_skills();
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    let Mode::SkillsView(view) = &mut app.mode else {
        panic!()
    };
    assert!(view.select_package("plain", Harness::Claude));

    app.handle_key(&key(KeyCode::Char('v')), Instant::now());
    assert_eq!(app.notice.as_ref().unwrap().text, "plain is valid");

    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: other\ndescription: Broken identity.\n---\n",
    )
    .unwrap();
    app.handle_key(&key(KeyCode::Char('v')), Instant::now());
    let notice = &app.notice.as_ref().unwrap().text;
    assert!(notice.contains("frontmatter name"), "{notice}");
}

#[cfg(unix)]
#[tokio::test]
async fn l_launches_the_selected_harness_and_reopening_s_restores_executions() {
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

    app.handle_key(&key(KeyCode::Char('l')), Instant::now());
    assert!(matches!(app.mode, Mode::Attached), "{:?}", app.notice);
    assert_eq!(
        app.sessions[app.selected].skill_id.as_deref(),
        Some("heimdall")
    );
    assert!(
        temp.path()
            .join(".claude/skills/heimdall/SKILL.md")
            .is_file()
    );

    app.mode = Mode::Control;
    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert_eq!(selected(&app), Some(("heimdall".into(), Harness::Claude)));
    let Mode::SkillsView(view) = &app.mode else {
        panic!()
    };
    assert_eq!(view.tab, SkillsTab::Executions);
    app.kill_all();
}

#[tokio::test]
async fn s_opens_the_workbench_grouped_by_harness() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
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
    assert!(
        !text.contains("[i] install") && !text.contains("[Enter] launch"),
        "the view is read-only: {text}"
    );

    // Enter only moves focus; explicit l is required to launch.
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::SkillsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.focus, SkillsPane::Detail);
    assert!(!temp.path().join(".claude/skills/heimdall").exists());
    assert!(app.sessions.is_empty());
    app.handle_key(&key(KeyCode::Left), Instant::now());

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
async fn the_view_has_details_and_executions_tabs_and_reflects_install_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_in(temp.path(), vec![shell_profile("test")]);
    std::fs::create_dir_all(temp.path().join("skills/plain")).unwrap();
    std::fs::write(
        temp.path().join("skills/plain/SKILL.md"),
        "---\nname: plain\ndescription: A plain skill with no telemetry.\n---\nBody.\n",
    )
    .unwrap();
    app.reload_skills();
    let heimdall = app
        .skills
        .iter()
        .find(|s| s.id == "heimdall")
        .unwrap()
        .clone();
    agent_mux::skill::install::install(&heimdall, Harness::Codex, temp.path(), false).unwrap();

    app.handle_key(&key(KeyCode::Char('S')), Instant::now());
    assert_eq!(
        shape(&app)[..3],
        ["# claude", "heimdall@claude", "plain@claude"]
    );
    let tabs = |app: &App| match &app.mode {
        Mode::SkillsView(v) => (v.tabs(), v.tab),
        _ => panic!(),
    };
    assert_eq!(
        tabs(&app),
        (
            vec![SkillsTab::Details, SkillsTab::Executions],
            SkillsTab::Details
        )
    );
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Executions);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Details);
    app.handle_key(&key(KeyCode::BackTab), Instant::now());
    assert_eq!(tabs(&app).1, SkillsTab::Executions);

    // the Codex row shows the CLI-made install; Claude and agy do not
    let text = screen(&app);
    assert_eq!(text.matches("installed ✓").count(), 1, "{text}");
    let Mode::SkillsView(v) = &app.mode else {
        panic!()
    };
    let codex = &v.install[&("heimdall".to_string(), Harness::Codex)];
    assert!(codex.installed && codex.managed && codex.current);
    assert!(!v.install[&("heimdall".to_string(), Harness::Claude)].installed);

    // a foreign directory reads as not managed after a rescan
    let dir = temp.path().join(".claude/skills/plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "someone else's").unwrap();
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(screen(&app).contains("not managed"));
    app.kill_all();
}

/// The Workspace field is a text field: every character belongs to the
/// path. The harness shortcuts (`1-3`, `c`, `x`, `a`) may not swallow the
/// letters of an ordinary directory name — they apply once the Harness
/// field has focus.
#[cfg(unix)]
#[tokio::test]
async fn the_workspace_field_accepts_the_harness_shortcut_letters() {
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
    app.sidebar_section = SidebarSection::Agents;
    app.selected_agent = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    app.handle_key(&key(KeyCode::Enter), Instant::now());

    let before = match &app.mode {
        Mode::SkillLauncher(s) => s.workspace.clone(),
        _ => panic!("the launcher did not open"),
    };
    // Every character a real path segment can contain, shortcuts included.
    for c in "/ax1c2x3".chars() {
        app.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    match &app.mode {
        Mode::SkillLauncher(s) => {
            assert_eq!(
                s.workspace,
                format!("{before}/ax1c2x3"),
                "the path field must receive the shortcut letters"
            );
            assert_eq!(
                s.field,
                SkillLauncherField::Workspace,
                "typing must not move focus to the harness list"
            );
        }
        _ => panic!("left the launcher"),
    }

    // The shortcuts still select a harness once that field has focus.
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    match &app.mode {
        Mode::SkillLauncher(s) => {
            assert_eq!(s.field, SkillLauncherField::Harness);
            assert_eq!(s.selected, 1, "x is the second harness");
        }
        _ => panic!("left the launcher"),
    }
    app.kill_all();
}
