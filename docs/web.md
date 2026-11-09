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
| `GET /api/live` | | Server-Sent Events: the store changed; see [Live](#live). |
| `GET /api/resolve?id=<id>` | | `{ok, session_key, launch_id}` for a session key or a launch id (`launches.session_key`, else the traces that carry the launch); `launch_id` is set only when the session's traces carry it, so the pair passes `get_session`. `404` for an unknown id, `400` for an empty one. |
| `/api/usage/…`, `/api/tools/…` | | Slots for the Usage and Tools areas (no routes yet). |

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

`web/index.html` loads `app.css`, then `app.js`, then `trace.js`, `usage.js`,
`tools.js`, `swarm.js`, `runs.js` and `library.js`, in that order. `app.js`
owns the shell; every other script only registers pages.

**Layout.** A sidebar of grouped pages (Overview, Sessions, Usage, Tools,
Swarms/Runs, Library, Health; an empty group is not drawn), a top bar with
the page title, the command palette button and `?`, and the page. At phone
width (≤ 760px) the sidebar folds into a bar with a menu button. The live
indicator beside the brand is green while `/api/live` is connected.

**Themes.** Light and dark follow `prefers-color-scheme`; the toggle at the
bottom of the sidebar (or `t`) cycles auto → light → dark, stored in
`localStorage` (`agent-mux-theme`) and applied as `html[data-theme]` before
the first paint. Every colour is a CSS variable on `:root`, redefined for
dark under `@media (prefers-color-scheme: dark)` (`:root:not([data-theme=light])`)
and under `:root[data-theme=dark]`; chart series read `--c1` … `--c8` and the
heatmap `--heat-0` … `--heat-4`, so charts redraw on a theme change.

**Hash routing.** A page lives at `#/<path>`; parameters are `:name`
segments (`#/sessions/claude%3As1`) and an optional query string
(`#/sessions?provider=codex&since=24h`). With no hash the first nav page
(Overview) opens.

**Page registry.** `window.AgentMux.registerPage({id, title, path, render(el, params), nav, navId, group, key, live})`:

| Field | Meaning |
|---|---|
| `id` | Unique; the nav highlights the page whose `id` (or `navId`) matches. |
| `title` | The nav label and heading; a function of the params for detail pages. |
| `path` | `'/sessions'` or `'/sessions/:key'`; one `:name` per segment. |
| `render(el, params)` | Fills `el` (emptied first); `params` merges the path parameters and the query string. May be async; a thrown error is shown in place. |
| `nav` | `false` keeps a detail page out of the nav; `navId` names the nav entry it belongs to. |
| `group` | The nav group (`AgentMux.groups`). Defaults by id (`sessions`/`search` → Sessions, `skills` → Tools, `swarms`/`bus`/`runs`/`inbox` → Swarms/Runs, `library`/`agents` → Library, `health` → Health), else Tools. |
| `key` | The letter after `g` that jumps to the page (defaults: `o` Overview, `s` Sessions, `f` Search, `u` Usage, `t` Tools, `k` Skills, `w` Swarms, `b` Bus, `r` Runs, `i` Inbox, `l` Library, `a` Agents, `h` Health). |
| `live` | `true` re-renders the page when the store changes: quietly (into a detached element swapped in when done, keeping the selected row), debounced, and never while a field has focus or an overlay is open. Overview and Sessions are live. |

`AgentMux.onLeave(fn)` runs `fn` when the current page is left or
re-rendered (to drop a subscription or a timer).

**Client.** `AgentMux.api(path, body)` POSTs JSON and returns the parsed
answer; it throws (`err.status`, `err.body`) on a non-2xx status or an
`{ok: false}` body.

**Formatting.** `fmtTokens` (`950`, `12.3k`, `1.2M`, `3.4B`), `fmtNum`
(`1,234,567`), `fmtCost` (`$3.41`, `$0.0042`, `$12.5k`), `fmtPercent`
(ratio → `42%`), `fmtDuration` (ms → `850ms`, `12.4s`, `3m05s`, `2h07m`,
`3d4h`), `fmtTime` (RFC 3339 or nanoseconds → local
`YYYY-MM-DD HH:MM:SS`), `fmtAgo` (`5h ago`), `dayKey` (local `YYYY-MM-DD`),
`sinceToRfc3339` (`24h`, `7d`, a date), `shortPath`. Every formatter answers
`–` for a missing value.

**DOM.** `h(tag, attrs, ...children)`, `svg(tag, attrs, ...children)`,
`link(path, text, query)`, `hrefFor(path, query)`,
`table({columns, rows, sortKey, sortDesc, empty, emptyHint, onRow})`
(client-side sortable columns: `{key, label, num, render, sort, class, title}`;
rows take part in `j`/`k`, and `Enter` calls `onRow` or follows the row's
first link), `kv(pairs)`, `cards([[label, value, extra]])` (`extra`: a
sparkline or a hint), `panel(title, ...children)`, `loading(text)`,
`empty(text, hint)`, `errorBox()`, `envelopeNote(envelope)` (coverage and
warnings), `navigate(path, query)` and `setQuery(changes)` (merge into the
current route's query; `null` removes a key).

### Filter bar

`AgentMux.filterBar({params, fields, providers, workspaces, defaults})`
returns a form that keeps the shared filters in the hash query string, so a
filtered view is a link:

| Query key | Control |
|---|---|
| `range` | Presets `24h`, `7d` (the default), `30d`, `90d`, `custom`. |
| `from`, `to` | Local days (`YYYY-MM-DD`) of a `custom` range; `to` is inclusive. |
| `provider` | The harness (`claude`, `codex`, `antigravity`, `opencode`; `providers` overrides the list). |
| `workspace` | A workspace or project fragment; `workspaces` fills the suggestions. |

`fields` picks among `'range'`, `'provider'` and `'workspace'` (default all
three). A change navigates, so the page renders again with the new params.
`AgentMux.filters(params, defaults)` reads them back as
`{range, from, to, since, until, provider, workspace}` with `since`/`until`
in RFC 3339 (`''` when open), ready for a trace call's arguments.

### Charts (`M.charts`)

Dependency-free SVG, drawn at the container's width (redrawn on resize and on
a theme change), coloured from the theme's CSS variables. Every mark carries a
`data-tip` the shared tooltip shows on hover. An empty input draws the empty
state instead.

| Helper | Draws |
|---|---|
| `charts.bar({labels, series: [{name, values}], stacked, height, format, onClick(i), empty})` | Grouped bars, or one stacked bar per label with `stacked: true`; a legend when there are several series. |
| `charts.line({labels, series: [{name, values}], area, height, format})` | Lines, filled below with `area: true`; one hover column per label shows every series. |
| `charts.heatmap({values: {'YYYY-MM-DD': n}, from, to, format, onClick(day)})` | A calendar of weeks by weekday in five shades, with month labels and a less/more legend. |
| `charts.hbars({items: [{label, value, href, hint, title}], format, limit, color})` | A ranked horizontal bar list (HTML), the value and its share in the tooltip. |
| `charts.sparkline(values, {width, height, area, format, labels, color})` | An inline SVG trend for a card or a table cell. |

`format` formats axis ticks and tooltips (`M.fmtTokens`, `M.fmtCost`…;
default `fmtNum`). Helpers: `charts.color(i)` (the series colour),
`charts.dayRange(since, until)` (the local days in between, for bucketing),
`charts.niceMax(v)`.

```js
const M = window.AgentMux;
const days = M.charts.dayRange(M.filters(params).since);
el.appendChild(M.panel('Tokens per day', M.charts.bar({
  labels: days.map((d) => d.slice(5)),
  series: [{ name: 'input', values: inputs }, { name: 'output', values: outputs }],
  stacked: true, format: M.fmtTokens,
})));
```

### Keyboard

| Keys | Action |
|---|---|
| `Ctrl+K` / `⌘K` | Command palette: the pages, toggle theme, the shortcuts, and the most recent sessions; typing filters them, and any text can be opened as a session key or launch id (resolved through `/api/resolve`, then `#/sessions/<key>?launch_id=<id>`; an unknown id stays in the palette with the reason). `↑`/`↓` move, `Enter` opens, `Esc` closes. |
| `j` / `k` | Next / previous table row on the page. |
| `Enter` | Open the selected row. |
| `/` | Focus the page's search or first filter field (else open the palette). |
| `g` then a letter | Jump to a page (its `key`). |
| `t` | Toggle the theme. |
| `?` | The shortcuts overlay, listing every page's `g` key. |
| `Esc` | Close an overlay, or leave a field. |

Single letters are ignored while a field has focus.

### Live

`GET /api/live` is a Server-Sent Events stream (`text/event-stream`) over the
store's change feed. The server reads `MAX(seq)` of `trace_changes` with a
read-only connection (`tracing::store::latest_change_seq`) every
`WebState::live_poll` (`web::LIVE_POLL`, one second) on a blocking thread, and
sends:

- `event: ready` once on connect, `data: {"seq": n, "previous": n}`;
- `event: change` whenever the sequence differs from the last one sent,
  `data: {"seq": n, "previous": m}` (also when it goes down: a replaced store);
- a comment every 15 seconds to keep an idle connection open.

The `id:` of each event is its sequence. A missing store, or one without the
feed, reads as `0`, and the stream starts sending once a writer creates it;
the route never creates or writes the store.

In the app, `AgentMux.live.subscribe(fn)` calls `fn({seq, previous})` on each
change and returns the unsubscribe function; one `EventSource` serves every
subscriber, and a reconnect that missed changes counts as one.
`AgentMux.live.status` is `connecting`, `live` or `offline`. A page with
`live: true` subscribes through the shell.

```sh
curl -N localhost:7331/api/live
```

**Pages.** `app.js` registers Overview (`/overview`: the filter bar, cards
with sparklines, sessions per day by harness, cost per day, an activity
heatmap and the top workspaces by cost, from `list_sessions`). `trace.js`
registers Sessions (`/sessions`), Session (`/sessions/:key`), Search
(`/search`), Skills & Agents (`/skills`) and Health (`/health`); `swarm.js`
Swarms (`/swarms`) and Bus (`/bus`), `runs.js` Runs (`/runs`) and Inbox
(`/inbox`), `library.js` Library (`/library`) and Agents (`/agents`).
`usage.js` and `tools.js` are empty slots for the Usage and Tools areas.

**Server side.** `web::router(state)` serves `/`, `/static/{*path}`,
`/api/live` and merges `trace::routes()`, `swarm::routes()`, `runs::routes()`,
`library::routes()`, `usage::routes()` and `tools::routes()` (each
`pub fn routes() -> axum::Router<Arc<WebState>>`). `WebState` holds the
store path (`db`), the library root (`library_root`, `assets::root()`), the
one `TraceService` (`trace`) every trace call runs through, so cursors stay
valid between requests, and the live poll interval (`live_poll`);
`WebState::new(db, library_root)` scopes it to every workspace, `with_scope`
to one. Routes answer `ApiResult<T>`; `ApiError` carries the status and
renders the JSON error, and a `ServiceError` converts into it. Store reads go
through `tracing::store::open_ro` or the service; nothing writes. A new
static file is added to `web::ASSETS` (`include_str!`). The integration tests
start a server with `tests/it/web/support.rs`: `start(db: &Path) -> (String,
JoinHandle<()>)` on a free port of a runtime shared by the test binary (abort
the handle to stop it), `seeded_store(dir) -> (db, workspace)` with two
sessions, and `get` / `post` helpers over `ureq`. Each area has its own test
file (`core.rs`, `usage.rs`, `tools.rs`, `session.rs`, `swarm.rs`, `runs.rs`,
`library.rs`).

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

## Usage

## Tools

The **Tools** group has Tools, Agents & skills activity, and Recent edits pages.
All use the shared date, harness and workspace filter bar. Activity links to the
existing Skills & Agents analysis at `#/skills`.

Read-only `POST /api/tools/{endpoint}` accepts a JSON object with optional
`since` and `until` (RFC 3339, inclusive/exclusive), `provider`, `workspace`
(case-insensitive path fragment; `project` is an alias) and `model` filters. The server's `--workspace`
scope always applies, even when a request names another project. Responses
are `{ok: true, data: ...}`; invalid dates/fields return 400 JSON errors.

- `workspaces`: sorted distinct project directories with observed activity in
  the date/harness/model and server workspace scope. The typed workspace
  fragment is ignored here so the filter bar can suggest matching projects.
- `summary`: `tools`, `providers`, `models`, `provider_mix`, `model_mix`, `days`, `mix` and `mcp_servers`.
  Aggregates include count, errors, error_rate (0–1), nearest-rank p50/p95
  duration in milliseconds, total duration and recorded cost (null if unknown).
  Mix keys are UTC day-start nanoseconds followed by `|tool`; day keys are UTC
  day-start nanoseconds. Harness/model mix keys use `group|tool`. Missing tool models fall back to a model in that turn.
- `failures`: `total` and the latest 500 failing `calls`, including error text,
  session_key and turn_id. Errors use the current schema's `level = ERROR`.
- `sequences`: `retry_count`, `recovery_count`, latest 500 `retries` and
  `recoveries` (each with `from`/`to` calls), and frequency-sorted `switches`.
  These compare adjacent tool calls within a session, across turns: a retry
  repeats the failed tool, recovery follows an error with success, and a
  switch changes tools. Filters apply before sequence construction.
- `activity`: `agents`, `skills`, `agent_days`, `skill_days`; observation-level
  usage, recorded cost and duration. Agent/skill costs include their direct
  child observations, as in agent_stats; missing costs stay unknown.
  Concurrent durations may overlap.
- `edits`: `total` project/path groups and latest 500 `edits`, newest first.
  Successful edit/write/patch tools with an observed path contribute; each
  group has its count and latest call's session and turn. Workspace filters
  select a project. Paths are trace metadata, never read from disk.

These queries open `WebState.db` read-only on a blocking thread and never
write to the store or invoke a harness.

Session links use `?turn=<trace id>` for the session viewer’s exact-turn deep link.

## Session viewer
