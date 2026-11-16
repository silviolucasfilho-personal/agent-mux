//! The Factory screen and the factory's inbox rows as the operator drives
//! them, over a temporary store. Nothing here runs a harness: the screen
//! starts `agent-mux factory …` as a process, and the tests point that at
//! a script that records its arguments.

#![cfg(unix)]

use agent_mux::app::{App, Mode};
use agent_mux::factory::{Item, Source, State, brain, intake, store};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

struct Fixture {
    app: App,
    temp: tempfile::TempDir,
    ws: PathBuf,
    db: PathBuf,
}

fn conn(db: &Path) -> rusqlite::Connection {
    agent_mux::tracing::store::open_aux(db).unwrap()
}

/// An App over an empty store, with a script in place of the binary.
fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let db = root.join("traces.db");
    let _ =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    let ws = root.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let exe = root.join("fake-agent-mux");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\n",
            root.join("started").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    app.library_root = Some(root.join("library"));
    app.skills_dir = Some(root.join("library").join("skills"));
    app.skill_install_home = Some(root.join("home"));
    app.loops_file = Some(root.join("loops.json"));
    app.runtime_dir = Some(root.join("runtime"));
    app.trace_db_path = Some(db.clone());
    app.factory_exe = Some(exe);
    app.history_sessions.clear();
    Fixture { app, temp, ws, db }
}

fn add(f: &Fixture, title: &str, state: State, issue: Option<&str>) -> Item {
    let mut it = match issue {
        Some(n) => Item::new(&f.ws, "o/r", Source::Issue, n, title),
        None => Item::new(&f.ws, "o/r", Source::Manual, "", title),
    };
    it.state = state;
    let c = conn(&f.db);
    store::insert(&c, &it).unwrap();
    store::update(&c, &mut it).unwrap();
    it
}

