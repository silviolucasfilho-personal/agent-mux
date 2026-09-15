use agent_mux::agent::artifacts::render_artifacts;
use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::launch::{
    LaunchError, LaunchOptions, build_agent_launch, selected_harness_index,
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

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert_eq!(launch.cwd, PathBuf::from("/workspace"));
    assert!(
        launch
            .profile
            .args
            .contains(&"--append-system-prompt".to_string())
    );
    assert!(
        launch
            .profile
            .args
            .contains(&"Audit instructions.".to_string())
    );
    assert!(launch.profile.args.contains(&"--model".to_string()));
    assert!(
        launch
            .profile
            .args
            .contains(&"claude-3-7-sonnet".to_string())
    );
    assert!(
        launch
            .profile
            .args
            .contains(&"--dangerously-skip-permissions".to_string())
    );
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

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert!(launch.profile.args.contains(&"--yolo".to_string()));
    assert!(launch.profile.args.contains(&"Run audit".to_string()));
    assert!(launch.profile.args.contains(&"-c".to_string()));
    assert!(
        launch
            .profile
            .args
            .contains(&"developer_instructions=\"Review.\"".to_string())
    );
}

#[test]
fn build_agent_launch_codex_configures_mcp_servers() {
    let d = parse_definition(
        "---\nid: heimdall\nharnesses: [codex]\nmcp_servers: [agent-mux]\nstartup_task: Briefing\n---\nHeimdall instructions.",
        Path::new("heimdall/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("heimdall/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Codex);
    let options = LaunchOptions::default();

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert!(
        launch
            .profile
            .args
            .iter()
            .any(|a| a.starts_with("mcp_servers.agent-mux.command="))
    );
    assert!(
        launch
            .profile
            .args
            .iter()
            .any(|a| a.starts_with("mcp_servers.agent-mux.args="))
    );
    assert!(launch.profile.args.contains(&"Briefing".to_string()));
}

#[test]
fn build_agent_launch_claude_configures_mcp_servers() {
    let d = parse_definition(
        "---\nid: heimdall\nharnesses: [claude]\nmcp_servers: [agent-mux]\nstartup_task: Briefing\n---\nHeimdall instructions.",
        Path::new("heimdall/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("heimdall/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Claude);
    let options = LaunchOptions::default();

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert!(launch.profile.args.contains(&"--mcp-config".to_string()));
    assert!(
        launch
            .profile
            .args
            .iter()
            .any(|a| a.contains("mcpServers") && a.contains("agent-mux"))
    );
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

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert_eq!(launch.agent_id, "audit");
    assert!(
        launch
            .profile
            .args
            .contains(&"--prompt-interactive".to_string())
    );
    assert!(launch.profile.args.contains(&"Investigate".to_string()));
    assert!(
        launch
            .profile
            .args
            .contains(&"--dangerously-skip-permissions".to_string())
    );
    // Instructions reach agy as a named custom agent, materialized on
    // prepare rather than at build time.
    let agent_at = launch
        .profile
        .args
        .iter()
        .position(|a| a == "--agent")
        .unwrap();
    assert_eq!(launch.profile.args[agent_at + 1], "Audit");
    assert_eq!(launch.files.len(), 1);
    let (path, bytes) = &launch.files[0];
    assert!(
        path.ends_with("agents/audit/agent.md"),
        "{}",
        path.display()
    );
    let md = std::str::from_utf8(bytes).unwrap();
    assert!(md.contains("name: Audit\n"));
    assert!(md.contains("mainAgent: true\n"));
    assert!(md.contains("# System Prompt\n\nAGY instructions."));
    assert!(launch.setup_commands.is_empty());
}

#[test]
fn build_agent_launch_agy_registers_mcp_via_agy_cli() {
    let d = parse_definition(
        "---\nid: heimdall\nharnesses: [agy]\nmcp_servers: [agent-mux]\n---\nHeimdall.",
        Path::new("heimdall/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("heimdall/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Antigravity);
    let launch = build_agent_launch(
        &d,
        &profile,
        &LaunchOptions::default(),
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();

    assert_eq!(launch.setup_commands.len(), 1);
    let cmd = &launch.setup_commands[0];
    assert_eq!(&cmd[..4], &["agy", "mcp", "add", "agent-mux"]);
    let sep = cmd.iter().position(|a| a == "--").unwrap();
    assert_eq!(&cmd[sep + 1..sep + 4], &["mcp", "serve", "--stdio"]);
    assert!(cmd.contains(&"--all-workspaces".to_string()));
}

#[test]
fn prepare_launch_materializes_files_and_reports_failed_setup_commands() {
    use agent_mux::agent::launch::{AgentLaunch, prepare_launch};
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("deep/agents/x/agent.md");
    let launch = AgentLaunch {
        profile: make_test_profile(Harness::Antigravity),
        cwd: root.path().to_path_buf(),
        env: vec![],
        agent_id: "x".into(),
        source_hash: String::new(),
        diagnostics: vec![],
        files: vec![(target.clone(), b"hello".to_vec())],
        setup_commands: vec![vec!["definitely-not-a-real-command-xyz".into()]],
    };
    let warnings = prepare_launch(&launch).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("could not run"));
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

    let res = build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts);
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

    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();

    assert!(
        launch
            .profile
            .args
            .contains(&"echo \"hello world\" && ls -la".to_string())
    );
    assert!(
        launch
            .profile
            .args
            .contains(&"Instructions with 'single' and \"double\" quotes.".to_string())
    );
}

#[test]
fn launch_block_layers_between_override_and_profile() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude]\nlaunch:\n  claude:\n    args: [--verbose, --add-dir, /extra]\n    model: block-model\n    bypass_approvals: true\n    env:\n      AUDIT_LEVEL: strict\n---\nReview.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let artifacts = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let mut profile = make_test_profile(Harness::Claude);
    profile.model = Some("profile-model".to_string());
    profile.bypass_approvals = Some(false);

    // No explicit override: the block beats the profile.
    let launch = build_agent_launch(
        &d,
        &profile,
        &LaunchOptions::default(),
        Path::new("/workspace"),
        &artifacts,
    )
    .unwrap();
    let args = &launch.profile.args;
    let model_at = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[model_at + 1], "block-model");
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    let verbose_at = args.iter().position(|a| a == "--verbose").unwrap();
    assert_eq!(args[verbose_at + 1], "--add-dir");
    assert_eq!(args[verbose_at + 2], "/extra");
    assert!(
        launch
            .env
            .contains(&("AUDIT_LEVEL".to_string(), "strict".to_string()))
    );

    // An explicit override still wins over the block.
    let options = LaunchOptions {
        model: Some("override-model".to_string()),
        bypass_approvals: Some(false),
        ..Default::default()
    };
    let launch =
        build_agent_launch(&d, &profile, &options, Path::new("/workspace"), &artifacts).unwrap();
    let args = &launch.profile.args;
    let model_at = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[model_at + 1], "override-model");
    assert!(!args.contains(&"--dangerously-skip-permissions".to_string()));
}

