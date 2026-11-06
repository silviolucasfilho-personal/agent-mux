# Web explorer

`agent-mux web` serves a read-only web interface over the trace store: the
sessions, their turns and tool calls, search, skill and agent metrics and the
store's health, from the same read-only service the MCP server answers. It
binds the loopback interface only, never writes to the store and never starts
a harness.

```sh
agent-mux web                      # http://127.0.0.1:7331/
agent-mux web --port 0 --open      # a free port, opened in the browser
agent-mux web --db ~/other.db      # another store
agent-mux web --workspace ~/proj   # scope the trace API to one workspace
```

| Option | Meaning |
|---|---|
| `--port N` | Listen on `127.0.0.1:N`; `0` asks the OS for a free port. Default 7331. |
| `--db PATH` | The trace store. Default: `$AGENT_MUX_TRACE_DB`, then `[tracing] db_path` from `profiles.toml` (as `agent-mux trace` resolves it), then `~/.agent-mux/traces.db`. |
| `--workspace DIR` | Scope the trace API to one workspace's sessions, as a session's MCP server is. Default: every workspace. |
| `--open` | Open the URL with the platform opener (`links::open`). |

The URL is printed on stdout (one line, `http://127.0.0.1:<port>/`), so a
script can read it; everything else goes to stderr. A missing store is not an
error: the server starts, `get_health` reports `db_available: false` and the
other tools answer `503` until a writer (the TUI, `trace import`) creates it.

Code: `src/web/` (`mod.rs` the server and CLI, one module per area) and `web/`
(the single-page app, embedded into the binary with `include_str!`: no build
step, no CDN, no network beyond the loopback socket). Tests: `tests/it/web/`.

## API

Every endpoint is `POST` with a JSON body and answers JSON. A success is the
area's own document; an error is `{"ok": false, "error": "<message>"}` with a
4xx or 5xx status. Unknown `/api/` paths are a 404 of that shape. `GET /`,
`GET /index.html` and `GET /static/<file>` serve the app.

| Route | Body | Answer |
|---|---|---|
| `POST /api/trace/briefing` | `BriefingArgs` (`since`, `until`, `provider`, `cursor`, `limit`) | The `TraceService` envelope of `agent_mux_get_briefing`. Session counts need a `--workspace` scope. |
| `POST /api/trace/list_sessions` | `ListSessionsArgs` (`since`, `until`, `provider`, `runtime_state`, `cursor`, `limit`) | `agent_mux_list_sessions` |
| `POST /api/trace/get_session` | `GetSessionArgs` (`session_key`, `launch_id`) | `agent_mux_get_session` |
| `POST /api/trace/get_timeline` | `TimelineArgs` (`session_key`, `launch_id`, `cursor`, `limit`) | `agent_mux_get_timeline` |
| `POST /api/trace/search_traces` | `SearchArgs` (`query`, `session_key`, `provider`, `since`, `until`, `cursor`, `limit`) | `agent_mux_search_traces` |
| `POST /api/trace/analyze_skills` | `AnalyzeSkillsArgs` (`skill`, `provider`, `since`, `until`, `cursor`, `limit`) | `agent_mux_analyze_skills` |
| `POST /api/trace/compare_runs` | `CompareRunsArgs` (`a`, `b`) | `agent_mux_compare_runs` |
| `POST /api/trace/get_health` | `{}` | `agent_mux_get_health` |
| `POST /api/swarm/…` | | Swarms and the bus; see below. |
| `POST /api/runs/…` | | Loop and workflow runs; see below. |
| `POST /api/library/…` | | The configuration library; see below. |

The trace envelope is the one the MCP server returns as `structuredContent`
(`schema_version`, `as_of`, `scope`, `window`, `data`, `coverage`, `warnings`,
`next_cursor`, `truncated`; `src/tracing/analysis/service.rs`). The argument
objects are closed: an unknown field is a `400`. A service error keeps its
code in the message (`[NOT_FOUND] session 'x' not found`) and maps to a status:
`INVALID_ARGUMENT` and `CURSOR_EXPIRED` 400, `SCOPE_DENIED` 403, `NOT_FOUND`
404, `SCHEMA_UNSUPPORTED` and `CONTENT_UNAVAILABLE` 422, `DB_UNAVAILABLE` and
`BUSY` 503, `QUERY_TIMEOUT` 504.

```sh
curl -s -X POST localhost:7331/api/trace/list_sessions -d '{"provider":"claude","limit":5}'
curl -s -X POST localhost:7331/api/trace/get_health
```

## The app

`web/index.html` loads `app.css`, then `app.js`, then `trace.js`, `swarm.js`,
`runs.js` and `library.js`, in that order. `app.js` owns the shell; every
other script only registers pages.

**Hash routing.** A page lives at `#/<path>`; parameters are `:name`
segments (`#/sessions/claude%3As1`) and an optional query string
(`#/sessions?provider=codex&since=24h`). The nav lists the registered pages
in registration order; with no hash the first page opens.

**Page registry.** `window.AgentMux.registerPage({id, title, path, render(el, params), nav})`:

| Field | Meaning |
|---|---|
| `id` | Unique; the nav highlights the page whose `id` (or `navId`) matches. |
| `title` | The nav label and heading; a function of the params for detail pages. |
| `path` | `'/sessions'` or `'/sessions/:key'`; one `:name` per segment. |
| `render(el, params)` | Fills `el` (emptied first); `params` merges the path parameters and the query string. May be async; a thrown error is shown in place. |
| `nav` | `false` keeps a detail page out of the nav; `navId` names the nav entry it belongs to. |

