//! `agent-mux web`: a read-only web explorer over the trace store.
//!
//! The server binds `127.0.0.1` only, serves the single-page app embedded
//! from the repo-level `web/` folder (`include_str!`, no build step, no
//! CDN) and answers JSON under `/api/`. Every area owns one Rust module and
//! one script: `trace` (`/api/trace/`, `web/trace.js`), `swarm`
//! (`/api/swarm/`, `web/swarm.js`), `runs` (`/api/runs/`, `web/runs.js`)
//! `library` (`/api/library/`, `web/library.js`), `usage` (`/api/usage/`,
//! `web/usage.js`) and `tools` (`/api/tools/`, `web/tools.js`); each exposes
//! `pub fn routes() -> Router<Arc<WebState>>` and `router` merges them.
//! `GET /api/live` is a Server-Sent Events stream that says when the
//! store's `trace_changes` feed advances (`live`).
//!
//! Nothing here writes to the store or starts a harness: connections are
//! `tracing::store::open_ro`, and the trace API is `TraceService`, the
//! same read-only service behind `agent-mux mcp serve`. Every JSON error is
//! `{"ok": false, "error": "…"}` with a 4xx/5xx status (`ApiError`).
//!
//! Guide: `docs/web.md`.

pub mod library;
pub mod runs;
pub mod swarm;
pub mod tools;
pub mod trace;
pub mod usage;

use crate::tracing::analysis::{Scope, ServiceConfig, ServiceError, TraceService};
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Shared state of one server, `Arc`ed into every route.
pub struct WebState {
    /// The trace store (opened read-only per query).
    pub db: PathBuf,
    /// The configuration library root (`assets::root()`).
    pub library_root: PathBuf,
    /// The trace service every `/api/trace/` call runs through. One per
    /// server, so pagination cursors stay valid across requests.
    pub trace: TraceService,
    /// How often `/api/live` reads the change feed.
    pub live_poll: Duration,
}

/// Default `/api/live` poll interval.
pub const LIVE_POLL: Duration = Duration::from_millis(1000);

impl WebState {
    /// A state over `db` scoped to every workspace.
    pub fn new(db: PathBuf, library_root: PathBuf) -> Result<Self, ServiceError> {
        Self::with_scope(db, library_root, Scope::all_workspaces())
    }

    pub fn with_scope(
        db: PathBuf,
        library_root: PathBuf,
        scope: Scope,
    ) -> Result<Self, ServiceError> {
        let trace = TraceService::new(ServiceConfig::new(db.clone(), scope))?;
        Ok(Self {
            db,
            library_root,
            trace,
            live_poll: LIVE_POLL,
        })
    }
}

/// A JSON error: `{"ok": false, "error": "…"}` with its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        let status = match &err {
            ServiceError::InvalidArgument(_) | ServiceError::CursorExpired(_) => {
                StatusCode::BAD_REQUEST
            }
            ServiceError::NotFound(_) => StatusCode::NOT_FOUND,
            ServiceError::ScopeDenied(_) => StatusCode::FORBIDDEN,
            ServiceError::DbUnavailable(_) | ServiceError::Busy(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ServiceError::QueryTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            ServiceError::SchemaUnsupported(_) | ServiceError::ContentUnavailable(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ServiceError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, err.to_string())
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "ok": false, "error": self.message });
        (self.status, axum::Json(body)).into_response()
    }
}

/// `Result` every JSON route answers.
pub type ApiResult<T> = Result<axum::Json<T>, ApiError>;

/// The SPA files, embedded at build time: (published path, media type, text).
pub const ASSETS: &[(&str, &str, &str)] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_str!("../../web/index.html"),
    ),
    (
        "app.css",
        "text/css; charset=utf-8",
        include_str!("../../web/app.css"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/app.js"),
    ),
    (
        "trace.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/trace.js"),
    ),
    (
        "swarm.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/swarm.js"),
    ),
    (
        "runs.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/runs.js"),
    ),
    (
        "library.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/library.js"),
    ),
    (
        "usage.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/usage.js"),
    ),
    (
        "tools.js",
        "text/javascript; charset=utf-8",
        include_str!("../../web/tools.js"),
    ),
];

/// One embedded asset by its published name.
pub fn asset(name: &str) -> Option<(&'static str, &'static str)> {
    ASSETS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, mime, text)| (*mime, *text))
}

async fn index() -> Response {
    let (mime, text) = asset("index.html").expect("index.html is embedded");
    ([(header::CONTENT_TYPE, mime)], text).into_response()
}

async fn static_file(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    match asset(&path) {
        Some((mime, text)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            text,
        )
            .into_response(),
        None => ApiError::not_found(format!("no such asset: {path}")).into_response(),
    }
}

