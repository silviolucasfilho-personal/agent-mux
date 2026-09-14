use agent_mux::mcp::tools::tool_names;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use tempfile::tempdir;

#[test]
fn catalog_contains_exactly_eight_read_tools() {
    let names = tool_names();
    assert_eq!(names.len(), 8);
    assert!(names.contains(&"agent_mux_get_briefing"));
    assert!(names.contains(&"agent_mux_compare_runs"));
    assert!(
        !names
            .iter()
            .any(|n| n.contains("shell") || n.contains("write"))
    );
}

#[test]
fn mcp_stdio_full_lifecycle_and_protocol_contract() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("traces.db");
    let ws_path = temp.path().join("workspace");
    std::fs::create_dir_all(&ws_path).unwrap();

    let exe = env!("CARGO_BIN_EXE_agent-mux");
    let mut child = Command::new(exe)
        .arg("mcp")
        .arg("serve")
        .arg("--stdio")
        .arg("--db")
        .arg(&db_path)
        .arg("--workspace")
        .arg(&ws_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-mux mcp serve");

    let mut stdin = child.stdin.take().expect("failed to open stdin");
    let stdout = child.stdout.take().expect("failed to open stdout");
    let mut reader = BufReader::new(stdout);

    // Set up database schema
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
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
            );
            CREATE VIEW session_stats AS
            SELECT
                session_key,
                COUNT(id) AS turn_count,
                SUM(total_tokens) AS total_tokens,
                SUM(total_cost_usd) AS total_cost_usd,
                (SELECT COUNT(o.id) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.session_key = traces.session_key AND o.type = 'tool') AS total_tools
            FROM traces
            GROUP BY session_key;
            CREATE VIEW trace_stats AS
            SELECT
                id, session_key, launch_id, total_tokens, total_cost_usd, start_ns, end_ns
            FROM traces;
            CREATE VIEW skill_stats AS
            SELECT
                skill,
                COUNT(DISTINCT trace_id) AS turns_loaded,
                COUNT(id) AS tools,
                SUM(total_tokens) AS tokens,
                SUM(total_cost_usd) AS cost,
                0 AS turns_unused
            FROM observations
            WHERE skill IS NOT NULL
            GROUP BY skill;",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
             VALUES ('s-1', 'claude', ?1, 1000000000, 2000000000)",
            [ws_path.to_string_lossy().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
             VALUES ('t-1', 's-1', 'l-1', 'claude', ?1, 1000000000, 2000000000, 'hello', 'world', 100, 0.01)",
            [ws_path.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    // 1. Initialize
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0.0" }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        !line.contains("\x1b["),
        "stdout must never contain ANSI escape sequences"
    );
    let init_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "agent-mux");

    // Send initialized notification
    let initialized_notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&initialized_notif).unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();

    // 2. tools/list - validate against golden fixture
    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&list_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    assert!(
        !line.contains("\x1b["),
        "stdout must never contain ANSI escape sequences"
    );
    let list_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(list_resp["id"], 2);
    let tools = list_resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);

    let golden_json = include_str!("fixtures/mcp/golden_tools.json");
    let golden_tools: Vec<serde_json::Value> = serde_json::from_str(golden_json).unwrap();
    for g in golden_tools {
        let name = g["name"].as_str().unwrap();
        let found = tools.iter().find(|t| t["name"].as_str() == Some(name));
        assert!(found.is_some(), "Tool {name} must exist in tools/list");
        let t = found.unwrap();
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        assert_eq!(t["annotations"]["destructiveHint"], false);
        assert_eq!(t["description"], g["description"]);
        assert!(t["inputSchema"].is_object());
    }

    // 3. tools/call agent_mux_get_health
    let call_health_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_get_health",
            "arguments": {}
        }
    });
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&call_health_req).unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let health_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(health_resp["id"], 3);
    assert_eq!(health_resp["result"]["isError"], false);
    let structured = &health_resp["result"]["structuredContent"];
    assert_eq!(structured["schema_version"], 1);
    let content_text = health_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let text_val: serde_json::Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(
        &text_val, structured,
        "content text and structuredContent must match"
    );

    // 4. tools/call agent_mux_get_briefing
    let call_briefing = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_get_briefing",
            "arguments": {}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_briefing).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let briefing_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(briefing_resp["id"], 4);
    assert_eq!(briefing_resp["result"]["isError"], false);

    // 5. tools/call agent_mux_list_sessions
    let call_list = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_list_sessions",
            "arguments": {}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_list).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let list_sessions_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(list_sessions_resp["id"], 5);
    assert_eq!(list_sessions_resp["result"]["isError"], false);

    // 6. tools/call agent_mux_get_session
    let call_get_session = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_get_session",
            "arguments": {
                "session_key": "s-1"
            }
        }
    });
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&call_get_session).unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let get_session_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(get_session_resp["id"], 6);
    assert_eq!(get_session_resp["result"]["isError"], false);

    // 7. tools/call agent_mux_get_timeline
    let call_timeline = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_get_timeline",
            "arguments": {
                "session_key": "s-1"
            }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_timeline).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let timeline_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(timeline_resp["id"], 7);
    assert_eq!(timeline_resp["result"]["isError"], false);

    // 8. tools/call agent_mux_search_traces
    let call_search = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_search_traces",
            "arguments": {
                "query": "hello"
            }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_search).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let search_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(search_resp["id"], 8);
    assert_eq!(search_resp["result"]["isError"], false);

    // 9. tools/call agent_mux_analyze_skills
    let call_skills = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_analyze_skills",
            "arguments": {}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_skills).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let skills_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(skills_resp["id"], 9);
    assert_eq!(skills_resp["result"]["isError"], false);

    // 10. tools/call agent_mux_compare_runs
    let call_compare = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_compare_runs",
            "arguments": {
                "a": "l-1",
                "b": "l-1"
            }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_compare).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let compare_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(compare_resp["id"], 10);
    assert_eq!(compare_resp["result"]["isError"], false);

    // 11. tools/call with unknown field in arguments (Tool error -> isError: true)
    let call_bad_args = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "agent_mux_get_briefing",
            "arguments": {
                "disallowed_field": "invalid"
            }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_bad_args).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let err_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(err_resp["id"], 11);
    assert_eq!(err_resp["result"]["isError"], true);
    let err_text = err_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(err_text.contains("[INVALID_ARGUMENT]"));

    // 12. tools/call unknown tool (Protocol error)
    let call_unknown = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "tools/call",
        "params": {
            "name": "unknown_tool",
            "arguments": {}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&call_unknown).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let proto_err_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(proto_err_resp["id"], 12);
    assert!(proto_err_resp["error"].is_object());

    // 13. Ping
    let ping_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "ping"
    });
    writeln!(stdin, "{}", serde_json::to_string(&ping_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    line.clear();
    reader.read_line(&mut line).unwrap();
    let ping_resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ping_resp["id"], 13);

    // 14. Drop stdin -> EOF -> clean exit
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "Server must exit cleanly with code 0 on stdin EOF"
    );
}

