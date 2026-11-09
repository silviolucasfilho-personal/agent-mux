//! Read-only swarm and bus routes over a real temporary schema.
use super::support::{get, post, seeded_store, start};
use agent_mux::bus::{
    TaskIsolation, TaskStatus, store as bus,
    swarm::{plan::PlannedTask, record},
};
use agent_mux::tracing::store::{now_ns, open_aux};
use serde_json::{Value, json};

fn read(base: &str, path: &str) -> Value {
    let (status, content_type, body) = get(base, path);
    assert_eq!(status, 200, "{path}: {body}");
    assert!(content_type.contains("application/json"));
    serde_json::from_str(&body).unwrap()
}

#[test]
fn swarm_and_bus_routes_read_seeded_facts() {
    let temp = tempfile::tempdir().unwrap();
    let (db, workspace) = seeded_store(temp.path());
    let conn = open_aux(&db).unwrap();
    let ws = bus::normalize_workspace(&workspace);
    let now = now_ns();
    let s = record::create_swarm(
        &conn,
        None,
        &record::NewSwarm {
            workspace: ws.clone(),
            goal: "Ship the web".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: None,
            owner_run: None,
            worker_pool: vec![],
            headless: false,
            review: agent_mux::bus::swarm::review::ReviewSettings::off(),
            merge_gate: Default::default(),
        },
        now,
    )
    .unwrap();
    let mut ids = Vec::new();
    for key in ["schema", "queries"] {
        ids.push(
            record::add_task(
                &conn,
                None,
                &s,
                &PlannedTask {
                    key: key.into(),
                    title: key.into(),
                    instructions: "Implement it".into(),
                    ..Default::default()
                },
                false,
                now,
            )
            .unwrap(),
        );
    }
    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
         content_mode, correlation_plan, started_ns, ended_ns, agent_mux_version, metadata)
         VALUES ('web-worker', 'run-web', 1, 'claude', 'claude', ?1, 'web', 'full', '{}', ?2, ?2, 'test', ?3)",
        rusqlite::params![ws, now, json!({"swarm_id": s.id, "swarm_role": "worker",
            "swarm_task": ids[0], "swarm_task_key": "schema"}).to_string()]
    ).unwrap();
    bus::update_task(
        &conn,
        None,
        &ids[0],
        "web-worker",
        TaskStatus::Done,
        Some(json!({"summary": "Implemented"})),
        None,
        None,
        Some("feature/web".into()),
        now,
    )
    .unwrap();
    let message = bus::send_message(
        &conn,
        None,
        &ws,
        "web-worker",
        None,
        "message",
        "Ready for review",
        None,
        now,
    )
    .unwrap();
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_messages WHERE delivered_ns IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let data_version: i64 = conn
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    let (base, server) = start(&db);
    let list = read(&base, "/api/swarm/list?since=0");
    assert_eq!(list["swarms"][0]["swarm"]["id"], s.id);
    assert_eq!(list["swarms"][0]["workers"], 1);
    assert_eq!(list["swarms"][0]["total"], 2);
    let root = format!("/api/swarm/{}", s.id);
    let detail = read(&base, &root);
    assert_eq!(detail["report"]["swarm"]["goal"], "Ship the web");
    assert_eq!(detail["report"]["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(detail["report"]["messages"][0]["body"], "Ready for review");
    assert!(!detail["tree_lines"].as_array().unwrap().is_empty());
    assert!(detail["timeline"]["lanes"].as_array().unwrap().len() >= 2);
    assert!(!detail["log"].as_array().unwrap().is_empty());
    let board = read(&base, &format!("{root}/tasks"));
    assert_eq!(board["tasks"][0]["task"]["status"], "done");
    assert_eq!(board["tasks"][0]["task"]["branch"], "feature/web");
    assert_eq!(
        board["tasks"][0]["task"]["result"]["summary"],
        "Implemented"
    );
    assert_eq!(board["tasks"][1]["task"]["status"], "open");
    let q = format!("?workspace={}", ws.replace('/', "%2F"));
    let peers = read(&base, &format!("/api/bus/peers{q}&all=1"));
    assert_eq!(peers["peers"][0]["launch_id"], "web-worker");
    assert!(
        read(&base, &format!("/api/bus/peers{q}"))["peers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let other = format!("?workspace={}", temp.path().join("other").display());
    assert!(
        read(&base, &format!("/api/bus/messages{other}"))["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        read(&base, &format!("/api/bus/tasks{other}"))["tasks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let messages = read(&base, &format!("/api/bus/messages{q}"));
    assert_eq!(messages["messages"][0]["id"], message.id);
    assert_eq!(
        read(&base, &format!("/api/bus/tasks{q}"))["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (status, via_post) = post(&base, &root, &json!({}));
    assert_eq!(status, 200);
    assert_eq!(via_post["report"]["swarm"]["id"], s.id);
    let after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_messages WHERE delivered_ns IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after, "reading messages never marks delivery");
    for path in ["/api/swarm/missing", "/api/swarm/missing/tasks"] {
        let (status, _, body) = get(&base, path);
        assert_eq!(status, 404);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ok"], false);
    }
    let (status, _, body) = get(&base, "/api/bus/tasks");
    assert_eq!(status, 400);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ok"], false);
    let (status, _, _) = get(&base, "/api/swarm/list?since=invalid");
    assert_eq!(status, 400);
    let (status, _, script) = get(&base, "/static/swarm.js");
    assert_eq!(status, 200);
    for page in ["'Swarms'", "'Swarm'", "'Bus'", "Timeline", "Messages"] {
        assert!(script.contains(page));
    }
    assert_eq!(
        conn.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        data_version,
        "API reads never change the store"
    );
    server.abort();
}

#[test]
fn swarm_missing_store_returns_json_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let (base, server) = start(&temp.path().join("missing.db"));
    let (status, _, body) = get(&base, "/api/swarm/list");
    assert_eq!(status, 503);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ok"], false);
    server.abort();
}
