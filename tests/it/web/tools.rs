//! Read-only tools endpoints over a real temporary trace store.
use super::support::{post, seeded_store, start};
use serde_json::json;

#[test]
fn tools_metrics_sequences_activity_and_recent_edits() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = seeded_store(temp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE observations SET level='ERROR',status_message='denied',mcp_server='files',path='src/a.rs',model='test-model' WHERE id='o1'", []).unwrap();
    // A successful retry in the next turn; a later switch to Bash.
    conn.execute("INSERT INTO observations (id,trace_id,type,name,tool_name,start_ns,end_ns,level,path,skill,total_cost_usd) SELECT 'retry','t2','tool','Edit','Edit',start_ns-1000000000,start_ns-600000000,'DEFAULT','src/a.rs','fix',0.25 FROM observations WHERE id='o2'", []).unwrap();
    conn.execute("INSERT INTO observations (id,trace_id,type,name,start_ns,end_ns,level,total_cost_usd) SELECT 'agent','t2','agent','reviewer',start_ns,start_ns+1000000000,'DEFAULT',0.5 FROM observations WHERE id='o2'", []).unwrap();
    conn.execute("INSERT INTO observations (id,trace_id,parent_id,type,name,start_ns,level,total_cost_usd) SELECT 'child','t2','agent','generation','child',start_ns,'DEFAULT',0.75 FROM observations WHERE id='o2'", []).unwrap();
    drop(conn);
    let (base, server) = start(&db);
    let ask = |area: &str, filter: serde_json::Value| {
        let (status, value) = post(&base, &format!("/api/tools/{area}"), &filter);
        assert_eq!(status, 200, "{value}");
        value["data"].clone()
    };
    for area in ["summary", "failures", "sequences", "activity", "edits"] {
        assert_eq!(
            ask(area, json!({"workspace":"/WS"})),
            ask(area, json!({"workspace":ws.to_str()})),
            "{area}: mixed-case path fragment"
        );
    }
    assert_eq!(
        ask("workspaces", json!({}))["workspaces"],
        json!([ws.to_str()])
    );
    assert_eq!(
        ask("workspaces", json!({"workspace":"missing"}))["workspaces"],
        json!([ws.to_str()])
    );
    let s = ask("summary", json!({}));
    let edit = s["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Edit")
        .unwrap();
    assert_eq!(edit["count"], 2);
    assert_eq!(edit["error_rate"], 0.5);
    assert_eq!(edit["p50_ms"], 200.0);
    assert_eq!(edit["p95_ms"], 400.0);
    assert_eq!(s["mcp_servers"][0]["name"], "files");
    assert!(!s["models"].as_array().unwrap().is_empty());
    assert!(!s["mix"].as_array().unwrap().is_empty());
    let f = ask("failures", json!({}));
    assert_eq!(f["total"], 1);
    assert_eq!(f["calls"][0]["turn_id"], "t1");
    let q = ask("sequences", json!({}));
    assert_eq!(q["retry_count"], 1);
    assert_eq!(q["recovery_count"], 1);
    assert_eq!(q["switches"][0]["name"], "Edit → Bash");
    let a = ask("activity", json!({}));
    assert_eq!(a["agents"][0]["name"], "reviewer");
    assert_eq!(a["agents"][0]["cost_usd"], 1.25);
    assert_eq!(a["skills"][0]["name"], "fix");
    assert_eq!(a["skill_days"][0]["duration_ms"], 400.0);
    let edits = ask("edits", json!({"workspace":ws.to_str()}));
    assert_eq!(edits["total"], 1);
    assert_eq!(edits["edits"][0]["path"], "src/a.rs");
    assert_eq!(edits["edits"][0]["turn_id"], "t2");
    assert_eq!(edits["edits"][0]["count"], 1); // failed writes are excluded
    assert_eq!(ask("edits", json!({"provider":"codex"}))["total"], 0);
    assert_eq!(
        ask("summary", json!({"workspace":"/other"}))["tools"],
        json!([])
    );
    assert_eq!(
        ask("summary", json!({"since":"2099-01-01T00:00:00Z"}))["tools"],
        json!([])
    );
    assert_eq!(ask("edits", json!({"project":ws.to_str()}))["total"], 1);
    assert_eq!(
        ask("summary", json!({"model":"test-model"}))["tools"][0]["count"],
        1
    );
    assert_eq!(
        ask("summary", json!({"until":"2000-01-01T00:00:00Z"}))["tools"],
        json!([])
    );
    assert_eq!(
        post(&base, "/api/tools/summary", &json!({"since":"bad"})).0,
        400
    );
    assert_eq!(
        post(&base, "/api/tools/summary", &json!({"unknown":1})).0,
        400
    );
    assert_eq!(post(&base, "/api/tools/nope", &json!({})).0, 404);
    server.abort();
}

#[test]
fn every_tools_endpoint_honours_server_workspace_scope() {
    use agent_mux::{
        tracing::analysis::Scope,
        web::{WebState, serve},
    };
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let scope = Scope::workspace(&temp.path().join("other")).unwrap();
    let state = Arc::new(WebState::with_scope(db, temp.path().into(), scope).unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = rt.spawn(async move {
        serve(state, listener).await.unwrap();
    });
    for area in [
        "summary",
        "failures",
        "sequences",
        "activity",
        "edits",
        "workspaces",
    ] {
        let (status, value) = post(
            &base,
            &format!("/api/tools/{area}"),
            &json!({"workspace":"/WS"}),
        );
        assert_eq!(status, 200);
        if area == "workspaces" {
            assert_eq!(value["data"]["workspaces"], json!([]));
        }
        let text = value.to_string();
        assert!(!text.contains("Edit"), "{text}");
        assert!(!text.contains("claude:s1"), "{text}");
    }
    server.abort();
}
