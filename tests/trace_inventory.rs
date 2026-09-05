//! The skill inventory against a real store: prompts and skill loads
//! seeded through the writer, the join that reports `missed` and
//! `never triggered`, the turn filter, the tool names a lint reads, the
//! CLI table, and the browser's Skills pane.

use agent_mux::app::{App, BrowserPane, Mode, TraceBrowserState};
use agent_mux::harness::Harness;
use agent_mux::tracing::cli::skills_lines;
use agent_mux::tracing::inventory::{inventory, skill_reports};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::query::{prompt_rows, skill_stats, tool_names, traces_with_skill};
use agent_mux::tracing::store::writer::{WriterConfig, spawn_writer};
use agent_mux::tracing::store::{OpenOptions, open_ro, open_rw};
use std::path::Path;
use std::time::Duration;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn trace(id: &str, ordinal: i64, input: &str, skills: &[&str]) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: "claude:s1".into(),
        provider: "claude".into(),
        session_id: "s1".into(),
        launch_id: Some("l1".into()),
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns: 1_000_000_000 * ordinal,
        end_ns: Some(1_000_000_000 * ordinal + 500),
        input: Some(input.into()),
        output: Some("ok".into()),
        thinking: None,
        skills: (!skills.is_empty()).then(|| skills.iter().map(|s| s.to_string()).collect()),
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        ordinal_salted: false,
        metadata: None,
    }
}

fn tool(id: &str, trace_id: &str, name: &str) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: None,
        obs_type: ObservationType::Tool,
        name: name.into(),
        kind: None,
        start_ns: 1_000_000_000,
        end_ns: Some(1_100_000_000),
        level: Level::Default,
        status_message: None,
        model: None,
        input: None,
        output: None,
        thinking: None,
        usage_raw: None,
        usage: None,
        tool_id: None,
        tool_name: None,
        skill: None,
        mcp_server: None,
        path: None,
        is_error: false,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    }
}

/// A store with three turns: one loaded `deploy`, two mention its
/// trigger phrases without loading it, and one called two tools.
fn seeded_store(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("traces.db");
    let store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-inv".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let handle = spawn_writer(store, WriterConfig::new(30), Box::new(|_, _| {}), None);
    let launch = LaunchRow {
        id: "l1".into(),
        run_id: "run-inv".into(),
        agent_mux_session: 1,
        profile: "Claude Code".into(),
        provider: "claude".into(),
        cwd: "/proj".into(),
        project_slug: "-proj".into(),
        content_mode: "full".into(),
        correlation_plan: "deterministic".into(),
        correlation: Some("deterministic".into()),
        session_key: Some("claude:s1".into()),
        injected_session_id: true,
        attached: false,
        started_ns: 1_000,
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
        metadata: None,
    };
    handle.tx.try_send(StoreOp::Launch(launch)).unwrap();
    handle
        .tx
        .try_send(StoreOp::Trace(trace(
            "t1",
            1,
            "please deploy the app",
            &["deploy"],
        )))
        .unwrap();
    handle
        .tx
        .try_send(StoreOp::Trace(trace("t2", 2, "can you ship it now", &[])))
        .unwrap();
    handle
        .tx
        .try_send(StoreOp::Trace(trace(
            "t3",
            3,
            "Ship It, then deploy the app",
            &["other"],
        )))
        .unwrap();
    handle
        .tx
        .try_send(StoreOp::Observation(tool("o1", "t3", "Bash")))
        .unwrap();
    handle
        .tx
        .try_send(StoreOp::Observation(tool("o2", "t3", "Artifact")))
        .unwrap();
    handle
        .tx
        .try_send(StoreOp::Observation(tool("o3", "t1", "skill: deploy")))
        .unwrap();
    assert!(handle.finish(Duration::from_secs(5)));
    db
}

fn project_with_skills(dir: &Path) -> std::path::PathBuf {
    let cwd = dir.join("proj");
    write(
        &cwd.join(".claude/skills/deploy/SKILL.md"),
        "---\nname: deploy\ndescription: Use when asked to \"deploy the app\" or \"ship it\".\n---\n",
    );
    write(
        &cwd.join(".claude/skills/silent/SKILL.md"),
        "---\nname: silent\ndescription: Never asked for.\n---\n",
    );
    cwd
}

