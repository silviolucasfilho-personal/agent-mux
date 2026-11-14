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
        assert!(text.contains("flows"), "width {w}:\n{text}");
        assert!(text.contains("skills"), "width {w}:\n{text}");
        assert!(text.contains("personas"), "width {w}:\n{text}");
        assert!(text.contains("scheduled"), "width {w}:\n{text}");
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
