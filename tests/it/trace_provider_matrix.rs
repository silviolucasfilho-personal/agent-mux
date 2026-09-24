//! A provider-neutral workbench fixture. The protocol parsers have their own
//! fixtures; this protects the row contract exposed to workbench views.

use agent_mux::tracing::loops::loop_metrics;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::query::{
    agent_stats, list_observations_tree, list_traces, skill_stats,
};
use agent_mux::tracing::store::{OpenOptions, open_rw};
use agent_mux::tracing::usage::NormalizedUsage;

fn launch(id: &str, provider: &str) -> LaunchRow {
    LaunchRow {
        id: id.into(),
        run_id: "matrix".into(),
        agent_mux_session: 1,
        profile: provider.into(),
        provider: provider.into(),
        cwd: "/fixture".into(),
        project_slug: "fixture".into(),
        content_mode: "full".into(),
        correlation_plan: "deterministic".into(),
        correlation: Some("deterministic".into()),
        session_key: Some(format!("{provider}:{id}")),
        injected_session_id: false,
        attached: false,
        started_ns: 1,
        ended_ns: Some(9_000_000_000),
        termination: Some("exited".into()),
        exit_code: Some(0),
        parse_errors: Some(0),
        dropped_ops: Some(0),
        reported_cost_usd: None,
        reported_lines_added: None,
        reported_lines_removed: None,
        agent_mux_version: "test".into(),
        user_id: None,
        release: None,
        environment: None,
        tags: vec![],
        metadata: None,
    }
}

fn trace(id: &str, provider: &str) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: format!("{provider}:launch-{provider}"),
        provider: provider.into(),
        session_id: format!("launch-{provider}"),
        launch_id: Some(format!("launch-{provider}")),
        ordinal: 1,
        name: "fixture turn".into(),
        status: TraceStatus::Closed,
        start_ns: 1_000_000_000,
        end_ns: Some(8_000_000_000),
        input: Some("use alpha and beta, then delegate verification".into()),
        output: Some("done".into()),
        thinking: None,
        skills: Some(vec!["alpha".into(), "beta".into()]),
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        metadata: None,
    }
}

fn observation(
    id: &str,
    trace_id: &str,
    parent: Option<&str>,
    obs_type: ObservationType,
    name: &str,
    skill: Option<&str>,
    span: (i64, i64),
) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: parent.map(str::to_string),
        obs_type,
        name: name.into(),
        kind: None,
        start_ns: span.0,
        end_ns: Some(span.1),
        level: Level::Default,
        status_message: None,
        model: (obs_type == ObservationType::Generation).then(|| "claude-haiku-4-5".into()),
        input: Some(format!("{name} input")),
        output: Some(format!("{name} output")),
        thinking: None,
        usage_raw: None,
        usage: (obs_type == ObservationType::Generation).then(|| NormalizedUsage {
            input: Some(20),
            output: Some(5),
            total: Some(25),
            ..Default::default()
        }),
        tool_id: (obs_type == ObservationType::Tool).then(|| id.into()),
        tool_name: (obs_type == ObservationType::Tool).then(|| name.into()),
        skill: skill.map(str::to_string),
        mcp_server: None,
        path: None,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn every_provider_exposes_two_skills_and_a_nested_agent_to_workbench_queries() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = open_rw(
        &temp.path().join("matrix.db"),
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "matrix".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    for provider in ["claude", "codex", "antigravity"] {
        let launch_id = format!("launch-{provider}");
        let trace_id = format!("turn-{provider}");
        let generation = format!("gen-{provider}");
        let agent = format!("agent-{provider}");
        store
            .apply(&[
                StoreOp::Launch(launch(&launch_id, provider)),
                StoreOp::Trace(trace(&trace_id, provider)),
                StoreOp::Observation(observation(
                    &generation,
                    &trace_id,
                    None,
                    ObservationType::Generation,
                    "model",
                    Some("alpha"),
                    (1_100_000_000, 2_000_000_000),
                )),
                StoreOp::Observation(observation(
                    &format!("tool-alpha-{provider}"),
                    &trace_id,
                    Some(&generation),
                    ObservationType::Tool,
                    "Read",
                    Some("alpha"),
                    (2_100_000_000, 2_300_000_000),
                )),
                StoreOp::Observation(observation(
                    &agent,
                    &trace_id,
                    Some(&generation),
                    ObservationType::Agent,
                    "agent: verifier",
                    None,
                    (2_400_000_000, 6_000_000_000),
                )),
                StoreOp::Observation(observation(
                    &format!("tool-beta-{provider}"),
                    &trace_id,
                    Some(&agent),
                    ObservationType::Tool,
                    "Write",
                    Some("beta"),
                    (3_000_000_000, 3_300_000_000),
                )),
            ])
            .unwrap();
    }
    let conn = store.conn();
    let skills = skill_stats(conn).unwrap();
    assert_eq!(
        skills
            .iter()
            .find(|s| s.skill == "alpha")
            .unwrap()
            .turns_loaded,
        3
    );
    assert_eq!(skills.iter().find(|s| s.skill == "beta").unwrap().tools, 3);
    let agents = agent_stats(conn).unwrap();
    assert_eq!((agents.len(), agents[0].invocations), (1, 3));
    for provider in ["claude", "codex", "antigravity"] {
        let turn = list_traces(conn, &format!("{provider}:launch-{provider}"))
            .unwrap()
            .pop()
            .unwrap();
        let rows = list_observations_tree(conn, &turn.id).unwrap();
        assert!(rows.iter().any(|r| r.obs_type == "agent"));
        assert!(rows.iter().any(|r| r.skill.as_deref() == Some("alpha")));
        assert!(rows.iter().any(|r| r.skill.as_deref() == Some("beta")));
        let metrics = loop_metrics(&turn, &rows);
        assert_eq!((metrics.tool_calls, metrics.subagents), (2, 1));
        assert!(metrics.model_ms > 0);
    }
}
