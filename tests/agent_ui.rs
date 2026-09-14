use agent_mux::agent::AgentDefinition;
use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use agent_mux::tracing::analysis::{
    Briefing, Evidence, EvidenceSource, RuntimeState, SessionCard, TaskOutcome,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use std::time::Instant;
use tempfile::TempDir;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_shell_profile(name: &str) -> Profile {
    #[cfg(windows)]
    let command = "cmd.exe";
    #[cfg(not(windows))]
    let command = "sh";

    Profile {
        name: name.into(),
        command: command.into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

#[test]
fn runtime_has_no_agent_named_module() {
    let lib = std::fs::read_to_string("src/lib.rs").unwrap();
    assert!(!lib.contains("pub mod heimdall"));
    assert!(!std::path::Path::new("src/heimdall.rs").exists());
}

#[tokio::test]
async fn test_agents_sidebar_navigation_and_launcher() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);
    app.set_pane_size(24, 80);

    // Initial state is Active
    assert_eq!(app.sidebar_section, SidebarSection::Active);

    // Tab moves from Active to Agents
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    // Pressing Enter on Agents opens AgentLauncher mode
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::AgentLauncher(_)));

    // In launcher, default harness is Antigravity (index 2 for Heimdall from AGENTS.md)
    if let Mode::AgentLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 2);
        assert_eq!(state.selected_harness(), Harness::Antigravity);
    }

    // Pressing '1' jumps to Claude (index 0)
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    if let Mode::AgentLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_harness(), Harness::Claude);
    }

    // Down moves to Codex (index 1)
    app.handle_key(&key(KeyCode::Down), Instant::now());
    if let Mode::AgentLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_harness(), Harness::Codex);
    }

    // Pressing '3' jumps to Antigravity (index 2)
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    if let Mode::AgentLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 2);
        assert_eq!(state.selected_harness(), Harness::Antigravity);
    }

    // Pressing Esc cancels back to Control mode
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    // Mouse click in Agents area selects Agents section
    app.sidebar_section = SidebarSection::Active;
    let (_, agents_rect, _) = agent_mux::ui::sidebar_areas(app.pane_size.0 + 3, app.agents.len());
    let click_agents = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: agents_rect.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click_agents, Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);

    app.kill_all();
}

#[tokio::test]
async fn test_dynamic_agents_discovery_and_selection() {
    let temp_dir = TempDir::new().unwrap();
    let agents_dir = temp_dir.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Create a custom agent: architect.md
    let architect_md = r#"---
id: architect
name: Architect
icon: 🏗️
description: Designs scalable systems and modular architectures.
harnesses: [claude, codex]
---
You are the System Architect.
Focus on high-level architecture, module decomposition, and API contracts.
"#;
    std::fs::write(agents_dir.join("architect.md"), architect_md).unwrap();

    // Load agents specifying agents_dir
    let agents = agent_mux::agent::load_agents(Some(&agents_dir));

    let architect = agents
        .iter()
        .find(|a| a.id == "architect")
        .expect("architect loaded");
    assert_eq!(architect.name, "Architect");
    assert_eq!(architect.icon.as_deref(), Some("🏗️"));
    assert_eq!(architect.harnesses.len(), 2);
    assert_eq!(architect.harnesses[0], Harness::Claude);
    assert_eq!(architect.harnesses[1], Harness::Codex);
    assert!(architect.instructions.contains("System Architect"));

    // Initialize App and test switching between agents
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);
    app.set_pane_size(24, 80);
    app.agents = agents;
    app.selected_agent = 0;

    // Focus Agents sidebar
    app.sidebar_section = SidebarSection::Agents;

    // If more than 1 agent, test navigation
    if app.agents.len() > 1 {
        app.handle_key(&key(KeyCode::Down), Instant::now());
        assert_eq!(app.selected_agent, 1);

        app.handle_key(&key(KeyCode::Up), Instant::now());
        assert_eq!(app.selected_agent, 0);
    }

    // Enter opens launcher for the selected agent
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    if let Mode::AgentLauncher(ref state) = app.mode {
        assert_eq!(state.agent_id, app.agents[0].id);
        assert_eq!(state.agent_name, app.agents[0].name);
    }

    app.kill_all();
}

