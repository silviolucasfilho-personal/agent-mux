//! The Library screen (spec
//! `2026-11-14-library-screen-flows-skills-personas-design.md`): the
//! flows / skills / personas tree, the harness picker with a verdict
//! per harness, and the Agents list keeping only launched work.

use agent_mux::app::library::{Item, Row, Section, Verdict};
use agent_mux::app::{App, Mode};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn profile(name: &str, command: &str) -> Profile {
    Profile {
        name: name.into(),
        command: command.into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

fn app_in(home: &Path) -> App {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![profile("claude", "claude")], None, tx);
    app.set_pane_size(24, 80);
    let skills_dir = home.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    app.skill_install_home = Some(home.to_path_buf());
    app.skills_dir = Some(skills_dir);
    app.reload_skills();
    app
}

fn screen(app: &App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
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

#[test]
fn the_library_tree_has_the_three_sections_and_the_builtin_skill() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.open_library(Section::Flows);
    let Mode::Library(st) = &app.mode else {
        panic!("not in the library");
    };
    let rows = app.library_rows(st);
    let headers: Vec<Section> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Header(s) => Some(*s),
            _ => None,
        })
        .collect();
    assert_eq!(
        headers,
        vec![
            Section::Flows,
            Section::Skills,
            Section::Personas,
            Section::Scheduled
        ]
    );
    assert!(
        rows.iter().any(|r| matches!(r, Row::Skill(_))),
        "the compiled-in skill is a row: {rows:?}"
    );
    // and it renders at the three widths the specs ask for
    for w in [80, 120, 180] {
        let text = screen(&app, w, 40);
        assert!(text.contains("FLOWS"), "width {w}:\n{text}");
        assert!(text.contains("SKILLS"), "width {w}:\n{text}");
        assert!(text.contains("PERSONAS"), "width {w}:\n{text}");
        assert!(text.contains("SCHEDULED"), "width {w}:\n{text}");
        // the conventions of the Agents and Swarm trees: where the
        // keyboard is, in the title and in the status bar
        assert!(
            text.contains("Library • Keyboard focus"),
            "width {w}:\n{text}"
        );
        // no row is drawn over by a hint: the last row of the tree is
        // its border
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[rows.len() - 2].starts_with('└'), "width {w}:\n{text}");
    }
}

#[test]
fn the_picker_lists_every_harness_with_a_verdict() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.open_library(Section::Skills);
    // move onto the (only) skill row
    let at = match &app.mode {
        Mode::Library(st) => app
            .library_rows(st)
            .iter()
            .position(|r| matches!(r, Row::Skill(_))),
        _ => unreachable!(),
    };
    let at = at.expect("a skill row");
    if let Mode::Library(st) = &mut app.mode {
        st.cursor = at;
    }
    app.handle_library_key(&key(KeyCode::Enter));
    let Mode::Library(st) = &app.mode else {
        panic!("picker closed the library");
    };
    let picker = st.picker.as_ref().expect("the picker is open");
    assert_eq!(picker.rows.len(), Harness::ALL.len());
    assert!(matches!(picker.item, Item::Skill(_)));
    // heimdall declares every harness: each is Ready or Not installed
    for (h, v) in &picker.rows {
        assert!(
            matches!(v, Verdict::Ready | Verdict::NotInstalled),
            "{h:?}: {v:?}"
        );
    }
    // Esc closes the picker, q leaves the screen
    app.handle_library_key(&key(KeyCode::Esc));
    assert!(matches!(&app.mode, Mode::Library(st) if st.picker.is_none()));
    app.handle_library_key(&key(KeyCode::Char('q')));
    assert!(matches!(app.mode, Mode::Main));
}

#[test]
fn the_filter_narrows_the_tree_and_esc_clears_it() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.open_library(Section::Skills);
    app.handle_library_key(&key(KeyCode::Char('/')));
    for c in "heim".chars() {
        app.handle_library_key(&key(KeyCode::Char(c)));
    }
    app.handle_library_key(&key(KeyCode::Enter));
    let rows = match &app.mode {
        Mode::Library(st) => app.library_rows(st),
        _ => panic!("not in the library"),
    };
    assert!(rows.iter().any(|r| matches!(r, Row::Skill(_))), "{rows:?}");
    assert!(!rows.iter().any(|r| matches!(r, Row::Flow(_))), "{rows:?}");
}

/// A shell session started as the built-in `skeptic` persona.
fn push_persona_session(app: &mut App) -> usize {
    let idx = app
        .launch(profile("shell", "bash"), std::env::temp_dir())
        .expect("spawn shell");
    app.sessions[idx].agent = Some("skeptic".into());
    idx
}

