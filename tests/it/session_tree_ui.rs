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
    // scheduled agent files in the real library must not leak in
    app.library_root = Some(temp.path().join("library"));
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
    assert!(out.contains("⚙"), "a workflow run carries its glyph");
    assert!(out.contains("map-cod"), "the row names the workflow");
    assert!(out.contains("├"), "its steps hang off it");
    assert!(
        out.contains("read:src"),
        "a step is named by its step, not its profile: {out}"
    );
    // the steps sit right under their run in the Agents list
    let lines = app.agent_lines();
    let run = lines
        .iter()
        .position(|l| matches!(&l.kind, agent_mux::app::agents_list::AgentKind::Run(k) if k == "wf:abcd1234ef"))
        .expect("a row for the run");
    assert_eq!(
        lines[run + 1].kind,
        agent_mux::app::agents_list::AgentKind::Session(1)
    );
    assert_eq!(
        lines[run + 2].kind,
        agent_mux::app::agents_list::AgentKind::Session(2)
    );
}

#[test]
fn space_folds_an_agent_without_losing_its_sessions() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[None, None]);
    let lines = app.agent_lines();
    let harness = lines
        .iter()
        .position(|l| l.kind == AgentKind::Harness(0))
        .unwrap();
    app.select_agent_line(&lines, harness);
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());

    assert_eq!(app.agent_row(), Some(AgentKind::Harness(0)));
    assert!(
        !app.agent_lines()
            .iter()
            .any(|l| matches!(l.kind, AgentKind::Session(_)))
    );
    assert_eq!(app.sessions.len(), 2);

    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    assert_eq!(
        app.agent_lines()
            .iter()
            .filter(|l| matches!(l.kind, AgentKind::Session(_)))
            .count(),
        2
    );
}

#[test]
fn space_folds_a_section_and_keeps_its_heading_selected() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[Some(wf("abcd1234ef", "read:src"))]);
    let lines = app.agent_lines();
    let flows = lines
        .iter()
        .position(|l| l.kind == AgentKind::Header("flows"))
        .unwrap();
    app.select_agent_line(&lines, flows);
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());

    assert_eq!(app.agent_row(), Some(AgentKind::Header("flows")));
    assert!(
        app.agent_lines()
            .iter()
            .any(|l| l.kind == AgentKind::Header("flows"))
    );
    assert!(
        !app.agent_lines()
            .iter()
            .any(|l| matches!(l.kind, AgentKind::Run(_)))
    );

    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    assert!(
        app.agent_lines()
            .iter()
            .any(|l| matches!(l.kind, AgentKind::Run(_)))
    );
}

#[test]
fn clicking_a_heading_folds_its_rows() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[Some(wf("abcd1234ef", "read:src"))]);
    let lines = app.agent_lines();
    let heights = agent_mux::ui::agent_row_heights(&lines);
    let flows = lines
        .iter()
        .position(|l| l.kind == AgentKind::Header("flows"))
        .unwrap();
    let (agents, _) = agent_mux::ui::sidebar_areas(app.pane_size.0 + 3, 0);
    app.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: agents.y + 1 + heights[..flows].iter().sum::<usize>() as u16,
            modifiers: KeyModifiers::NONE,
        },
        Instant::now(),
    );
    assert_eq!(app.agent_row(), Some(AgentKind::Header("flows")));
    assert!(
        !app.agent_lines()
            .iter()
            .any(|l| matches!(l.kind, AgentKind::Run(_)))
    );
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
    // the harness row counts its sessions; each has a detail line
    assert!(out.contains("2 idle"), "{out}");
    assert!(out.contains("├ 1") && out.contains("└ 2"), "{out}");
    assert!(out.contains("│   p0 · not traced"), "{out}");
    assert!(
        out.contains("    p1 · not traced"),
        "the last stem stops: {out}"
    );
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
fn clicking_a_run_selects_it_and_clicking_a_step_selects_the_session() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[
        None,
        Some(wf("abcd1234ef", "read:src")),
        Some(wf("abcd1234ef", "read:tests")),
    ]);
    app.set_pane_size(30, 100);
    let lines = app.agent_lines();
    // the screen row a line starts on: a session under a harness takes two
    let heights = agent_mux::ui::agent_row_heights(&lines);
    let at = |k: &AgentKind| {
        let i = lines.iter().position(|l| l.kind == *k).unwrap();
        heights[..i].iter().sum::<usize>() as u16
    };
    let (agents, _) = agent_mux::ui::sidebar_areas(app.pane_size.0 + 3, 0);
    let click = |row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: agents.y + 1 + row,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click(at(&AgentKind::Session(1))), Instant::now());
    assert_eq!(app.selected, 1, "clicked the first step");
    app.handle_mouse(
        click(at(&AgentKind::Run("wf:abcd1234ef".into()))),
        Instant::now(),
    );
    assert_eq!(
        app.agent_row(),
        Some(AgentKind::Run("wf:abcd1234ef".into()))
    );
    // a run row has no session keys: Enter does not attach
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Main));
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

