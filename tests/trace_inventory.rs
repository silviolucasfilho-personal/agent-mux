//! The skill inventory against a real store: prompts and skill loads
//! seeded through the writer, the join that reports `missed` and
//! `never triggered`, the turn filter, the tool names a lint reads, the
//! CLI table, and the Skills view.

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
fn the_skills_view_lists_native_skills_with_their_turns_and_statistics() {
    use agent_mux::app::{SkillRow, SkillsTab, SkillsViewState};

    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let cwd = project_with_skills(temp.path());
    let home = temp.path().join("nohome");
    let skills_dir = temp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    let mut view = SkillsViewState::new(Some(&db), &cwd, &home, &home, Some(&skills_dir));
    // grouped by harness: the compiled-in package under each harness it
    // declares, then the project's native Claude skills
    let shape: Vec<String> = view
        .rows
        .iter()
        .map(|r| match r {
            SkillRow::Header(h) => format!("# {}", h.as_str()),
            SkillRow::Package { index, harness } => {
                format!("{}@{}", view.packages[*index].id, harness.as_str())
            }
            SkillRow::Native { index } => format!("native {}", view.native[*index].name),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            "# claude",
            "heimdall@claude",
            "native deploy",
            "native silent",
            "# codex",
            "heimdall@codex",
            "# agy",
            "heimdall@agy",
        ]
    );
    assert_eq!(view.selected, 1, "the first selectable row, never a header");

    // j reaches the native rows; the store knows deploy's turn and misses
    view.step(1);
    assert_eq!(
        view.selected_native().map(|d| d.name.as_str()),
        Some("deploy")
    );
    assert_eq!(view.tabs(), vec![SkillsTab::Details, SkillsTab::Executions]);
    assert!(
        view.launches.is_empty(),
        "native skills have no agent-mux launches"
    );
    assert_eq!(view.turns.len(), 1);
    assert_eq!(view.turns[0].stat.id, "t1");
    assert!(
        !view.turns[0].attributed,
        "the tool row carries no skill attribution"
    );
    let report = view.selected_report().expect("deploy is in the store");
    assert_eq!(report.stat.as_ref().unwrap().turns_loaded, 1);
    assert_eq!(report.missed, 2);
    let detail: String = view
        .detail_lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(detail.contains("project"), "{detail}");
    assert!(
        detail.contains("2 prompt(s) quoted a trigger phrase"),
        "{detail}"
    );

    // headers are skipped in both directions and the ends clamp
    view.step(1);
    assert_eq!(
        view.selected_native().map(|d| d.name.as_str()),
        Some("silent")
    );
    view.step(1);
    assert_eq!(
        view.selected_package().map(|(p, h)| (p.id.as_str(), h)),
        Some(("heimdall", Harness::Codex))
    );
    view.step(-1);
    assert_eq!(
        view.selected_native().map(|d| d.name.as_str()),
        Some("silent")
    );
    view.step(-10);
    assert_eq!(view.selected, 1);

    // a harness filter keeps only that group; the same key clears it
    view.toggle_filter(Harness::Codex);
    assert_eq!(view.rows.len(), 2);
    assert_eq!(
        view.selected_package().map(|(p, h)| (p.id.as_str(), h)),
        Some(("heimdall", Harness::Codex))
    );
    assert!(view.clear_filter());
    assert_eq!(view.rows.len(), 8);
    assert!(!view.clear_filter());

    // without a store the view still lists what is on disk
    let bare = SkillsViewState::new(None, &cwd, &home, &home, Some(&skills_dir));
    assert_eq!(bare.rows.len(), 8);
    assert!(bare.turns.is_empty());
    assert!(bare.error.as_deref().unwrap().contains("tracing is off"));
}

#[test]
fn skill_launches_match_by_id_first_and_by_session_name_for_older_rows() {
    use agent_mux::tracing::store::query::{skill_launches, traces_with_skill_detail};

    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("launches.db");
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-live".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let launch =
        |id: &str, profile: &str, provider: &str, started: i64, meta: Option<serde_json::Value>| {
            LaunchRow {
                id: id.into(),
                run_id: "run-live".into(),
                agent_mux_session: 1,
                profile: profile.into(),
                provider: provider.into(),
                cwd: "/proj".into(),
                project_slug: "-proj".into(),
                content_mode: "full".into(),
                correlation_plan: "deterministic".into(),
                correlation: None,
                session_key: None,
                injected_session_id: false,
                attached: false,
                started_ns: started,
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
                metadata: meta,
            }
        };
    let by_id = launch(
        "l-id",
        "Heimdall (claude)",
        "claude",
        3_000,
        Some(serde_json::json!({"skill_id": "heimdall", "skill_harness": "claude"})),
    );
    let mut by_name = launch("l-name", "Heimdall (claude)", "claude", 2_000, None);
    by_name.ended_ns = Some(2_500);
    by_name.exit_code = Some(0);
    let other_skill = launch(
        "l-other",
        "Heimdall (claude)",
        "claude",
        1_000,
        Some(serde_json::json!({"skill_id": "other"})),
    );
    let codex = launch(
        "l-codex",
        "Heimdall (codex)",
        "codex",
        4_000,
        Some(serde_json::json!({"skill_id": "heimdall", "skill_harness": "codex"})),
    );
    store
        .apply(&[
            StoreOp::Launch(by_id),
            StoreOp::Launch(by_name),
            StoreOp::Launch(other_skill),
            StoreOp::Launch(codex),
        ])
        .unwrap();

    let conn = open_ro(&db).unwrap();
    let rows = skill_launches(&conn, "heimdall", "Heimdall (claude)", Some("claude"), 10).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["l-id", "l-name"],
        "newest first; other skill ids excluded"
    );
    assert!(
        rows[0].by_id && rows[0].live,
        "still running under the open run"
    );
    assert!(!rows[1].by_id && !rows[1].live);
    assert_eq!(rows[1].exit_code, Some(0));
    assert_eq!(rows[0].turns, 0);

    let all = skill_launches(&conn, "heimdall", "Heimdall (claude)", None, 10).unwrap();
    assert_eq!(all.len(), 3, "every harness when no provider is given");
    assert_eq!(all[0].id, "l-codex");

    assert!(
        traces_with_skill_detail(&conn, "heimdall", 10)
            .unwrap()
            .is_empty()
    );
}
