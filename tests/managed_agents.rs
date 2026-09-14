use agent_mux::agent::budgets::{Budget, ManagedCapabilities, validate_budget};
use agent_mux::agent::managed::{
    ManagedAdapter, ManagedEvent, ManagedRequest, ScriptedManagedAdapter,
};

#[test]
fn unsupported_enforcement_rejects_enablement() {
    let caps = ManagedCapabilities {
        cancel: true,
        max_turns: false,
        token_budget: false,
        cost_budget: false,
        resume: true,
    };
    let budget = Budget {
        max_turns: 3,
        timeout_seconds: 120,
        max_tokens: None,
        max_cost_usd: None,
    };
    assert!(validate_budget(&caps, &budget).is_err());
}

#[tokio::test]
async fn scripted_adapter_yields_structured_events_and_completes() {
    let adapter = ScriptedManagedAdapter::new(vec![
        ManagedEvent::Started,
        ManagedEvent::TurnStarted { turn: 1 },
        ManagedEvent::Evidence {
            id: "ev_1".to_string(),
        },
        ManagedEvent::Usage {
            tokens: 150,
            cost_usd: Some(0.002),
        },
        ManagedEvent::Completed {
            output: "Investigation finished cleanly.".to_string(),
        },
    ]);

    let req = ManagedRequest {
        agent_id: "audit".to_string(),
        prompt: "Check recent errors".to_string(),
        source_hash: "abc".to_string(),
        budget: Budget::default(),
        native_session_id: None,
    };

    let mut session = adapter.start(req).await.unwrap();

    let mut events = Vec::new();
    while let Some(event) = session.next_event().await.unwrap() {
        events.push(event);
    }

    assert_eq!(events.len(), 5);
    assert!(matches!(events[0], ManagedEvent::Started));
    assert!(matches!(events[4], ManagedEvent::Completed { .. }));
}

#[tokio::test]
async fn session_cancellation_marks_session_cancelled() {
    let adapter = ScriptedManagedAdapter::new(vec![
        ManagedEvent::Started,
        ManagedEvent::TurnStarted { turn: 1 },
    ]);

    let req = ManagedRequest {
        agent_id: "audit".to_string(),
        prompt: "Run check".to_string(),
        source_hash: "abc".to_string(),
        budget: Budget::default(),
        native_session_id: None,
    };

    let mut session = adapter.start(req).await.unwrap();
    let first = session.next_event().await.unwrap();
    assert!(matches!(first, Some(ManagedEvent::Started)));

    session.cancel().await.unwrap();
    assert!(session.is_cancelled());
}
