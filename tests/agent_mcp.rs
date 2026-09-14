use agent_mux::agent::artifacts::RenderContext;
use agent_mux::agent::artifacts::mcp_command;
use agent_mux::agent::artifacts::render_artifacts_with_context;
use agent_mux::agent::definition::parse_definition;
use std::path::Path;

#[test]
fn mcp_command_keeps_paths_as_arguments() {
    let (command, args) = mcp_command(
        Path::new("/tmp/bin/agent-mux"),
        Path::new("/tmp/a b/traces.db"),
        Path::new("/tmp/a b"),
    );
    assert_eq!(command, Path::new("/tmp/bin/agent-mux"));
    assert_eq!(args[4], "/tmp/a b/traces.db");
    assert_eq!(args.len(), 7);
    assert_eq!(args[0], "mcp");
    assert_eq!(args[1], "serve");
    assert_eq!(args[2], "--stdio");
    assert_eq!(args[3], "--db");
    assert_eq!(args[5], "--workspace");
    assert_eq!(args[6], "/tmp/a b");
}

#[test]
fn render_artifacts_emits_mcp_server_entry() {
    let d = parse_definition(
        "---\nid: heimdall\nharnesses: [claude, codex, agy]\nmcp_servers: [agent-mux]\n---\nWatcher.",
        Path::new("heimdall/AGENTS.md"),
    )
    .unwrap();

    let ctx = RenderContext {
        executable: Path::new("/usr/local/bin/agent-mux").to_path_buf(),
        db: Path::new("/var/data/traces.db").to_path_buf(),
        workspace: Path::new("/workspace/project").to_path_buf(),
    };

    let set = render_artifacts_with_context(&d, Path::new("heimdall/AGENTS.md"), &ctx).unwrap();

    // Claude mcp.json
    let claude_bytes = set.files.get(Path::new("claude/mcp.json")).unwrap();
    let claude_val: serde_json::Value = serde_json::from_slice(claude_bytes).unwrap();
    let claude_server = &claude_val["mcpServers"]["agent-mux"];
    assert_eq!(claude_server["command"], "/usr/local/bin/agent-mux");
    let claude_args = claude_server["args"].as_array().unwrap();
    assert_eq!(claude_args[4], "/var/data/traces.db");
    assert_eq!(claude_args[6], "/workspace/project");

    // Codex mcp.toml
    let codex_bytes = set.files.get(Path::new("codex/mcp.toml")).unwrap();
    let codex_toml = std::str::from_utf8(codex_bytes).unwrap();
    assert!(codex_toml.contains("[mcp_servers.agent-mux]"));
    assert!(codex_toml.contains("command = \"/usr/local/bin/agent-mux\""));
    assert!(codex_toml.contains("\"/var/data/traces.db\""));
    assert!(codex_toml.contains("\"/workspace/project\""));

    // AGY mcp.json
    let agy_bytes = set.files.get(Path::new("agy/mcp.json")).unwrap();
    let agy_val: serde_json::Value = serde_json::from_slice(agy_bytes).unwrap();
    let agy_server = &agy_val["mcpServers"]["agent-mux"];
    assert_eq!(agy_server["command"], "/usr/local/bin/agent-mux");
    let agy_args = agy_server["args"].as_array().unwrap();
    assert_eq!(agy_args[4], "/var/data/traces.db");
    assert_eq!(agy_args[6], "/workspace/project");
}

#[test]
fn render_artifacts_emits_empty_fragment_when_no_mcp_servers() {
    let d = parse_definition(
        "---\nid: clean\nharnesses: [claude, codex, agy]\n---\nNo tools.",
        Path::new("clean/AGENTS.md"),
    )
    .unwrap();

    let ctx = RenderContext {
        executable: Path::new("/usr/local/bin/agent-mux").to_path_buf(),
        db: Path::new("/var/data/traces.db").to_path_buf(),
        workspace: Path::new("/workspace/project").to_path_buf(),
    };

    let set = render_artifacts_with_context(&d, Path::new("clean/AGENTS.md"), &ctx).unwrap();

    let claude_bytes = set.files.get(Path::new("claude/mcp.json")).unwrap();
    let claude_val: serde_json::Value = serde_json::from_slice(claude_bytes).unwrap();
    assert!(claude_val["mcpServers"].as_object().unwrap().is_empty());

    let codex_bytes = set.files.get(Path::new("codex/mcp.toml")).unwrap();
    let codex_toml = std::str::from_utf8(codex_bytes).unwrap();
    assert!(!codex_toml.contains("agent-mux"));

    let agy_bytes = set.files.get(Path::new("agy/mcp.json")).unwrap();
    let agy_val: serde_json::Value = serde_json::from_slice(agy_bytes).unwrap();
    assert!(agy_val["mcpServers"].as_object().unwrap().is_empty());
}