/// The highest `trace_changes` sequence of the store at `db`, read-only:
/// `0` while the store or the feed does not exist. `conn` is kept between
/// polls and dropped on an error so the next poll reopens it.
fn live_seq(db: &Path, conn: &mut Option<rusqlite::Connection>) -> i64 {
    if conn.is_none() {
        *conn = crate::tracing::store::open_ro(db).ok();
    }
    let Some(c) = conn.as_ref() else {
        return 0;
    };
    match crate::tracing::store::latest_change_seq(c) {
        Ok(seq) => seq,
        Err(_) => {
            *conn = None;
            0
        }
    }
}

/// A `/api/live` event: `ready` once with the current sequence, then
/// `change` whenever it moves (`{"seq": n, "previous": m}`).
fn live_event(kind: &str, seq: i64, previous: i64) -> Event {
    Event::default()
        .event(kind)
        .id(seq.to_string())
        .data(serde_json::json!({ "seq": seq, "previous": previous }).to_string())
}

/// `GET /api/live`: Server-Sent Events over the store's change feed. Polls
/// `MAX(seq)` of `trace_changes` every `WebState::live_poll` on a blocking
/// thread (read-only connection) and emits `change` when it differs from
/// the last value sent; a comment line keeps idle connections open.
async fn live(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    struct Poll {
        state: Arc<WebState>,
        conn: Option<rusqlite::Connection>,
        last: Option<i64>,
    }
    async fn read(mut p: Poll) -> (Poll, i64) {
        let db = p.state.db.clone();
        let conn = p.conn.take();
        let (conn, seq) = tokio::task::spawn_blocking(move || {
            let mut conn = conn;
            let seq = live_seq(&db, &mut conn);
            (conn, seq)
        })
        .await
        .unwrap_or((None, 0));
        p.conn = conn;
        (p, seq)
    }
    let start = Poll {
        state,
        conn: None,
        last: None,
    };
    let stream = futures_util::stream::unfold(start, |p| async move {
        let mut p = p;
        loop {
            if p.last.is_some() {
                tokio::time::sleep(p.state.live_poll).await;
            }
            let (next, seq) = read(p).await;
            p = next;
            match p.last {
                None => {
                    p.last = Some(seq);
                    return Some((Ok::<_, Infallible>(live_event("ready", seq, seq)), p));
                }
                Some(prev) if prev != seq => {
                    p.last = Some(seq);
                    return Some((Ok(live_event("change", seq, prev)), p));
                }
                Some(_) => {}
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveQuery {
    id: String,
}

/// What `/api/resolve` answers for an id: the session key it names, and
/// the launch id when the id was a launch whose traces carry it (so it can
/// travel to `get_session` as `launch_id`).
fn resolve_id(
    conn: &rusqlite::Connection,
    id: &str,
) -> rusqlite::Result<Option<(String, Option<String>)>> {
    use rusqlite::OptionalExtension;
    let session: Option<String> = conn
        .query_row("SELECT key FROM sessions WHERE key = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(key) = session {
        return Ok(Some((key, None)));
    }
    // a launch: its own row first, else the traces it wrote
    let mut key: Option<String> = conn
        .query_row(
            "SELECT session_key FROM launches WHERE id = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    if key.is_none() {
        key = conn
            .query_row(
                "SELECT session_key FROM traces WHERE launch_id = ?1 LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
    }
    let Some(key) = key else {
        return Ok(None);
    };
    let traced: bool = conn
        .query_row(
            "SELECT 1 FROM traces WHERE session_key = ?1 AND launch_id = ?2 LIMIT 1",
            [key.as_str(), id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    Ok(Some((key, traced.then(|| id.to_string()))))
}

/// `GET /api/resolve?id=<session key or launch id>`:
/// `{ok, session_key, launch_id}` (`launch_id` null unless the id was a
/// launch its session's traces carry); 404 when the store knows neither.
/// The command palette opens a session typed by either id through it.
async fn resolve(
    State(state): State<Arc<WebState>>,
    query: Result<axum::extract::Query<ResolveQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<serde_json::Value> {
    let axum::extract::Query(q) = query.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let id = q.id.trim().to_string();
    if id.is_empty() {
        return Err(ApiError::bad_request("id is empty"));
    }
    let db = state.db.clone();
    let found = tokio::task::spawn_blocking(move || {
        let conn = crate::tracing::store::open_ro(&db)
            .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;
        resolve_id(&conn, &id)
            .map_err(|e| ApiError::internal(e.to_string()))
            .map(|r| (id, r))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))??;
    match found {
        (_, Some((session_key, launch_id))) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "session_key": session_key,
            "launch_id": launch_id,
        }))),
        (id, None) => Err(ApiError::not_found(format!("no session or launch '{id}'"))),
    }
}

async fn api_not_found() -> Response {
    ApiError::not_found("no such route").into_response()
}

/// The whole application: the SPA, `/static/*` and every area's `/api/`.
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/static/{*path}", get(static_file))
        .merge(trace::routes())
        .merge(swarm::routes())
        .merge(runs::routes())
        .merge(library::routes())
        .merge(usage::routes())
        .merge(tools::routes())
        .route("/api/live", get(live))
        .route("/api/resolve", get(resolve).post(resolve))
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .with_state(state)
}

/// Serves `router(state)` on `listener` until the listener's task ends.
pub async fn serve(state: Arc<WebState>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

/// `--db` default: `$AGENT_MUX_TRACE_DB`, then the configured `db_path`
/// (`[tracing]` in profiles.toml, as `agent-mux trace` reads it), then
/// `~/.agent-mux/traces.db`.
pub fn default_db() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_TRACE_DB")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    if let Ok(cfg) = crate::config::load()
        && let Some(resolved) =
            crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
    {
        return resolved.db_path;
    }
    ServiceConfig::resolve(None, Scope::all_workspaces()).db_path
}

pub const USAGE: &str = "usage: agent-mux web [--port N] [--db PATH] [--workspace DIR] [--open]

  --port N         listen on 127.0.0.1:N (default 7331; 0 picks a free port)
  --db PATH        the trace store (default: as `agent-mux trace` resolves it)
  --workspace DIR  scope the trace API to one workspace (default: every workspace)
  --open           open the URL in the browser

The server is read-only: it never writes to the store or starts a harness.";

/// Default port; `--port 0` asks the OS for a free one.
pub const DEFAULT_PORT: u16 = 7331;

/// Parsed `agent-mux web` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub port: u16,
    pub db: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub open: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut opts = Options {
            port: DEFAULT_PORT,
            db: None,
            workspace: None,
            open: false,
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--port" | "-p" => {
                    i += 1;
                    let raw = args.get(i).ok_or("--port needs a number")?;
                    opts.port = raw
                        .parse()
                        .map_err(|_| format!("--port: not a port number: {raw}"))?;
                }
                "--db" => {
                    i += 1;
                    opts.db = Some(PathBuf::from(args.get(i).ok_or("--db needs a path")?));
                }
                "--workspace" => {
                    i += 1;
                    opts.workspace = Some(PathBuf::from(
                        args.get(i).ok_or("--workspace needs a directory")?,
                    ));
                }
                "--open" => opts.open = true,
                "help" | "--help" | "-h" => return Err(USAGE.to_string()),
                other => return Err(format!("unknown web option: {other}\n\n{USAGE}")),
            }
            i += 1;
        }
        Ok(opts)
    }
}