fn render(app: &App) -> String {
    let (width, height) = (120, 30);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// What the fake binary was started with, once it has run.
fn started(f: &Fixture, lines: usize) -> Vec<String> {
    let file = f.temp.path().canonicalize().unwrap().join("started");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let got: Vec<String> = std::fs::read_to_string(&file)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default();
        if got.len() >= lines || Instant::now() > deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_screen_lists_items_by_what_they_wait_on_and_shows_the_selected_one() {
    let mut f = fixture();
    add(&f, "Reset email arrives twice", State::Running, Some("214"));
    let asks = add(&f, "Export CSV", State::NeedsAnswer, Some("209"));
    let mut done = add(&f, "Typo in docs", State::Delivered, Some("198"));
    done.pr_number = Some(221);
    done.pr_url = Some("https://github.com/o/r/pull/221".into());
    store::update(&conn(&f.db), &mut done).unwrap();
    store::add_event(
        &conn(&f.db),
        &asks.id,
        "state.needs-answer",
        serde_json::json!(["Which columns?"]),
    )
    .unwrap();

    f.app.open_factory_in(Some(f.ws.clone()));
    let text = render(&f.app);
    assert!(text.contains("factory · proj · 1 need you"), "{text}");
    for want in [
        "▸ running (1)",
        "#214 Reset email arrives twice",
        "▸ needs you (1)",
        "#209 Export CSV",
        "▸ delivered (1)",
        "PR #221",
        "▸ brain",
        "[1 Item]",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    // the running item has no live process in this test: the screen says so
    assert!(text.contains("its process is gone"), "{text}");

    // down to the item that asks; its thread has the recorded event
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Char('3'));
    let text = render(&f.app);
    assert!(text.contains("[3 Thread]"), "{text}");
    assert!(text.contains("state.needs-answer"), "{text}");
    assert!(text.contains("Which columns?"), "{text}");

    // Esc closes
    press(&mut f.app, KeyCode::Esc);
    assert!(matches!(f.app.mode, Mode::Main));
}

#[test]
fn new_answer_and_retry_start_the_factory_command_and_stop_cancels() {
    let mut f = fixture();
    let asks = add(&f, "Export CSV", State::NeedsAnswer, None);
    let stuck = add(&f, "Flaky login", State::Unresolved, None);
    f.app.open_factory_in(Some(f.ws.clone()));
    let ws = f.ws.to_string_lossy().into_owned();

    press(&mut f.app, KeyCode::Char('n'));
    assert!(render(&f.app).contains("what is wanted, or #<issue>"));
    type_text(&mut f.app, "add a changelog");
    press(&mut f.app, KeyCode::Enter);
    assert_eq!(
        started(&f, 1),
        vec![format!("factory run add a changelog --workspace {ws}")]
    );

    // the two items are under "needs you", newest change first
    let Mode::Factory(v) = &mut f.app.mode else {
        panic!("the screen stays open");
    };
    assert!(v.select(&asks.id));
    press(&mut f.app, KeyCode::Char('a'));
    type_text(&mut f.app, "csv");
    press(&mut f.app, KeyCode::Enter);
    let Mode::Factory(v) = &mut f.app.mode else {
        panic!("the screen stays open");
    };
    assert!(v.select(&stuck.id));
    press(&mut f.app, KeyCode::Char('r'));
    let got = started(&f, 3);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.contains(&format!("factory answer {} csv --workspace {ws}", asks.id)));
    assert!(got.contains(&format!("factory retry {}", stuck.id)));

    // x asks, y cancels the item in the store
    press(&mut f.app, KeyCode::Char('x'));
    assert!(render(&f.app).contains(&format!("stop and cancel {}?", stuck.id)));
    press(&mut f.app, KeyCode::Char('y'));
    let after = store::get(&conn(&f.db), &stuck.id).unwrap().unwrap();
    assert_eq!(after.state, State::Cancelled);
    let events = store::events_of(&conn(&f.db), &stuck.id).unwrap();
    assert_eq!(events.last().unwrap().kind, "state.cancelled");
}

#[test]
fn a_brain_note_waits_in_the_inbox_until_the_operator_decides() {
    let mut f = fixture();
    let it = add(&f, "Typo in docs", State::Delivered, None);
    let stuck = add(&f, "Flaky login", State::Failed, None);
    for note in ["There is no test suite; check by reading.", "Never keep me"] {
        store::add_event(&conn(&f.db), &it.id, "brain.note", serde_json::json!(note)).unwrap();
    }
    f.app.on_tick(Instant::now());
    assert_eq!(f.app.inbox_count(), 3, "a failed item and two notes");

    f.app.open_inbox();
    let text = render(&f.app);
    for want in [
        "Factory",
        "failed",
        "Flaky login",
        "brain note",
        "There is no test suite",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    // the item first; Enter on the first note adds it to the brain
    press(&mut f.app, KeyCode::Down);
    assert!(render(&f.app).contains("add it to the brain"));
    press(&mut f.app, KeyCode::Enter);
    let library = f.temp.path().canonicalize().unwrap().join("library");
    let page = brain::read(&library, &f.ws);
    assert!(
        page.contains("There is no test suite; check by reading."),
        "{page}"
    );
    // d discards the second: nothing more is written
    press(&mut f.app, KeyCode::Char('d'));
    assert_eq!(brain::read(&library, &f.ws), page);
    assert!(store::pending_brain_notes(&conn(&f.db)).unwrap().is_empty());
    let kinds: Vec<String> = store::events_of(&conn(&f.db), &it.id)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"brain.note.added".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"brain.note.discarded".to_string()),
        "{kinds:?}"
    );

    // Enter on the item opens the Factory screen on it
    press(&mut f.app, KeyCode::Up);
    press(&mut f.app, KeyCode::Enter);
    let Mode::Factory(v) = &f.app.mode else {
        panic!("Enter on a work item opens the factory");
    };
    assert_eq!(
        v.selected_item().map(|i| i.id.as_str()),
        Some(stuck.id.as_str())
    );
}

#[test]
fn the_label_poll_starts_on_the_tick_only_when_intake_is_on() {
    let mut f = fixture();
    // a GitHub origin is what makes a folder pollable
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&f.ws)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "."]);
    git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
    f.app.factory_workspace_override = Some(f.ws.clone());

    let t0 = Instant::now();
    f.app.on_tick(t0);
    assert!(started(&f, 0).is_empty(), "intake is off by default");

    f.app.factory.intake_label = true;
    f.app.factory.poll_interval_s = 300;
    f.app.on_tick(t0);
    f.app.on_tick(t0 + Duration::from_secs(10));
    let ws = f.ws.to_string_lossy().into_owned();
    assert_eq!(
        started(&f, 1),
        vec![format!("factory poll --workspace {ws}")]
    );
    f.app.on_tick(t0 + Duration::from_secs(301));
    assert_eq!(started(&f, 2).len(), 2, "one poll per interval");
    // the poll's output is kept for the operator
    let log = intake::dir(&f.temp.path().canonicalize().unwrap().join("runtime")).join("poll.log");
    assert!(log.is_file());
}
