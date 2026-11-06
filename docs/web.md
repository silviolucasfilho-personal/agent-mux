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

These read-only routes accept **GET** query parameters (and POST aliases
for the shared SPA client); they do not use the trace envelope.

| Route | Answer |
|---|---|
| `GET /api/swarm/list?since=<ns>&workspace=<path>` | `swarms`: up to 500 live or recently ended swarms, newest first, each with `swarm`, worker launch count, cost, done and total tasks. Since defaults to zero; live swarms are always included. Workspace is optional. |
| `GET /api/swarm/{id}` | `report` from `swarm_report::report`, `tree_lines`, structured `timeline` (lanes, spans and marks), and rendered `log` lines, matching the trace CLI. |
| `GET /api/swarm/{id}/tasks` | `tasks`: board entries with the task (status, assigned worker, branch, result), plan extension and review. |
| `GET /api/bus/peers?workspace=<path>&all=1` | `peers` from the bus store; `all=1` includes exited launches. |
| `GET /api/bus/messages?workspace=<path>` | `messages`: the latest 500 workspace messages in chronological order, without marking delivery or acknowledgement. |
| `GET /api/bus/tasks?workspace=<path>` | `tasks`: the workspace bus tasks. |

Bus routes require a nonempty workspace; paths are normalized as in the
bus CLI. Missing swarms return JSON 404, invalid queries JSON 400, and an
unavailable store JSON 503. Every request opens the database read-only.
Live worker state comes from the same runtime snapshots used by swarm
reports outside the TUI.

**Swarms** (`#/swarms`) lists ID, goal, status, start, worker launches and
cost. An ID opens **Swarm** (`#/swarms/<id>`): Board shows tasks and peers,
Tree shows the report tree, Timeline shows one row per lane with alive
bounds, working/question spans and event marks, Log shows the event log,
and Messages shows the swarm's messages. A workspace link opens **Bus**
(`#/bus?workspace=<path>`), whose workspace form loads peers, tasks and
messages. These pages have no write controls.

## Runs: loops and workflows

The Runs area reads `loop_runs`, `workflow_runs` and `workflow_steps` (the
rows `loops::store` and `workflows::store` write) and the launch a run points
at. Its routes are `GET` with path and query parameters, unlike the trace
API, and answer plain JSON documents (`{"ok": true, …}`), not the trace
envelope; an error is the shared `{"ok": false, "error": "…"}`: `404` for an
unknown run, `503` while the store does not exist. Nothing here writes a row
or decides an inbox item.

| Route | Answer |
|---|---|
| `GET /api/runs/loops?limit=N` | `{ok, runs: [loop run…]}`: every loop's runs, newest scheduled first (`loops::store::all_recent_runs`). `limit` defaults to 100, at most 1000. |
| `GET /api/runs/loops/{id}` | `{ok, run, launch, facts, files_touched}`: the row (`loops::store::get_run`), its launch (`{id, session_key, provider, started_ns, ended_ns}` or `null`), the facts read from the launch's traces (`loops::store::run_facts`: `tokens`, `cost_usd`, `verifier_ran`, `verifier_verdict`, `verifier_verdicts`, `files_touched`, `final_message`; `null` without a launch) and the write tools' paths (`loops::store::files_touched`). |
| `GET /api/runs/workflows?limit=N` | `{ok, runs: [workflow run…]}`, newest first (`workflows::store::recent_runs`), without the document text. |
| `GET /api/runs/workflows/{id}` | `{ok, run, steps}`: the run by id or unique id prefix (`workflows::store::resolve_run`), with its `document`, and its sessions (`workflows::store::steps_of`), each with the `session_key` of its launch. |
| `GET /api/runs/inbox` | `{ok, loops, workflows}`: what waits on a human. |

A loop run is `LoopRun` field by field (`id`, `loop_id`, `workspace`,
`pattern`, `harness`, `level`, `effective_level`, `launch_id`, `scheduled_ns`,
`started_ns`, `ended_ns`, `outcome`, `items_found`, `actions_taken`,
`escalations`, `tokens`, `cost_usd`, `readiness_score`, `worktree`, `branch`,
`decision`, `decided_ns`, `detail`, `pattern_hash`) plus `outcome_word` (the
outcome in the reader's words: `fix ready`, `needs you`, `quiet`…),
`duration_s` and `in_inbox`. A workflow run is `WorkflowRun` field by field
plus `result_text` (a string result as is, an object pretty-printed),
`duration_s`, `awaiting` (the verdict a finished run waits on when its
document's `accept` list does not admit it, or `no verdict`) and
`needs_human`. A step is `WorkflowStep` field by field plus `result_text`,
`duration_s` and `session_key`.

**Inbox.** The loop side is `loops::store::inbox`: runs that proposed a fix
or escalated and have no decision yet, the rows the TUI's `I` lists under
Loops. The workflow side applies the TUI's rule to the stored runs
(`web::runs::workflow_needs_human`, after `RecentWorkflowRun::needs_human`):
a run whose status is not `finished`, or that has an error, or that finished
with a verdict its document does not `accept` (the document is parsed from
the row). A run still `running` is not listed. Two things the TUI inbox keeps
in memory only are not here: a planner's document waiting for review, and the
items dismissed with `d` in this TUI session.

```sh
curl -s localhost:7331/api/runs/loops?limit=5
curl -s localhost:7331/api/runs/loops/2026-11-06T08%3A00%3A00Z
curl -s localhost:7331/api/runs/workflows/aed11f58
curl -s localhost:7331/api/runs/inbox
```

Pages (`web/runs.js`; the script fetches with `GET`, so it has its own small
client beside `AgentMux.api`):

- **Runs** (`#/runs`) is two sortable tables, loop runs and workflow runs:
  run id, loop and pattern (or workflow name), workspace, harness, status
  (the outcome word and the decision; the workflow status and the verdict it
  awaits), verdict, started, duration and cost. `?limit=` changes how many
  of each are loaded. A row opens its detail page.
- **Loop run** (`#/runs/loop/<id>`) shows the summary cards, the details
  (loop, pattern and its digest, workspace, harness, level and effective
  level, outcome and reason, decision, times, worktree and branch, the
  launch as a link to `#/sessions/<key>` when the store has it), the verdict
  (the verifier's combined word and each checker's), the budget facts
  (items, actions, escalations, recorded and traced tokens and cost,
  readiness score), the files touched, the final message and the raw
  `detail` document.
- **Workflow run** (`#/runs/workflow/<id>`) shows the summary cards, the
  run (document name and source, workspace, harness and profile, the args,
  status, verdict, error, budget, times), the steps table (session, step,
  phase, kind, item, harness, status, launch link, duration, tokens, cost)
  with each step's result text and changed files folded under it, the run's
  result text, and the TOML the run executed.
- **Inbox** (`#/inbox`) lists the undecided loop runs and the workflow runs
  that need a human, each row linking to its detail page. It only reads:
  decide in the TUI (`I`, `a`/`d`) or with `agent-mux loop decide`.

## Configuration library

Filled by its own task.
