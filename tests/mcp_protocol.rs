//! `agent-mux mcp serve --stdio` as a harness would drive it: the built
//! binary with piped stdio against a temporary store. Initialize, the tool
//! catalog, tool calls equal to the in-process service, typed tool errors,
//! protocol errors, notifications and EOF.

use agent_mux::tracing::analysis::{BriefingArgs, Request, Scope, ServiceConfig, TraceService};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{SessionRow, StoreOp};
use agent_mux::tracing::store::{OpenOptions, open_rw};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn seeded_store(dir: &Path) -> PathBuf {
    let db = dir.join("traces.db");
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-mcp".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    store
        .apply(&[StoreOp::Session(SessionRow {
            key: "claude:s1".into(),
            provider: "claude".into(),
            session_id: "s1".into(),
            user_id: None,
            cwd: Some(ws.to_string_lossy().into_owned()),
            project_slug: None,
            transcript_path: None,
            title: Some("first".into()),
            seen_ns: agent_mux::tracing::store::now_ns(),
            extra: None,
        })])
        .unwrap();
    db
}

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Server {
    fn spawn(db: &Path, workspace: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
            .args(["mcp", "serve", "--stdio", "--db"])
            .arg(db)
            .arg("--workspace")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Server {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
    }

    fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("a reply within 10 s");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout is not JSON: {e}: {line}"))
    }

    fn call(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}));
        let reply = self.recv();
        assert_eq!(reply["id"], id);
        reply
    }
}

#[test]
fn the_server_speaks_initialize_list_and_call_over_stdio() {
    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let ws = temp.path().join("ws");
    let mut srv = Server::spawn(&db, &ws);

    // initialize negotiates a known version and advertises tools only
    srv.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}));
    let init = srv.recv();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "agent-mux");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["capabilities"].get("resources").is_none());

    // the initialized notification produces no reply; ping does
    srv.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    srv.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    let pong = srv.recv();
    assert_eq!(pong["id"], 2, "no reply was emitted for the notification");
    assert_eq!(pong["result"], json!({}));

    srv.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}));
    let list = srv.recv();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 10);
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"agent_mux_get_briefing") && names.contains(&"agent_mux_get_health"));
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].as_str().unwrap().len() > 20);
    }

    // health answers without touching the store's contents
    let health = srv.call(4, "agent_mux_get_health", json!({}));
    assert_eq!(health["result"]["isError"], false);
    let data = &health["result"]["structuredContent"]["data"];
    assert_eq!(data["db_available"], true);
    assert_eq!(data["reader_version"], env!("CARGO_PKG_VERSION"));

    // a briefing equals what the in-process service returns for the same scope
    let briefing = srv.call(5, "agent_mux_get_briefing", json!({}));
    assert_eq!(briefing["result"]["isError"], false);
    let over_wire = &briefing["result"]["structuredContent"];
    assert_eq!(over_wire["schema_version"], 1);
    let text: Value =
        serde_json::from_str(briefing["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        &text, over_wire,
        "text and structured content carry the same envelope"
    );
    let service = TraceService::new(ServiceConfig::new(
        db.clone(),
        Scope::workspace(&ws).unwrap(),
    ))
    .unwrap();
    let local = service
        .execute(Request::Briefing(BriefingArgs::default()))
        .unwrap();
    assert_eq!(
        over_wire["data"]["total_sessions"],
        json!(local.data["total_sessions"])
    );
    assert_eq!(
        over_wire["scope"],
        serde_json::to_value(&local.scope).unwrap()
    );

    // typed tool errors are results, not protocol errors
    let unknown = srv.call(6, "agent_mux_drop_tables", json!({}));
    assert_eq!(unknown["result"]["isError"], true);
    assert_eq!(
        unknown["result"]["structuredContent"]["error"]["code"],
        "INVALID_ARGUMENT"
    );
    let bad_args = srv.call(
        7,
        "agent_mux_get_session",
        json!({"session_key":"claude:s1","bogus":1}),
    );
    assert_eq!(bad_args["result"]["isError"], true);
    let not_found = srv.call(
        8,
        "agent_mux_get_session",
        json!({"session_key":"claude:nope"}),
    );
    assert_eq!(
        not_found["result"]["structuredContent"]["error"]["code"],
        "NOT_FOUND"
    );

    // protocol errors
    srv.send(json!({"jsonrpc":"2.0","id":9,"method":"resources/list"}));
    assert_eq!(srv.recv()["error"]["code"], -32601);
    writeln!(srv.stdin, "{{not json").unwrap();
    assert_eq!(srv.recv()["error"]["code"], -32700);
    srv.send(json!([{"jsonrpc":"2.0","id":10,"method":"ping"}]));
    assert_eq!(srv.recv()["error"]["code"], -32600);

    // EOF ends the process cleanly
    drop(srv.stdin);
    let status = srv.child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn a_missing_store_keeps_health_and_types_every_other_error() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let mut srv = Server::spawn(&temp.path().join("absent.db"), &ws);
    srv.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}));
    assert_eq!(srv.recv()["result"]["protocolVersion"], "2025-11-25");
    let health = srv.call(2, "agent_mux_get_health", json!({}));
    assert_eq!(
        health["result"]["structuredContent"]["data"]["db_available"],
        false
    );
    let briefing = srv.call(3, "agent_mux_get_briefing", json!({}));
    assert_eq!(briefing["result"]["isError"], true);
    assert_eq!(
        briefing["result"]["structuredContent"]["error"]["code"],
        "DB_UNAVAILABLE"
    );
    assert!(
        !temp.path().join("absent.db").exists(),
        "the server never creates a store"
    );
    drop(srv.stdin);
    assert!(srv.child.wait().unwrap().success());
}
