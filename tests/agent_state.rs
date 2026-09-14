use agent_mux::agent::state::{AgentScope, JobStatus, StateError, StateStore};

#[test]
fn generating_a_briefing_does_not_acknowledge_it() {
    let root = tempfile::tempdir().unwrap();
    let mut state = StateStore::open(&root.path().join("agent-state.db")).unwrap();
    let scope = AgentScope {
        agent_id: "audit".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };
    let id = state
        .record_briefing(&scope, 42, "Observed progress", &[])
        .unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 0);
    state.acknowledge(&scope, &id).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 42);
}

#[test]
fn wrong_scope_acknowledgment_fails() {
    let root = tempfile::tempdir().unwrap();
    let mut state = StateStore::open(&root.path().join("agent-state.db")).unwrap();
    let scope1 = AgentScope {
        agent_id: "audit".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };
    let scope2 = AgentScope {
        agent_id: "other".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };
    let id = state
        .record_briefing(&scope1, 10, "Audit report", &[])
        .unwrap();
    let err = state.acknowledge(&scope2, &id).unwrap_err();
    match err {
        StateError::ScopeConflict { .. } => (),
        other => panic!("expected ScopeConflict, got {other:?}"),
    }
}

#[test]
fn old_acknowledgments_do_not_regress_cursor() {
    let root = tempfile::tempdir().unwrap();
    let mut state = StateStore::open(&root.path().join("agent-state.db")).unwrap();
    let scope = AgentScope {
        agent_id: "audit".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };
    let id1 = state
        .record_briefing(&scope, 50, "Briefing 1", &[])
        .unwrap();
    let id2 = state
        .record_briefing(&scope, 25, "Briefing 2", &[])
        .unwrap();

    state.acknowledge(&scope, &id1).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 50);

    state.acknowledge(&scope, &id2).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 50);
}

#[test]
fn crash_restart_transitions_running_jobs_to_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("agent-state.db");
    let scope = AgentScope {
        agent_id: "audit".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };

    {
        let mut state = StateStore::open(&db_path).unwrap();
        let job_id = state
            .create_job(&scope, "dedupe-1", 1, JobStatus::Running)
            .unwrap();
        assert_eq!(state.get_job_status(&job_id).unwrap(), JobStatus::Running);
    }

    // Reopening simulates startup after restart
    {
        let state = StateStore::open(&db_path).unwrap();
        assert_eq!(
            state.get_job_status("dedupe-1").unwrap(),
            JobStatus::Interrupted
        );
    }
}

#[test]
fn instruction_edits_retain_review_history() {
    let root = tempfile::tempdir().unwrap();
    let mut state = StateStore::open(&root.path().join("agent-state.db")).unwrap();
    let scope = AgentScope {
        agent_id: "audit".into(),
        source_path: "/agents/audit/AGENTS.md".into(),
        workspace: "/work".into(),
    };

    let b1 = state.record_briefing(&scope, 100, "Rev 1", &[]).unwrap();
    state.acknowledge(&scope, &b1).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 100);

    // Recording and acknowledging with same scope retains history
    let b2 = state.record_briefing(&scope, 120, "Rev 2", &[]).unwrap();
    state.acknowledge(&scope, &b2).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 120);
}
