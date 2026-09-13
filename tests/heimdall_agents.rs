use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::heimdall::{HeimdallHarness, generate_heimdall_prompt, query_heimdall_analysis};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

    // Pressing Enter on Agents opens HeimdallLauncher mode
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::HeimdallLauncher(_)));

    // In launcher, default harness is Claude (index 0)
    if let Mode::HeimdallLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_harness(), HeimdallHarness::Claude);
    }

    // Down moves to Codex (index 1)
    app.handle_key(&key(KeyCode::Down), Instant::now());
    if let Mode::HeimdallLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_harness(), HeimdallHarness::Codex);
    }

    // Pressing '3' jumps to Antigravity (index 2)
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    if let Mode::HeimdallLauncher(ref state) = app.mode {
        assert_eq!(state.selected, 2);
        assert_eq!(state.selected_harness(), HeimdallHarness::Antigravity);
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

#[test]
fn test_sqlite_heimdall_analysis_and_prompt_generation() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("traces.db");

    let store = agent_mux::tracing::store::open_rw(
        &db_path,
        agent_mux::tracing::store::OpenOptions {
            prices: agent_mux::tracing::pricing::PriceTable::builtin(),
            run_id: "run-1".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let conn = store.conn();

    let now_ns = 1_700_000_000_000_000_000i64;

    // Insert session and launch
    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns)
         VALUES ('claude:sess-1', 'claude', 'sess-1', ?1, ?1)",
        rusqlite::params![now_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, session_key, agent_mux_version, started_ns)
         VALUES ('launch-1', 'run-1', 1, 'Claude Code', 'claude', '/workspace', '-workspace', 'full', 'deterministic', 'claude:sess-1', 'test', ?1)",
        rusqlite::params![now_ns],
    )
    .unwrap();

    // Insert trace with skill
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, input, skills)
         VALUES ('trace-1', 'claude:sess-1', 'launch-1', 1, 'turn-1', 'closed', ?1, ?1 + 10000000000, 'implement feature', '[\"code-review\"]')",
        rusqlite::params![now_ns],
    )
    .unwrap();

    // Insert observations for skill: a generation and a tool
    conn.execute(
        "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, level, total_tokens, total_cost_usd, skill)
         VALUES ('obs-1', 'trace-1', 'generation', 'assistant', ?1, ?1 + 5000000000, 'DEFAULT', 120000, 0.45, 'code-review')",
        rusqlite::params![now_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, level, total_tokens, total_cost_usd, skill)
         VALUES ('obs-2', 'trace-1', 'tool', 'git_diff', ?1 + 5000000000, ?1 + 9500000000, 'DEFAULT', 5000, 0.02, 'code-review')",
        rusqlite::params![now_ns],
    )
    .unwrap();

    // Query analysis
    let analysis = query_heimdall_analysis(&db_path, &[], Instant::now());
    assert_eq!(analysis.total_sessions, 1);
    assert_eq!(analysis.total_traces, 1);
    assert_eq!(analysis.total_observations, 2);

    // Verify skill analysis
    assert_eq!(analysis.skills.len(), 1);
    let skill = &analysis.skills[0];
    assert_eq!(skill.skill, "code-review");
    assert_eq!(skill.turns_loaded, 1);
    assert_eq!(skill.generations, 1);
    assert_eq!(skill.tools, 1);
    assert_eq!(skill.tokens, Some(125000));
    assert_eq!(skill.slowest_tool_name.as_deref(), Some("git_diff"));
    assert_eq!(skill.slowest_tool_latency_ms, Some(4500));

    // Verify prompt generation
    let prompt = generate_heimdall_prompt(&analysis, &db_path);
    assert!(prompt.contains("Heimdall"));
    assert!(prompt.contains("code-review"));
    assert!(prompt.contains("git_diff"));
    assert!(prompt.contains("125000 tokens"));
    assert!(prompt.contains("sqlite3"));
}

#[tokio::test]
async fn test_dynamic_agents_discovery_and_selection() {
    let temp_dir = TempDir::new().unwrap();
    let agents_dir = temp_dir.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Create a custom agent: architect.md
    let architect_md = r#"---
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

    // Both Heimdall (seeded) and Architect should be present
    assert!(agents.iter().any(|a| a.id == "heimdall"));
    let architect = agents.iter().find(|a| a.id == "architect").expect("architect loaded");
    assert_eq!(architect.name, "Architect");
    assert_eq!(architect.icon.as_deref(), Some("🏗️"));
    assert_eq!(architect.harnesses.len(), 2);
    assert_eq!(architect.harnesses[0], HeimdallHarness::Claude);
    assert_eq!(architect.harnesses[1], HeimdallHarness::Codex);
    assert!(architect.instructions.contains("System Architect"));

    // Verify seeding of default heimdall.md in agents_dir
    assert!(agents_dir.join("heimdall.md").exists());

    // Initialize App and test switching between agents
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![make_shell_profile("test-session")], None, tx);
    app.set_pane_size(24, 80);
    app.agents = agents;
    app.selected_agent = 0;

    // Focus Agents sidebar
    app.sidebar_section = SidebarSection::Agents;

    // Down moves to next agent
    if app.agents.len() > 1 {
        app.handle_key(&key(KeyCode::Down), Instant::now());
        assert_eq!(app.selected_agent, 1);

        // Up moves back
        app.handle_key(&key(KeyCode::Up), Instant::now());
        assert_eq!(app.selected_agent, 0);
    }

    // Enter opens launcher for the selected agent
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    if let Mode::HeimdallLauncher(ref state) = app.mode {
        assert_eq!(state.agent_id, app.agents[0].id);
        assert_eq!(state.agent_name, app.agents[0].name);
    }

    app.kill_all();
}