#[test]
fn mcp_concurrent_db_writes_do_not_poison_protocol() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("traces.db");
    let ws_path = temp.path().join("workspace");
    std::fs::create_dir_all(&ws_path).unwrap();

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
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
            );
            CREATE VIEW session_stats AS
            SELECT session_key, COUNT(id) AS turn_count, SUM(total_tokens) AS total_tokens, SUM(total_cost_usd) AS total_cost_usd, 0 AS total_tools FROM traces GROUP BY session_key;
            CREATE VIEW trace_stats AS
            SELECT id, session_key, launch_id, total_tokens, total_cost_usd, start_ns, end_ns FROM traces;
            CREATE VIEW skill_stats AS
            SELECT skill, COUNT(DISTINCT trace_id) AS turns_loaded, COUNT(id) AS tools, SUM(total_tokens) AS tokens, SUM(total_cost_usd) AS cost, 0 AS turns_unused FROM observations WHERE skill IS NOT NULL GROUP BY skill;",
        ).unwrap();
    }

    // Spawn a writer thread that inserts rows into SQLite
    let db_clone = db_path.clone();
    let stop_writer = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = std::sync::Arc::clone(&stop_writer);
    let writer_handle = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(&db_clone).unwrap();
        let mut counter = 0;
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            counter += 1;
            let _ = conn.execute(
                "INSERT OR REPLACE INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns) VALUES (?1, 'claude', '/workspace', ?2, ?3)",
                rusqlite::params![format!("s-{counter}"), counter * 1000, counter * 2000],
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let exe = env!("CARGO_BIN_EXE_agent-mux");
    let mut child = Command::new(exe)
        .arg("mcp")
        .arg("serve")
        .arg("--stdio")
        .arg("--db")
        .arg(&db_path)
        .arg("--workspace")
        .arg(&ws_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn agent-mux mcp serve");

    let mut stdin = child.stdin.take().expect("failed to open stdin");
    let stdout = child.stdout.take().expect("failed to open stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0.0" }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).unwrap();

    // Call briefing repeatedly while writer is writing
    for id in 2..=10 {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "agent_mux_get_briefing",
                "arguments": {}
            }
        });
        writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        stdin.flush().unwrap();

        line.clear();
        reader.read_line(&mut line).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["id"], id);
        assert_eq!(resp["result"]["isError"], false);
    }

    stop_writer.store(true, std::sync::atomic::Ordering::Relaxed);
    writer_handle.join().unwrap();

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}