#[test]
fn doctor_agent_checks_mcp_health() {
    use agent_mux::agent::artifacts::write_artifacts;
    use agent_mux::agent::install::{DoctorStatus, doctor_agent};
    use tempfile::tempdir;

    let root = tempdir().unwrap();
    let agent_dir = root.path().join("heimdall");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let agents_md = agent_dir.join("AGENTS.md");
    std::fs::write(
        &agents_md,
        "---\nid: heimdall\nharnesses: [claude, codex, agy]\nmcp_servers: [agent-mux]\n---\nWatcher.",
    )
    .unwrap();

    let d = parse_definition(&std::fs::read_to_string(&agents_md).unwrap(), &agents_md).unwrap();
    let ctx = RenderContext {
        executable: std::path::PathBuf::from("/usr/local/bin/agent-mux"),
        db: root.path().join("traces.db"),
        workspace: root.path().to_path_buf(),
    };
    let artifacts = render_artifacts_with_context(&d, &agents_md, &ctx).unwrap();
    let gen_dir = agent_dir.join("generated");
    write_artifacts(&gen_dir, &artifacts).unwrap();

    let report = doctor_agent(&d, &agent_dir).unwrap();
    assert_eq!(report.status, DoctorStatus::Healthy);
    assert!(report.issues.is_empty());
}

#[test]
fn trace_briefing_cli_routes_to_service() {
    use agent_mux::tracing::analysis::service::{BriefingArgs, Request, TraceService};
    use rusqlite::Connection;
    use tempfile::tempdir;

    let root = tempdir().unwrap();
    let db_path = root.path().join("traces.db");
    let ws_path = root.path().join("my_project");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws_path = ws_path.canonicalize().unwrap();

    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE traces (
            id TEXT PRIMARY KEY,
            session_key TEXT,
            launch_id TEXT,
            provider TEXT,
            cwd TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL
        );
        CREATE TABLE sessions (
            key TEXT PRIMARY KEY,
            provider TEXT,
            cwd TEXT,
            first_seen_ns INTEGER,
            last_seen_ns INTEGER
        );
        CREATE TABLE observations (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            name TEXT,
            type TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            is_error INTEGER,
            status_message TEXT,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL,
            skill TEXT
        );",
    )
    .unwrap();

    let now_ns = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 10_000_000_000;
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "s1",
            "claude",
            ws_path.to_string_lossy().to_string(),
            now_ns,
            now_ns + 1000
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            "t1",
            "s1",
            "l1",
            "claude",
            ws_path.to_string_lossy().to_string(),
            now_ns,
            now_ns + 1000,
            "fix the bug",
            "fixed",
            150,
            0.005
        ],
    )
    .unwrap();

    let service = TraceService::new(agent_mux::tracing::analysis::ServiceConfig::new(
        db_path.clone(),
        agent_mux::tracing::analysis::Scope::workspace(&ws_path).unwrap(),
    ))
    .unwrap();

    let service_res = service
        .execute(Request::Briefing(BriefingArgs::default()))
        .unwrap();

    assert_eq!(service_res.schema_version, 1);
    let briefing_data: agent_mux::tracing::analysis::BriefingData =
        serde_json::from_value(service_res.data).unwrap();
    assert_eq!(briefing_data.total_sessions, 1);

    // Run via CLI
    let args = vec![
        "briefing".to_string(),
        "--db".to_string(),
        db_path.to_string_lossy().to_string(),
        "--workspace".to_string(),
        ws_path.to_string_lossy().to_string(),
        "--json".to_string(),
    ];
    let cli_res = agent_mux::tracing::cli::run(&args);
    assert!(cli_res.is_ok());
}