#[tokio::test]
async fn test_stale_errors_preserve_cached_facts() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);

    assert!(app.cached_briefing.is_none());
    assert!(app.cached_briefing_warning.is_none());

    let initial_briefing = Briefing {
        cards: vec![SessionCard {
            session_key: Some("claude:s1".into()),
            launch_id: Some("launch-1".into()),
            provider: Some("claude".into()),
            cwd: PathBuf::from("/workspace"),
            runtime_state: RuntimeState::Working,
            task_outcome: TaskOutcome::Unknown,
            initial_goal: Evidence::observed(
                "build parser".into(),
                EvidenceSource::Transcript,
                None,
            ),
            current_activity: Evidence::observed(
                "cargo test".into(),
                EvidenceSource::Transcript,
                None,
            ),
            completed_turns: 3,
            open_turns: 0,
            total_tools: 5,
            tool_counts: vec![],
            files_modified: vec![Evidence::observed(
                "src/lib.rs".into(),
                EvidenceSource::Transcript,
                None,
            )],
            recent_commands: vec![Evidence::observed(
                "cargo check".into(),
                EvidenceSource::Transcript,
                None,
            )],
            last_assistant_output: Some(Evidence::observed(
                "Tests passed".into(),
                EvidenceSource::Transcript,
                None,
            )),
            duration_ms: 12000,
            last_active_ns: 1000,
            total_tokens: Some(42000),
            total_cost_usd: Some(0.12),
            active_tools: vec![],
        }],
        scope_workspace: PathBuf::from("/workspace"),
        since_ns: 0,
        until_ns: 1000,
        total_sessions: 1,
        total_turns: 3,
        total_tools: 5,
        total_tokens: Some(42000),
        total_cost_usd: Some(0.12),
        warnings: vec![],
    };

    // Revision 1 succeeds
    app.briefing_revision = 1;
    app.handle_analysis_updated(1, Ok(initial_briefing.clone()));
    assert_eq!(app.cached_briefing, Some(initial_briefing.clone()));
    assert!(app.cached_briefing_as_of.is_some());
    assert!(app.cached_briefing_warning.is_none());

    // Revision 2 fails with error: cached briefing MUST NOT be cleared
    app.briefing_revision = 2;
    app.handle_analysis_updated(2, Err("database locked".into()));
    assert_eq!(app.cached_briefing, Some(initial_briefing));
    assert_eq!(
        app.cached_briefing_warning.as_deref(),
        Some("database locked")
    );

    app.kill_all();
}

#[tokio::test]
async fn test_rendering_never_opens_sqlite() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);
    app.set_pane_size(24, 80);

    // Set invalid trace db path so any attempt to open it synchronously will fail
    app.trace_db_path = Some(PathBuf::from("/nonexistent/path/traces.db"));

    // Add an agent with trace.read capability
    let mut trace_agent = AgentDefinition::default();
    trace_agent.id = "audit".into();
    trace_agent.name = "Audit Agent".into();
    trace_agent.capabilities = vec!["trace.read".into()];
    app.agents = vec![trace_agent];
    app.selected_agent = 0;
    app.sidebar_section = SidebarSection::Agents;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    // Drawing must succeed without opening SQLite or panicking
    terminal
        .draw(|f| {
            agent_mux::ui::draw(f, &app, Instant::now());
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
    assert!(content.contains("Audit Agent"));
    assert!(content.contains("Executive Briefing"));

    app.kill_all();
}

#[tokio::test]
async fn test_capability_based_preview_selection() {
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);
    app.set_pane_size(24, 80);

    // 1. Agent with trace.read capability
    let mut audit_agent = AgentDefinition::default();
    audit_agent.id = "audit".into();
    audit_agent.name = "Audit Agent".into();
    audit_agent.capabilities = vec!["trace.read".into()];

    // 2. Agent without trace.read capability
    let mut reviewer_agent = AgentDefinition::default();
    reviewer_agent.id = "reviewer".into();
    reviewer_agent.name = "Code Reviewer".into();
    reviewer_agent.capabilities = vec![];

    app.agents = vec![audit_agent, reviewer_agent];
    app.sidebar_section = SidebarSection::Agents;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    // Select audit agent (index 0) -> renders trace briefing
    app.selected_agent = 0;
    terminal
        .draw(|f| {
            agent_mux::ui::draw(f, &app, Instant::now());
        })
        .unwrap();
    let content0: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(content0.contains("Executive Briefing"));

    // Select reviewer agent (index 1) -> renders generic definition card (not briefing)
    app.selected_agent = 1;
    terminal
        .draw(|f| {
            agent_mux::ui::draw(f, &app, Instant::now());
        })
        .unwrap();
    let content1: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(content1.contains("Autonomous Agent [reviewer]"));
    assert!(!content1.contains("Executive Briefing"));

    app.kill_all();
}
