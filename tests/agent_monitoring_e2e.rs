use agent_mux::agent::budgets::Budget;
use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::managed::{ManagedEvent, ScriptedManagedAdapter};
use agent_mux::agent::state::{AgentScope, StateStore};
use agent_mux::agent::watch::{Watcher, WatcherConfig};
use agent_mux::tracing::store;
use std::path::Path;
use std::sync::Arc;

#[test]
fn default_budget_is_bounded() {
    let budget = Budget::default();
    assert_eq!(budget.max_turns, 3);
    assert_eq!(budget.timeout_seconds, 120);
    assert_eq!(budget.max_cost_usd, None);
}

#[tokio::test]
async fn e2e_monitoring_review_cycle_and_restart_guarantees() {
    let temp = tempfile::tempdir().unwrap();
    let trace_path = temp.path().join("traces.db");
    let state_path = temp.path().join("agent-state.db");

    // 1. Initialize store
    let mut opts = store::OpenOptions::default();
    opts.run_id = "run-e2e".to_string();
    let store = store::open_rw(&trace_path, opts).unwrap();

    // 2. Define audit agent with opt-in automatic monitoring
    let source = r#"---
id: audit
harnesses: [codex]
triggers:
  - event: process_exit
    prompt: "Investigate process exit"
    debounce_ms: 0
monitoring:
  automatic: true
  max_jobs_per_hour: 3
  max_concurrent_jobs: 1
---
Auditor instructions.
"#;
    let def = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    let scope = AgentScope {
        agent_id: def.id.clone(),
        source_path: "/work/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };

    let adapter = Arc::new(ScriptedManagedAdapter::new(vec![
        ManagedEvent::Started,
        ManagedEvent::TurnStarted { turn: 1 },
        ManagedEvent::Completed {
            output: "Session exit verified without errors.".into(),
        },
    ]));

    // 3. Create watcher
    let mut watcher = Watcher::new(
        scope.clone(),
        def.clone(),
        trace_path.clone(),
        state_path.clone(),
        adapter.clone(),
        WatcherConfig::default(),
    )
    .unwrap();

    // 4. Ingest session exit change in trace store
    store
        .conn()
        .execute_batch(
            "INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
             VALUES('claude:sess_e2e','claude','sess_e2e',1000,2000);
             UPDATE sessions SET last_seen_ns = 3000 WHERE key = 'claude:sess_e2e';",
        )
        .unwrap();

    // 5. Watcher tick detects change, runs bounded job, records briefing
    watcher.tick(1_000_000_000).await.unwrap();
    assert_eq!(watcher.executed_jobs_count(), 1);

    // 6. Check state store: briefing recorded, review cursor remains 0 until explicit acknowledgment
    let mut state = StateStore::open(&state_path).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 0);

    // Retrieve briefing ID
    let briefing_id: String = state
        .conn()
        .query_row(
            "SELECT id FROM briefings WHERE scope_key = ?1",
            rusqlite::params![scope.scope_key()],
            |r| r.get(0),
        )
        .unwrap();

    // 7. Explicit acknowledgment advances review cursor
    state.acknowledge(&scope, &briefing_id).unwrap();
    let reviewed_after_ack = state.reviewed_through(&scope).unwrap();
    assert!(reviewed_after_ack > 0);

    // 8. Restart watcher: reopening preserves review state
    let mut watcher2 = Watcher::new(
        scope.clone(),
        def,
        trace_path.clone(),
        state_path.clone(),
        adapter,
        WatcherConfig::default(),
    )
    .unwrap();

    // Tick without new changes executes 0 new jobs
    watcher2.tick(2_000_000_000).await.unwrap();
    assert_eq!(watcher2.executed_jobs_count(), 0);

    // Verify review progress was preserved across watcher restart
    let state2 = StateStore::open(&state_path).unwrap();
    assert_eq!(state2.reviewed_through(&scope).unwrap(), reviewed_after_ack);
}
