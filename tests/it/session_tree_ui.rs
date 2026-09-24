//! The loop and workflow hierarchy, in the Active sidebar and in the trace
//! browser's Sessions pane: the calls of one run gather under one header,
//! the header folds, and the cursor walks rows rather than list indices.

use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::session::Session;
use agent_mux::tracing::store::model::{LaunchRow, SessionRow, StoreOp, TraceRow, TraceStatus};
use agent_mux::tracing::store::{OpenOptions, open_ro, open_rw};
use agent_mux::tree::{GroupKind, GroupRef};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn profile(name: &str) -> Profile {
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

fn render(app: &App, width: u16, height: u16) -> String {
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

fn wf(run: &str, step: &str) -> GroupRef {
    GroupRef {
        kind: GroupKind::Workflow,
        id: run.into(),
        title: "map-codebase".into(),
        detail: format!("#{run}"),
        label: step.into(),
    }
}

/// An App with `n` shell sessions; `groups[i]` is what session `i`
/// belongs to.
fn app_with_sessions(groups: &[Option<GroupRef>]) -> (App, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(vec![profile("Claude Code")], None, tx.clone());
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.history_sessions.clear();
    for (i, group) in groups.iter().enumerate() {
        let mut s = Session::spawn(
            900 + i,
            profile(&format!("p{i}")),
            std::env::temp_dir(),
            10,
            80,
            tx.clone(),
            &[],
            &[],
        )
        .unwrap();
        s.group = group.clone();
        app.sessions.push(s);
    }
    (app, temp)
}

#[test]
fn a_workflow_runs_steps_gather_under_one_header() {
    let (app, _t) = app_with_sessions(&[
        None,
        Some(wf("abcd1234ef", "read:src")),
        Some(wf("abcd1234ef", "read:tests")),
    ]);
    let out = render(&app, 120, 40);
    assert!(out.contains("▾"), "an open header draws its fold marker");
    assert!(out.contains("⚙"), "a workflow header carries its glyph");
    assert!(out.contains("map-codeba"), "the header names the workflow");
    assert!(out.contains("├"), "the first step hangs off the header");
    assert!(out.contains("└"), "and the last one closes the family");
    assert!(
        out.contains("read:src"),
        "a step is named by its step, not its profile: {out}"
    );
}

#[test]
fn space_folds_the_run_and_the_cursor_skips_the_family() {
    let (mut app, _t) = app_with_sessions(&[
        None,
        Some(wf("abcd1234ef", "read:src")),
        Some(wf("abcd1234ef", "read:tests")),
        None,
    ]);
    app.sidebar_section = SidebarSection::Active;
    // down once: the loose session, then the first step
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    assert_eq!(app.selected, 1);
    // fold the run the step belongs to
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    let out = render(&app, 120, 40);
    assert!(out.contains("▸"), "a folded header draws a closed marker");
    assert!(
        !out.contains("read:src"),
        "the steps are hidden while folded: {out}"
    );
    // one more step down leaves the whole family behind
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    assert_eq!(app.selected, 3, "the folded run is a single stop");
    app.handle_key(&key(KeyCode::Char('k')), Instant::now());
    assert_eq!(app.selected, 1, "and back onto the header it stands for");
    // unfolding brings the steps back
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    assert!(render(&app, 120, 40).contains("read:src"));
}

#[test]
fn the_digits_count_the_rows_the_tree_draws() {
    let (mut app, _t) = app_with_sessions(&[
        None,
        Some(wf("abcd1234ef", "read:src")),
        Some(wf("abcd1234ef", "read:tests")),
    ]);
    app.sidebar_section = SidebarSection::Active;
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    assert_eq!(app.selected, 2, "the third drawn session is the last step");
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    assert_eq!(app.selected, 0);
}

/// A session with no loop or workflow behind it stays a plain row.
#[test]
fn loose_sessions_keep_their_place_and_their_profile() {
    let (app, _t) = app_with_sessions(&[None, None]);
    let out = render(&app, 120, 40);
    assert!(out.contains("p0"), "{out}");
    assert!(out.contains("p1"));
    assert!(!out.contains("▾"), "nothing to group, no headers: {out}");
}

// ---------------------------------------------------------------- store

type LaunchSeed = (&'static str, &'static str, Option<&'static str>);

/// Two workflow steps of one run, one loop run, one hand-started session.
const SEED: [LaunchSeed; 4] = [
    (
        "claude:w1",
        "l-w1",
        Some(
            r#"{"workflow_run_id":"abcd1234ef","workflow":"map-codebase","workflow_step":"read:src","workflow_phase":"Read"}"#,
        ),
    ),
    (
        "claude:w2",
        "l-w2",
        Some(
            r#"{"workflow_run_id":"abcd1234ef","workflow":"map-codebase","workflow_step":"read:tests","workflow_phase":"Read"}"#,
        ),
    ),
    (
        "claude:l1",
        "l-l1",
        Some(
            r#"{"loop_id":"nightly","loop_run_id":"99887766aa","loop_pattern":"pr-babysitter","loop_level":"L2"}"#,
        ),
    ),
    ("claude:p1", "l-p1", None),
];

fn seed_store(dir: &Path) {
    write_launches(dir, &SEED, 0);
}

/// Writes one session, launch and turn per seed, `base` apart in time so a
/// later call lands after the earlier ones.
fn write_launches(dir: &Path, launches: &[LaunchSeed], base: i64) {
    let mut store = open_rw(
        &dir.join("traces.db"),
        OpenOptions {
            prices: agent_mux::tracing::pricing::PriceTable::builtin(),
            run_id: "run-1".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let mut ops = Vec::new();
    for (i, (key, launch_id, meta)) in launches.iter().enumerate() {
        let at = base + 1_000 + i as i64;
        let (_, session_id) = key.split_once(':').unwrap();
        ops.push(StoreOp::Session(SessionRow {
            key: (*key).into(),
            provider: "claude".into(),
            session_id: session_id.into(),
            user_id: None,
            cwd: Some("/proj".into()),
            project_slug: Some("-proj".into()),
            transcript_path: None,
            title: Some(format!("session {session_id}")),
            seen_ns: at,
            extra: None,
        }));
        ops.push(StoreOp::Launch(LaunchRow {
            id: (*launch_id).into(),
            run_id: "run-1".into(),
            agent_mux_session: i as i64,
            profile: "Claude Code".into(),
            provider: "claude".into(),
            cwd: "/proj".into(),
            project_slug: "-proj".into(),
            content_mode: "full".into(),
            correlation_plan: "deterministic".into(),
            correlation: None,
            session_key: Some((*key).into()),
            injected_session_id: true,
            attached: false,
            started_ns: at,
            ended_ns: None,
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: vec![],
            metadata: meta.map(|m| serde_json::from_str(m).unwrap()),
        }));
        ops.push(StoreOp::Trace(TraceRow {
            id: format!("t-{session_id}"),
            session_key: (*key).into(),
            provider: "claude".into(),
            session_id: session_id.into(),
            launch_id: Some((*launch_id).into()),
            ordinal: 1,
            name: format!("turn {session_id}"),
            status: TraceStatus::Closed,
            start_ns: at,
            end_ns: Some(at + 1_000),
            input: None,
            output: None,
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            metadata: None,
        }));
    }
    store.apply(&ops).unwrap();
}

#[test]
fn the_store_reports_the_run_behind_each_traced_session() {
    let temp = tempfile::tempdir().unwrap();
    seed_store(temp.path());
    let conn = open_ro(&temp.path().join("traces.db")).unwrap();
    let groups = agent_mux::tracing::store::query::session_groups(&conn, 100).unwrap();
    assert_eq!(groups.len(), 3, "the hand-started session has no parent");
    let w1 = &groups["claude:w1"];
    assert_eq!(w1.kind, GroupKind::Workflow);
    assert_eq!(w1.id, "abcd1234ef");
    assert_eq!(w1.title, "map-codebase");
    assert_eq!(w1.label, "read:src");
    assert_eq!(groups["claude:w2"].key(), w1.key(), "one run, one header");
    let l1 = &groups["claude:l1"];
    assert_eq!(l1.kind, GroupKind::Loop);
    assert_eq!(l1.id, "nightly");
    assert_eq!(l1.detail, "pr-babysitter");
    assert_eq!(l1.label, "99887766 L2");
    assert!(!groups.contains_key("claude:p1"));
}

#[test]
fn the_trace_browser_draws_the_runs_as_a_tree() {
    let temp = tempfile::tempdir().unwrap();
    seed_store(temp.path());
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![profile("Claude Code")], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    let mut browser = agent_mux::app::TraceBrowserState::new(
        Some(&temp.path().join("traces.db")),
        Some(Path::new("/proj")),
    );
    browser.all_projects = true;
    browser.reload_sessions();
    assert_eq!(browser.sessions.len(), 4);
    let rows = browser.session_rows();
    assert_eq!(rows.len(), 4 + 2, "four sessions under two headers");
    app.mode = Mode::TraceBrowser(Box::new(browser));
    let out = render(&app, 220, 40);
    assert!(out.contains("map-codebase"), "{out}");
    assert!(out.contains("nightly"), "{out}");
    assert!(out.contains("read:tests"), "a step is named by its step");
    assert!(out.contains("99887766 L2"), "a loop run by its run");

    // space folds the run the cursor sits in
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    let out = render(&app, 220, 40);
    assert!(out.contains("▸"), "the header closed: {out}");
}

/// A click lands on a drawn row, not on a session index: a header folds,
/// a child selects. The Active pane starts at row 0, so its first list row
/// is row 1.
#[test]
fn clicking_a_header_folds_it_and_clicking_a_child_selects_it() {
    let (mut app, _t) = app_with_sessions(&[
        None,
        Some(wf("abcd1234ef", "read:src")),
        Some(wf("abcd1234ef", "read:tests")),
    ]);
    app.set_pane_size(30, 100);
    let click = |row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // rows: 1 "p0", 2 header, 3 read:src, 4 read:tests
    app.handle_mouse(click(3), Instant::now());
    assert_eq!(app.selected, 1, "clicked the first step");
    app.handle_mouse(click(2), Instant::now());
    assert!(
        app.collapsed_groups.contains("wf:abcd1234ef"),
        "clicking the header folded the run"
    );
    app.handle_mouse(click(2), Instant::now());
    assert!(
        app.collapsed_groups.is_empty(),
        "and clicking it again opened it"
    );
}

/// A run that starts while the browser is open has to find its header:
/// the live refresh re-reads the parents when a session it has not
/// resolved appears.
#[test]
fn the_live_refresh_groups_a_run_that_started_after_the_browser_opened() {
    let temp = tempfile::tempdir().unwrap();
    seed_store(temp.path());
    let mut browser = agent_mux::app::TraceBrowserState::new(
        Some(&temp.path().join("traces.db")),
        Some(Path::new("/proj")),
    );
    browser.all_projects = true;
    browser.reload_sessions();
    assert_eq!(browser.session_rows().len(), 6);

    write_launches(
        temp.path(),
        &[(
            "claude:w3",
            "l-w3",
            Some(
                r#"{"workflow_run_id":"abcd1234ef","workflow":"map-codebase","workflow_step":"synthesize","workflow_phase":"Write"}"#,
            ),
        )],
        9_000,
    );
    // the refresh is rate-limited to 500 ms, so ask from far enough ahead
    browser.refresh_if_live(Instant::now() + std::time::Duration::from_secs(2));
    assert_eq!(browser.sessions.len(), 5);
    assert_eq!(
        browser.groups.get("claude:w3").map(|g| g.label.as_str()),
        Some("synthesize"),
        "the new step found its run"
    );
    assert_eq!(
        browser.session_rows().len(),
        7,
        "and joined the existing header rather than adding one"
    );
}
