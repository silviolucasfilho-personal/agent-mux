use agent_mux::agent::budgets::Budget;
use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::triggers::{EventKind, should_trigger};
use std::path::Path;

#[test]
fn watcher_does_not_trigger_on_its_descendants() {
    assert!(!should_trigger("audit", Some("audit"), &[]));
    assert!(!should_trigger("audit", Some("helper"), &["audit".into()]));
    assert!(should_trigger("audit", Some("builder"), &[]));
}

#[test]
fn default_monitoring_config_has_automatic_disabled() {
    let source = "---\nid: audit\nharnesses: [codex]\n---\nInspect progress.";
    let d = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert!(!d.monitoring.automatic);
    assert_eq!(d.monitoring.max_jobs_per_hour, 3);
    assert_eq!(d.monitoring.max_concurrent_jobs, 1);
    assert_eq!(d.monitoring.timeout_seconds, 120);
    assert_eq!(d.monitoring.max_turns, 3);
}

#[test]
fn triggers_and_monitoring_parsed_from_frontmatter() {
    let source = r#"---
id: audit
harnesses: [codex]
triggers:
  - event: repeated_error
    prompt: "Inspect the repeated error evidence."
    debounce_ms: 5000
  - event: process_exit
    prompt: "Summarize session on exit."
monitoring:
  automatic: true
  max_jobs_per_hour: 5
  max_concurrent_jobs: 2
  timeout_seconds: 60
  max_turns: 4
---
Inspect progress.
"#;
    let d = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(d.triggers.len(), 2);
    assert_eq!(d.triggers[0].event, EventKind::RepeatedError);
    assert_eq!(d.triggers[0].debounce_ms, 5000);
    assert_eq!(d.triggers[1].event, EventKind::ProcessExit);
    assert_eq!(d.triggers[1].debounce_ms, 5000); // default debounce
    assert!(d.monitoring.automatic);
    assert_eq!(d.monitoring.max_jobs_per_hour, 5);
    assert_eq!(d.monitoring.max_concurrent_jobs, 2);
    assert_eq!(d.monitoring.timeout_seconds, 60);
    assert_eq!(d.monitoring.max_turns, 4);
}

#[test]
fn default_budget_is_bounded() {
    let budget = Budget::default();
    assert_eq!(budget.max_turns, 3);
    assert_eq!(budget.timeout_seconds, 120);
    assert_eq!(budget.max_cost_usd, None);
    assert_eq!(budget.max_tokens, None);
}