**Client.** `AgentMux.api(path, body)` POSTs JSON and returns the parsed
answer; it throws (`err.status`, `err.body`) on a non-2xx status or an
`{ok: false}` body. Helpers: `fmtTokens`, `fmtCost`, `fmtDuration`, `fmtTime`
(RFC 3339 or nanoseconds), `sinceToRfc3339` (`24h`, `7d`, a date),
`shortPath`; DOM: `h(tag, attrs, ...children)`, `link(path, text, query)`,
`table({columns, rows, sortKey, sortDesc, empty, onRow})` (client-side
sortable columns: `{key, label, num, render, sort, class, title}`), `kv(pairs)`,
`cards(pairs)`, `loading()`, `errorBox()`, `envelopeNote(envelope)` (coverage
and warnings), and `navigate(path, query)`.

Pages: `trace.js` registers Sessions (`/sessions`), Session (`/sessions/:key`),
Search (`/search`), Skills & Agents (`/skills`) and Health (`/health`);
`swarm.js` Swarms (`/swarms`), `runs.js` Runs (`/runs`), `library.js` Library
(`/library`).

**Server side.** `web::router(state)` serves `/`, `/static/{*path}` and merges
`trace::routes()`, `swarm::routes()`, `runs::routes()` and `library::routes()`
(each `pub fn routes() -> axum::Router<Arc<WebState>>`). `WebState` holds the
store path (`db`), the library root (`library_root`, `assets::root()`) and the
one `TraceService` (`trace`) every trace call runs through, so cursors stay
valid between requests; `WebState::new(db, library_root)` scopes it to every
workspace, `with_scope` to one. Routes answer `ApiResult<T>`; `ApiError`
carries the status and renders the JSON error, and a `ServiceError` converts
into it. Store reads go through `tracing::store::open_ro` or the service;
nothing writes. The integration tests start a server with
`tests/it/web/support.rs`: `start(db: &Path) -> (String, JoinHandle<()>)` on a
free port of a runtime shared by the test binary (abort the handle to stop
it), `seeded_store(dir) -> (db, workspace)` with two sessions, and `get` /
`post` helpers over `ureq`.

## Trace store

The five trace pages read the trace API above and nothing else.

- **Sessions** lists every session the scope allows with harness (`provider`),
  workspace-substring, since (`24h`, `7d`, RFC 3339) and runtime-state
  filters; the harness, state and window filters travel to `list_sessions`,
  the workspace filter is applied in the browser since the service has no
  such argument. The list follows `next_cursor` up to twenty pages. Columns
  sort on click; a session key opens its page.
- **Session** calls `get_session` and `get_timeline` together: the summary
  cards (turns, tools, tokens, cost, duration), the card's facts (state,
  outcome, initial goal, current activity, active tools), the tool counts,
  the sub-agents (timeline observations of type `agent`), files modified,
  recent commands, the last assistant output, then the turns with their
  input and output snippets and an observation table each (type, name,
  duration, error).
- **Search** is `search_traces` over prompts and outputs with harness and
  since filters; a match links to its session.
- **Skills & Agents** is `analyze_skills` (turns loaded, attributed calls,
  tokens, cost, errors, p50/p95/max, slow calls, limitations) with skill,
  harness and since filters.
- **Health** is `get_health` as a key/value table plus the raw envelope.

## Swarms and bus

Filled by its own task.

## Runs: loops and workflows

Filled by its own task.

## Configuration library

The configuration library pages and API inspect every prompt, skill, loop
pattern, loop skill, loop agent, template, workflow and agent definition
agent-mux ships, along with any override under `~/.agent-mux/`
(`$AGENT_MUX_LIBRARY_DIR`) that shadows a compiled-in asset. The library is
strictly read-only: it enumerates assets and serves their effective text
without writing, resetting, or modifying any files.

- **Library (`#/library`)** lists every configurable item grouped into sections
  by kind (`prompts`, `settings`, `skill`, `workflow`, `agent`, `loop patterns`,
  `loop skill`, `loop agent`, `loop template`). Each item displays its name,
  a source badge indicating whether it is `built-in`, an `override` shadowing
  a default, or a `user` asset, its path on disk, byte size, and validation
  status. A search box filters by name or path fragment, and clicking an item
  opens its detail page.
- **Item (`#/library/<id>`)** displays an asset's metadata (kind, source status,
  whether an override shadows a built-in default, override file path, byte size,
  and validation status), followed by the effective text in a monospace block.
- **Agents (`#/agents`)** displays the agent registry loaded from
  `agents::schedule::load_registry`, listing workflow personas and scheduled task
  agents with their description, permitted tools, model, task kind, and
  execution schedule.

### Configuration API

| Route | Method | Description |
|---|---|---|
| `/api/library/kinds` | `GET` | Returns all configuration kinds with their labels, titles, and creatability. |
| `/api/library/items` | `GET` | Lists all configuration items (`id`, `kind`, `name`, `source`, `path`, `size`), optionally filtered by `?kind=` and grouped by kind. |
| `/api/library/items/{id}` | `GET` | Returns an asset's effective text, kind, source, whether an override shadows a built-in, and the override path. |
| `/api/library/agents` | `GET` | Returns the agent registry with name, description, tools, model, task kind, and schedule. |