/// `agent-mux web [--port N] [--db PATH] [--workspace DIR] [--open]`.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let opts = match Options::parse(args) {
        Ok(o) => o,
        Err(msg) if msg == USAGE => {
            println!("{USAGE}");
            return Ok(());
        }
        Err(msg) => anyhow::bail!("{msg}"),
    };
    let db = opts.db.clone().unwrap_or_else(default_db);
    let scope = match &opts.workspace {
        Some(dir) => Scope::workspace(Path::new(dir)).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => Scope::all_workspaces(),
    };
    let state = Arc::new(
        WebState::with_scope(db.clone(), crate::assets::root(), scope)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/");
    println!("{url}");
    if !db.exists() {
        eprintln!(
            "agent-mux web: no trace store at {} yet (the TUI creates it)",
            db.display()
        );
    }
    if opts.open
        && let Err(err) = crate::links::open(&url)
    {
        eprintln!("agent-mux web: could not open the browser: {err}");
    }
    serve(state, listener).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse_every_flag() {
        let args: Vec<String> = ["--port", "0", "--db", "/tmp/t.db", "--open"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = Options::parse(&args).unwrap();
        assert_eq!(opts.port, 0);
        assert_eq!(opts.db.as_deref(), Some(Path::new("/tmp/t.db")));
        assert!(opts.open);
        assert_eq!(Options::parse(&[]).unwrap().port, DEFAULT_PORT);
        assert!(Options::parse(&["--bogus".to_string()]).is_err());
        assert!(
            Options::parse(&["--port".to_string(), "x".to_string()])
                .unwrap_err()
                .contains("not a port")
        );
    }

    #[test]
    fn every_asset_is_embedded_and_index_loads_the_scripts_in_order() {
        for name in [
            "index.html",
            "app.css",
            "app.js",
            "trace.js",
            "swarm.js",
            "runs.js",
            "library.js",
            "usage.js",
            "tools.js",
        ] {
            assert!(asset(name).is_some(), "{name} is embedded");
        }
        let (_, index) = asset("index.html").unwrap();
        let order: Vec<usize> = [
            "app.js",
            "trace.js",
            "usage.js",
            "tools.js",
            "swarm.js",
            "runs.js",
            "library.js",
        ]
        .iter()
        .map(|s| {
            index
                .find(&format!("/static/{s}"))
                .unwrap_or_else(|| panic!("{s}"))
        })
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn service_errors_map_to_statuses() {
        let e: ApiError = ServiceError::NotFound("x".into()).into();
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        let e: ApiError = ServiceError::InvalidArgument("x".into()).into();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        let e: ApiError = ServiceError::DbUnavailable("x".into()).into();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