/// Live check against the installed `agy` CLI: preparing a Heimdall launch
/// writes the custom agent and registers the MCP server, and agy then runs
/// with those instructions and can see the agent_mux tools. Touches
/// `~/.gemini/config` exactly the way a real launch does.
/// Run with `cargo test --test agent_launch -- --ignored agy_live`.
#[test]
#[ignore]
fn agy_live_launch_applies_instructions_and_mcp() {
    use agent_mux::agent::launch::prepare_launch;
    let exe = env!("CARGO_BIN_EXE_agent-mux");
    let heimdall = agent_mux::agent::builtin_agents()
        .into_iter()
        .find(|a| a.id == "heimdall")
        .unwrap();
    let artifacts = render_artifacts(&heimdall, Path::new("heimdall/AGENTS.md")).unwrap();
    let profile = make_test_profile(Harness::Antigravity);
    let cwd = std::env::current_dir().unwrap();
    // SAFETY: test-only; the launch builder reads this to locate the MCP server binary.
    unsafe { std::env::set_var("AGENT_MUX_BIN", exe) };
    let launch = build_agent_launch(
        &heimdall,
        &profile,
        &LaunchOptions::default(),
        &cwd,
        &artifacts,
    )
    .unwrap();
    let warnings = prepare_launch(&launch).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");

    let out = std::process::Command::new("agy")
        .args([
            "--agent",
            "Heimdall",
            "-p",
            "Reply with exactly two lines. Line 1: the word HEIMDALL if your system prompt says you are Heimdall, otherwise NO. Line 2: the names of every tool available to you whose name starts with agent_mux_, comma separated, or NONE.",
            "--print-timeout",
            "90s",
        ])
        .current_dir(&cwd)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    eprintln!("agy said:\n{text}");
    assert!(
        text.contains("HEIMDALL"),
        "instructions not applied: {text}"
    );
    assert!(
        text.contains("agent_mux_get_briefing") && text.contains("agent_mux_analyze_agents"),
        "MCP tools not visible: {text}"
    );
}