#[test]
fn the_join_reports_missed_triggers_and_never_triggered_skills() {
    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let cwd = project_with_skills(temp.path());
    let conn = open_ro(&db).unwrap();

    let prompts = prompt_rows(&conn, 100).unwrap();
    assert_eq!(prompts.len(), 3, "newest first, skills parsed");
    assert_eq!(prompts[0].trace_id, "t3");
    assert_eq!(prompts[0].skills, vec!["other"]);
    assert_eq!(prompts[2].skills, vec!["deploy"]);

    let defs = inventory(Harness::Claude, &cwd, &temp.path().join("nohome"));
    let stats = skill_stats(&conn).unwrap();
    let reports = skill_reports(&defs, &stats, &prompts);
    let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["deploy", "other", "silent"]);
    let deploy = &reports[0];
    assert_eq!(deploy.stat.as_ref().unwrap().turns_loaded, 1);
    assert_eq!(
        deploy.missed, 2,
        "t2 and t3 mention a trigger without loading it"
    );
    assert_eq!(reports[1].note(), "not on disk");
    assert_eq!(reports[2].note(), "never triggered");

    let lines = skills_lines(&reports);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("skill"), "{}", lines[0]);
    assert!(
        lines[1].starts_with("deploy") && lines[1].contains("project"),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains("not on disk"), "{}", lines[2]);
    assert!(lines[3].contains("never triggered"), "{}", lines[3]);

    let turns = traces_with_skill(&conn, "deploy", 10).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, "t1");
    assert!(traces_with_skill(&conn, "nope", 10).unwrap().is_empty());

    assert_eq!(
        tool_names(&conn, "claude").unwrap(),
        vec!["Artifact", "Bash"]
    );
    assert!(tool_names(&conn, "codex").unwrap().is_empty());
}

#[test]
fn the_browser_skills_pane_lists_and_filters() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Instant;

    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let cwd = project_with_skills(temp.path());
    let browser =
        TraceBrowserState::new(Some(&db), Some(&cwd)).with_home(Some(temp.path().join("nohome")));
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let mut app = App::new(vec![], None, tx);
    app.mode = Mode::TraceBrowser(Box::new(browser));
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

    app.handle_key(&key(KeyCode::Char('K')), Instant::now());
    let Mode::TraceBrowser(b) = &app.mode else {
        panic!("left the browser");
    };
    assert!(b.skills_pane);
    assert_eq!(b.focused, BrowserPane::Sessions);
    let names: Vec<&str> = b.skills.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["deploy", "other", "silent"]);
    assert_eq!(b.selected_skill, 0);

    // j/k move within the skills, Enter filters the turns pane
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    app.handle_key(&key(KeyCode::Char('j')), Instant::now());
    app.handle_key(&key(KeyCode::Char('k')), Instant::now());
    app.handle_key(&key(KeyCode::Char('k')), Instant::now());
    let Mode::TraceBrowser(b) = &app.mode else {
        panic!()
    };
    assert_eq!(b.selected_skill, 0, "clamped at both ends");
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::TraceBrowser(b) = &app.mode else {
        panic!()
    };
    assert_eq!(b.focused, BrowserPane::Turns);
    assert_eq!(b.search_query.as_deref(), Some("skill: deploy"));
    assert_eq!(b.turns.len(), 1);
    assert_eq!(b.turns[0].id, "t1");

    // K again brings the sessions back
    app.handle_key(&key(KeyCode::Char('K')), Instant::now());
    let Mode::TraceBrowser(b) = &app.mode else {
        panic!()
    };
    assert!(!b.skills_pane);

    // without a store the pane still lists what is on disk
    let mut bare =
        TraceBrowserState::new(None, Some(&cwd)).with_home(Some(temp.path().join("nohome")));
    bare.toggle_skills_pane();
    let names: Vec<&str> = bare.skills.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["deploy", "silent"]);
    assert!(bare.skills.iter().all(|r| r.stat.is_none()));
    bare.filter_by_skill();
    assert!(bare.turns.is_empty(), "no store, no filter — and no panic");
}