/// The list is a tree: a fold arrow only where there is something to
/// fold, headings in capitals with their count, `├`/`└` on sessions.
#[test]
fn the_tree_shows_arrows_only_where_something_folds() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[None, Some(wf("abcd1234ef", "read:src"))]);
    app.profiles.push(profile("Codex"));
    let out = render(&app, 120, 40);
    let row = |needle: &str| {
        out.lines()
            .find(|l| l.contains(needle))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no row {needle:?}: {out}"))
    };
    assert!(row("Claude Code").contains("▾ ❯ Claude Code"), "{out}");
    assert!(
        row("Codex").contains("  ❯ Codex") && !row("Codex").contains('▾'),
        "nothing to fold under an empty harness: {out}"
    );
    assert!(row("FLOWS").contains("▾ FLOWS"), "{out}");
    assert!(
        row("map-cod").contains("▾ ⚙"),
        "a run with sessions folds: {out}"
    );
    assert!(
        row("understand").contains("  · understand") && !row("understand").contains('▾'),
        "a library flow has nothing to fold: {out}"
    );
    assert!(
        row("read:src").contains("└ 2 read:src"),
        "the only step is the last: {out}"
    );
    // the heading counts what is under it, folded or not
    let lines = app.agent_lines();
    let flows = lines
        .iter()
        .find(|l| l.kind == AgentKind::Header("flows"))
        .unwrap();
    // the library's flows and the orphan run
    assert_eq!(flows.children, app.workflow_rows().len() + 1);
    let run = lines
        .iter()
        .find(|l| matches!(l.kind, AgentKind::Run(_)))
        .unwrap();
    assert_eq!((run.depth, run.children), (1, 1));
}

/// `←` folds the row, or climbs to the parent; `→` unfolds, or descends.
#[test]
fn left_and_right_fold_and_walk_the_tree() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[None, None]);
    let lines = app.agent_lines();
    let second = lines
        .iter()
        .position(|l| l.kind == AgentKind::Session(1))
        .unwrap();
    app.select_agent_line(&lines, second);
    // ← on a session: up to its harness
    app.handle_key(&key(KeyCode::Left), Instant::now());
    assert_eq!(app.agent_row(), Some(AgentKind::Harness(0)));
    // ← on the harness: folded
    app.handle_key(&key(KeyCode::Left), Instant::now());
    assert!(app.agent_is_folded(&AgentKind::Harness(0)));
    assert_eq!(app.agent_row(), Some(AgentKind::Harness(0)));
    // → unfolds; → again steps onto the first session
    app.handle_key(&key(KeyCode::Right), Instant::now());
    assert!(!app.agent_is_folded(&AgentKind::Harness(0)));
    assert_eq!(app.agent_row(), Some(AgentKind::Harness(0)));
    app.handle_key(&key(KeyCode::Right), Instant::now());
    assert_eq!(app.agent_row(), Some(AgentKind::Session(0)));
    let out = render(&app, 120, 40);
    assert!(out.contains("├ 1") && out.contains("└ 2"), "{out}");
}

/// A click on an agent's arrow folds it; a click on its name only selects.
#[test]
fn clicking_the_arrow_folds_an_agent() {
    use agent_mux::app::agents_list::AgentKind;
    let (mut app, _t) = app_with_sessions(&[None]);
    let lines = app.agent_lines();
    let heights = agent_mux::ui::agent_row_heights(&lines);
    let harness = lines
        .iter()
        .position(|l| l.kind == AgentKind::Harness(0))
        .unwrap();
    let (agents, _) = agent_mux::ui::sidebar_areas(app.pane_size.0 + 3, 0);
    let click = |column: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: agents.y + 1 + heights[..harness].iter().sum::<usize>() as u16,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click(10), Instant::now());
    assert!(
        !app.agent_is_folded(&AgentKind::Harness(0)),
        "the name selects"
    );
    app.handle_mouse(
        click(agent_mux::ui::agent_arrow_column(&lines[harness])),
        Instant::now(),
    );
    assert!(
        app.agent_is_folded(&AgentKind::Harness(0)),
        "the arrow folds"
    );
}