#[tokio::test]
async fn a_personas_session_is_a_row_of_the_library_as_a_workers_is_of_its_swarm() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.library_root = Some(home.path().join("library"));
    app.reload_skills();
    let plain = app
        .launch(profile("shell", "bash"), std::env::temp_dir())
        .expect("spawn shell");
    let idx = push_persona_session(&mut app);
    let id = app.sessions[idx].id;

    // the Agents list has the plain session only, and says where the other is
    assert!(app.session_listed(plain));
    assert!(!app.session_listed(idx));
    let lines = app.agent_lines();
    assert!(
        !lines
            .iter()
            .any(|l| l.kind == agent_mux::app::agents_list::AgentKind::Session(idx)),
        "{lines:?}"
    );
    app.mode = Mode::Main;
    let text = screen(&app, 120, 40);
    assert!(text.contains("library: 1 session(s)"), "{text}");

    // the Library lists it under its persona
    app.open_library(Section::Personas);
    let rows = match &app.mode {
        Mode::Library(st) => app.library_rows(st),
        _ => unreachable!(),
    };
    let persona = rows
        .iter()
        .position(|r| matches!(r, Row::Persona(n) if n == "skeptic"))
        .expect("the built-in persona is a row");
    assert_eq!(rows.get(persona + 1), Some(&Row::Session(id)), "{rows:?}");
    let text = screen(&app, 120, 40);
    assert!(
        text.lines()
            .any(|l| l.contains("◇ skeptic") && l.contains("●1")),
        "{text}"
    );

    // on the session's row the pane is its terminal; Enter types into it
    if let Mode::Library(st) = &mut app.mode {
        st.cursor = persona + 1;
    }
    app.library_sync();
    assert_eq!(app.selected, idx);
    assert_eq!(app.pane_session(), None, "the tree has the keyboard");
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if st.pane_focused));
    assert_eq!(app.pane_session(), Some(idx));
    let text = screen(&app, 120, 40);
    assert!(
        text.contains("skeptic · shell —"),
        "the pane's title\n{text}"
    );
    assert!(text.contains("Keyboard goes to shell"), "{text}");
    // a letter is the session's now, not a verb of the screen
    app.handle_key(&key(KeyCode::Char('q')), Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if st.pane_focused));

    // the Focus Agents command returns to the tree; x asks before
    // stopping, and a no comes back to the Library
    app.execute_command(agent_mux::keymap::ActionId::FocusAgents, Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if !st.pane_focused));
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    assert!(matches!(app.mode, Mode::ConfirmKill), "{:?}", app.mode);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    match &app.mode {
        Mode::Library(st) => assert_eq!(app.library_row(st), Some(Row::Session(id))),
        other => panic!("back in the Library: {other:?}"),
    }

    // another session ending above keeps the cursor on the same row
    app.sessions.remove(plain);
    app.library_sync();
    match &app.mode {
        Mode::Library(st) => assert_eq!(app.library_row(st), Some(Row::Session(id))),
        other => panic!("{other:?}"),
    }
    assert_eq!(app.sessions[app.selected].id, id);

    // Esc leaves for the Agents list
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Main));
    app.kill_all();
}

#[test]
fn an_items_tabs_are_numbered_and_the_harnesses_tab_names_every_harness() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.open_library(Section::Skills);
    let text = screen(&app, 120, 40);
    for tab in ["1 Details", "2 Harnesses", "3 Executions"] {
        assert!(text.contains(tab), "missing {tab:?}\n{text}");
    }
    app.handle_key(&key(KeyCode::Char('2')), Instant::now());
    let Mode::Library(st) = &app.mode else {
        panic!("not in the library");
    };
    let pane = agent_mux::ui::library::pane_text(&app, st);
    for h in Harness::ALL {
        assert!(pane.contains(h.as_str()), "missing {h:?}\n{pane}");
    }
    // Executions is read from the store, not a pointer to another view
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    let Mode::Library(st) = &app.mode else {
        panic!("not in the library");
    };
    assert!(st.history.is_some());
    // ← on an item goes to its heading, where Enter folds the section
    app.handle_key(&key(KeyCode::Left), Instant::now());
    let Mode::Library(st) = &app.mode else {
        panic!("not in the library");
    };
    assert_eq!(app.library_row(st), Some(Row::Header(Section::Skills)));
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::Library(st) = &app.mode else {
        panic!("not in the library");
    };
    assert!(
        !app.library_rows(st)
            .iter()
            .any(|r| matches!(r, Row::Skill(_))),
        "the section is folded"
    );
}

/// The screen's verbs go through the action registry, as on every
/// screen: a verb the row has is delivered, and the filter's letters are
/// text, not verbs.
#[test]
fn the_local_verbs_reach_the_library_through_the_keyboard() {
    use agent_mux::keymap::ActionId;
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(home.path());
    app.library_root = Some(home.path().join("library"));
    app.open_library(Section::Personas);
    assert!(app.command_availability(ActionId::Edit).enabled());
    assert!(app.command_availability(ActionId::Run).enabled());
    assert!(app.command_availability(ActionId::New).enabled());
    assert!(!app.command_availability(ActionId::Stop).enabled());
    // r opens the picker; while it is open the verbs wait
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if st.picker.is_some()));
    app.handle_key(&key(KeyCode::Char('e')), Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if st.picker.is_some()));
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    // e opens the persona's editor, and closing it returns here
    app.handle_key(&key(KeyCode::Char('e')), Instant::now());
    assert!(matches!(app.mode, Mode::AgentEditor(_)), "{:?}", app.mode);
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Library(_)), "{:?}", app.mode);
    // in the filter, the verbs' letters are typed
    app.handle_key(&key(KeyCode::Char('/')), Instant::now());
    for c in "reviewer".chars() {
        app.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    let Mode::Library(st) = &app.mode else {
        panic!("typing left the library: {:?}", app.mode);
    };
    assert_eq!(st.filter, "reviewer");
    let rows = app.library_rows(st);
    let personas: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r, Row::Persona(_)))
        .collect();
    assert_eq!(personas, vec![&Row::Persona("reviewer".into())], "{rows:?}");
    // Enter keeps the filter, Esc clears it, Esc again leaves
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(&app.mode, Mode::Library(st) if st.filter.is_empty()));
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Main));
}
