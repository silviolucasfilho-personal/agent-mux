//! The loop builder as the user drives it: `f` and `o` in the Loops
//! section, a pattern of the user's own, an edited built-in saved as the
//! user's copy, and `R` restoring it.

use agent_mux::app::loop_builder::{LField, LoopBuilderState, Overlay};
use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::loops::builder::Origin;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::time::Instant;
use tokio::sync::mpsc;

fn app() -> (App, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(40, 160);
    app.library_root = Some(temp.path().join("library"));
    app.skills_dir = Some(temp.path().join("library").join("skills"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.load_loop_registry();
    app.history_sessions.clear();
    (app, temp)
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
}

fn ctrl(app: &mut App, c: char) {
    app.handle_key(
        &KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        Instant::now(),
    );
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn st(app: &App) -> &LoopBuilderState {
    match &app.mode {
        Mode::LoopBuilder(s) => s,
        other => panic!("not in the loop builder: {other:?}"),
    }
}

fn to_loops(app: &mut App) {
    for _ in 0..6 {
        if app.sidebar_section == SidebarSection::Loops {
            return;
        }
        press(app, KeyCode::Tab);
    }
    panic!("Tab never reached the Loops section");
}

fn to_field(app: &mut App, f: LField) {
    let target = LField::ALL.iter().position(|x| *x == f).unwrap();
    while st(app).field > target {
        press(app, KeyCode::Up);
    }
    while st(app).field < target {
        press(app, KeyCode::Down);
    }
}

fn type_field(app: &mut App, f: LField, text: &str) {
    to_field(app, f);
    press(app, KeyCode::Enter);
    ctrl(app, 'u');
    type_text(app, text);
    press(app, KeyCode::Enter);
}

fn select_pattern(app: &mut App, id: &str) {
    press(app, KeyCode::Esc); // to the list, if in the fields
    let at = st(app)
        .items
        .iter()
        .position(|i| i.pattern.id == id)
        .unwrap();
    while st(app).selected > at {
        press(app, KeyCode::Up);
    }
    while st(app).selected < at {
        press(app, KeyCode::Down);
    }
}

#[test]
fn patterns_are_created_edited_as_copies_and_restored() {
    let (mut app, temp) = app();
    let registry = temp.path().join("library/loops/registry.toml");
    to_loops(&mut app);

    // f: a new pattern of the user's own
    press(&mut app, KeyCode::Char('f'));
    assert!(matches!(st(&app).overlay, Some(Overlay::NewName { .. })));
    ctrl(&mut app, 'u');
    type_text(&mut app, "docs-drift");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        st(&app).current_field(),
        Some(LField::Goal),
        "straight to the goal"
    );
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "Find docs that no longer match the code.");
    press(&mut app, KeyCode::Enter);
    type_field(&mut app, LField::Every, "2h");
    // add loop-fix to its skills
    to_field(&mut app, LField::Skills);
    press(&mut app, KeyCode::Enter);
    let Some(Overlay::Multi { options, .. }) = &st(&app).overlay else {
        panic!("the skill list is open");
    };
    let fix = options.iter().position(|o| o.0 == "loop-fix").unwrap();
    for _ in 0..fix {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Enter);
    let text = render(&app);
    assert!(text.contains("Loop patterns"), "{text}");
    assert!(
        text.contains("every 2h ─▶ loop-triage ─▶ loop-rules ─▶ loop-fix"),
        "{text}"
    );
    assert!(text.contains("✓ valid"), "{text}");
    press(&mut app, KeyCode::Char('s'));
    let saved = std::fs::read_to_string(&registry).unwrap();
    assert!(saved.contains("id = \"docs-drift\""), "{saved}");
    assert!(saved.contains("default_interval_s = 7200"), "{saved}");
    let it = st(&app).current().unwrap();
    assert_eq!((it.origin, it.dirty()), (Origin::Yours, false));

    // a built-in, edited and saved: the user's copy
    select_pattern(&mut app, "ci-sweeper");
    press(&mut app, KeyCode::Enter);
    type_field(&mut app, LField::Every, "30m");
    to_field(&mut app, LField::Level);
    press(&mut app, KeyCode::Right); // L2 → L3
    assert!(render(&app).contains("*ci-sweeper"), "unsaved is marked");
    press(&mut app, KeyCode::Char('s'));
    let it = st(&app).current().unwrap();
    assert_eq!(it.origin, Origin::Edited);
    assert_eq!(it.pattern.default_interval_s, 1800);
    assert!(
        std::fs::read_to_string(&registry)
            .unwrap()
            .contains("id = \"ci-sweeper\"")
    );

    // a problem stops the save and says why
    type_field(&mut app, LField::Every, "1m");
    press(&mut app, KeyCode::Char('s'));
    assert!(app.notice.as_ref().unwrap().text.contains("at least 300"));
    press(&mut app, KeyCode::Char('R')); // drop the unsaved edit? no: it is Edited, so it asks
    assert!(matches!(st(&app).overlay, Some(Overlay::Confirm { .. })));
    press(&mut app, KeyCode::Char('y'));
    let it = st(&app).current().unwrap();
    assert_eq!(it.pattern.id, "ci-sweeper");
    assert_eq!(
        (it.origin, it.pattern.default_interval_s),
        (Origin::Builtin, 900)
    );
    let left = std::fs::read_to_string(&registry).unwrap();
    assert!(
        !left.contains("ci-sweeper") && left.contains("docs-drift"),
        "{left}"
    );

    // d deletes a pattern of the user's own; the library file goes with it
    select_pattern(&mut app, "docs-drift");
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('y'));
    assert!(!registry.exists());
    assert!(!st(&app).items.iter().any(|i| i.pattern.id == "docs-drift"));

    // o opens on the first pattern when no loop is selected; Esc closes
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Control));
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(st(&app).selected, 0);
}
