//! Shared helpers for the web tests: a server on a free port and a
//! temporary store with a couple of sessions.

use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    Level, ObservationRow, ObservationType, SessionRow, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::{OpenOptions, open_rw};
use agent_mux::web::{WebState, serve};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The runtime every test server runs on: one per test binary, so the
/// `JoinHandle` a test holds outlives the test's own stack.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

/// Starts a server over `db` on `127.0.0.1:0` and returns its base URL
/// (`http://127.0.0.1:<port>`, no trailing slash) and the task serving it;
/// abort the handle to stop the server.
pub fn start(db: &Path) -> (String, tokio::task::JoinHandle<()>) {
    start_with_library(db, &std::env::temp_dir())
}

/// `start` with an explicit configuration library root.
pub fn start_with_library(db: &Path, library_root: &Path) -> (String, tokio::task::JoinHandle<()>) {
    let state = WebState::new(db.to_path_buf(), library_root.to_path_buf()).expect("a web state");
    start_state(state)
}

/// `start` scoped to one workspace, as `agent-mux web --workspace DIR`.
pub fn start_scoped(db: &Path, workspace: &Path) -> (String, tokio::task::JoinHandle<()>) {
    let scope = agent_mux::tracing::analysis::Scope::workspace(workspace).expect("a scope");
    let state =
        WebState::with_scope(db.to_path_buf(), std::env::temp_dir(), scope).expect("a web state");
    start_state(state)
}

/// Serves `state` on a free port of the shared runtime.
pub fn start_state(state: WebState) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(state);
    let rt = runtime();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().unwrap();
    let handle = rt.spawn(async move {
        if let Err(err) = serve(state, listener).await {
            eprintln!("web test server: {err}");
        }
    });
    (format!("http://{addr}"), handle)
}

/// A store under `dir` with two sessions (`claude:s1` with two turns and
/// two tool calls in `<dir>/ws`, `codex:s2` with one turn in
/// `<dir>/other`). Returns the store path and the first workspace.
pub fn seeded_store(dir: &Path) -> (PathBuf, PathBuf) {
    let db = dir.join("traces.db");
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-web".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let ws = dir.join("ws");
    let other = dir.join("other");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let now = agent_mux::tracing::store::now_ns();
    let base = now - 60 * 1_000_000_000;
    let mut ops = vec![
        StoreOp::Session(session(
            "claude:s1",
            "claude",
            "s1",
            &ws,
            "fix the build",
            now,
        )),
        StoreOp::Session(session(
            "codex:s2",
            "codex",
            "s2",
            &other,
            "refactor auth",
            now,
        )),
        StoreOp::Trace(trace(
            "t1",
            "claude:s1",
            "claude",
            "s1",
            1,
            base,
            "fix the build error",
            "fixed it",
        )),
        StoreOp::Trace(trace(
            "t2",
            "claude:s1",
            "claude",
            "s1",
            2,
            base + 10_000_000_000,
            "run the tests",
            "tests pass",
        )),
        StoreOp::Trace(trace(
            "t3",
            "codex:s2",
            "codex",
            "s2",
            1,
            base + 20_000_000_000,
            "refactor the auth module",
            "done",
        )),
    ];
    ops.push(StoreOp::Observation(tool(
        "o1",
        "t1",
        "Edit",
        base + 1_000_000_000,
    )));
    ops.push(StoreOp::Observation(tool(
        "o2",
        "t2",
        "Bash",
        base + 11_000_000_000,
    )));
    store.apply(&ops).unwrap();
    (db, ws)
}

fn session(
    key: &str,
    provider: &str,
    id: &str,
    cwd: &Path,
    title: &str,
    seen_ns: i64,
) -> SessionRow {
    SessionRow {
        key: key.into(),
        provider: provider.into(),
        session_id: id.into(),
        user_id: None,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        project_slug: None,
        transcript_path: None,
        title: Some(title.into()),
        seen_ns,
        extra: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn trace(
    id: &str,
    session_key: &str,
    provider: &str,
    session_id: &str,
    ordinal: i64,
    start_ns: i64,
    input: &str,
    output: &str,
) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: session_key.into(),
        provider: provider.into(),
        session_id: session_id.into(),
        launch_id: None,
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns,
        end_ns: Some(start_ns + 5_000_000_000),
        input: Some(input.into()),
        output: Some(output.into()),
        thinking: None,
        skills: None,
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        metadata: None,
    }
}

fn tool(id: &str, trace_id: &str, name: &str, start_ns: i64) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: None,
        obs_type: ObservationType::Tool,
        name: name.into(),
        kind: None,
        start_ns,
        end_ns: Some(start_ns + 200_000_000),
        level: Level::Default,
        status_message: None,
        model: None,
        input: None,
        output: None,
        thinking: None,
        usage_raw: None,
        usage: None,
        tool_id: None,
        tool_name: Some(name.into()),
        skill: None,
        mcp_server: None,
        path: None,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    }
}

/// `GET base+path`: (status, content-type, body).
pub fn get(base: &str, path: &str) -> (u16, String, String) {
    let res = ureq::get(format!("{base}{path}"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .expect("an HTTP reply");
    let status = res.status().as_u16();
    let ctype = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = res.into_body().read_to_string().unwrap_or_default();
    (status, ctype, body)
}

/// `POST base+path` with a JSON body: (status, parsed JSON body).
pub fn post(base: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let res = ureq::post(format!("{base}{path}"))
        .config()
        .http_status_as_error(false)
        .build()
        .header("content-type", "application/json")
        .send(body.to_string().as_bytes())
        .expect("an HTTP reply");
    let status = res.status().as_u16();
    let text = res.into_body().read_to_string().unwrap_or_default();
    let json = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{path}: body is not JSON ({e}): {text}"));
    (status, json)
}
