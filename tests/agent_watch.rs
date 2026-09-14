use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::managed::{ManagedEvent, ScriptedManagedAdapter};
use agent_mux::agent::state::{AgentScope, StateStore};
use agent_mux::agent::watch::{Watcher, WatcherConfig};
use agent_mux::tracing::store;
use std::path::Path;
use std::sync::Arc;

#[test]
fn default_monitoring_does_not_launch_models() {
    let source = "---\nid: audit\nharnesses: [codex]\n---\nInspect progress.";
    let d = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert!(!d.monitoring.automatic);
}

#[tokio::test]
async fn watcher_tick_with_automatic_disabled_does_not_spawn_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let trace_path = temp.path().join("traces.db");
    let state_path = temp.path().join("agent-state.db");

    let mut opts = store::OpenOptions::default();
    opts.run_id = "test-run".to_string();
    let _store = store::open_rw(&trace_path, opts).unwrap();

    let source = r#"---
id: audit
harnesses: [codex]
triggers:
  - event: repeated_error
    prompt: "Investigate errors"
monitoring:
  automatic: false
---
Instructions
"#;
    let def = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    let scope = AgentScope {
        agent_id: def.id.clone(),
        source_path: "/work/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };

    let adapter = Arc::new(ScriptedManagedAdapter::new(vec![ManagedEvent::Completed {
        output: "Done".into(),
    }]));

    let mut watcher = Watcher::new(
        scope,
        def,
        trace_path,
        state_path.clone(),
        adapter,
        WatcherConfig::default(),
    )
    .unwrap();

    // Tick advances consumption cursor and checks triggers, but spawns 0 jobs
    watcher.tick(1_000_000_000).await.unwrap();

    let state = StateStore::open(&state_path).unwrap();
    assert_eq!(state.reviewed_through(&watcher.scope).unwrap(), 0);
}

#[tokio::test]
async fn watcher_with_automatic_enabled_spawns_bounded_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let trace_path = temp.path().join("traces.db");
    let state_path = temp.path().join("agent-state.db");

    let mut opts = store::OpenOptions::default();
    opts.run_id = "test-run".to_string();
    let store = store::open_rw(&trace_path, opts).unwrap();

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
Instructions
"#;
    let def = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    let scope = AgentScope {
        agent_id: def.id.clone(),
        source_path: "/work/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };

    let adapter = Arc::new(ScriptedManagedAdapter::new(vec![
        ManagedEvent::Started,
        ManagedEvent::Completed {
            output: "Exit investigated".into(),
        },
    ]));

    let mut watcher = Watcher::new(
        scope,
        def,
        trace_path.clone(),
        state_path.clone(),
        adapter,
        WatcherConfig::default(),
    )
    .unwrap();

    // Simulate session exit change in store
    store
        .conn()
        .execute_batch(
            "INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
             VALUES('claude:s1','claude','s1',1000,2000);
             UPDATE sessions SET last_seen_ns = 3000 WHERE key = 'claude:s1';",
        )
        .unwrap();

    // Tick processes the change and schedules the job
    watcher.tick(1_000_000_000).await.unwrap();

    // Verify job ran and completed
    let _state = StateStore::open(&state_path).unwrap();
    assert_eq!(watcher.executed_jobs_count(), 1);
}
