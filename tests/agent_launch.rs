use agent_mux::agent::artifacts::render_artifacts;
use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::launch::{
    build_agent_launch, selected_harness_index, LaunchError, LaunchOptions,
};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use std::path::{Path, PathBuf};

fn make_test_profile(harness: Harness) -> Profile {
    Profile {
        name: format!("Test {}", harness.as_str()),
        command: harness.as_str().to_string(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

#[test]
fn picker_honors_source_default() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\ndefault_harness: agy\n---\nReview changes.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    assert_eq!(selected_harness_index(&d), 2);
}

#[test]
fn build_agent_launch_claude_composes_argv() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\ndefault_harness: claude\nstartup_task: Check the repo\n---\nAudit instructions.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Claude);
    let options = LaunchOptions {
        harness_override: Some(Harness::Claude),
        model: Some("claude-3-7-sonnet".to_string()),
        bypass_approvals: Some(true),
        ..Default::default()
    };

    let launch = build_agent_launch(
        &d,
        &profile,
        &options,
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert_eq!(launch.cwd, PathBuf::from("/workspace"));
    assert!(launch.profile.args.contains(&"--append-system-prompt".to_string()));
    assert!(launch.profile.args.contains(&"Audit instructions.".to_string()));
    assert!(launch.profile.args.contains(&"--model".to_string()));
    assert!(launch.profile.args.contains(&"claude-3-7-sonnet".to_string()));
    assert!(launch.profile.args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(launch.profile.args.contains(&"Check the repo".to_string()));
}

#[test]
fn build_agent_launch_codex_composes_argv() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex]\ndefault_harness: codex\nstartup_task: Run audit\n---\nReview.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Codex);
    let options = LaunchOptions {
        harness_override: Some(Harness::Codex),
        bypass_approvals: Some(true),
        ..Default::default()
    };

    let launch = build_agent_launch(
        &d,
        &profile,
        &options,
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert!(launch.profile.args.contains(&"--yolo".to_string()));
    assert!(launch.profile.args.contains(&"Run audit".to_string()));
}

#[test]
fn build_agent_launch_agy_composes_argv() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [agy]\ndefault_harness: agy\nstartup_task: Investigate\n---\nAGY instructions.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Antigravity);
    let options = LaunchOptions {
        harness_override: Some(Harness::Antigravity),
        bypass_approvals: Some(true),
        ..Default::default()
    };

    let launch = build_agent_launch(
        &d,
        &profile,
        &options,
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert!(launch.profile.args.contains(&"--prompt-interactive".to_string()));
    assert!(launch.profile.args.contains(&"Investigate".to_string()));
    assert!(launch.profile.args.contains(&"--dangerously-skip-permissions".to_string()));
}

#[test]
fn unsupported_harness_rejected() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude]\ndefault_harness: claude\n---\nClaude only.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Codex);
    let options = LaunchOptions {
        harness_override: Some(Harness::Codex),
        ..Default::default()
    };

    let res = build_agent_launch(
        &d,
        &profile,
        &options,
        Path::new("/workspace"),
        &artifacts,
    );
    assert!(matches!(res, Err(LaunchError::UnsupportedHarness(_))));
}

#[test]
fn spaces_and_quotes_preserved_without_shell_splitting() {
    let d = parse_definition(
        "---\nid: complex\nharnesses: [claude]\nstartup_task: echo \"hello world\" && ls -la\n---\nInstructions with 'single' and \"double\" quotes.",
        Path::new("complex/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("complex/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Claude);
    let options = LaunchOptions::default();

    let launch = build_agent_launch(
        &d,
        &profile,
        &options,
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();

    assert!(launch.profile.args.contains(&"echo \"hello world\" && ls -la".to_string()));
    assert!(launch.profile.args.contains(&"Instructions with 'single' and \"double\" quotes.".to_string()));
}
