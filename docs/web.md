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
| `GET /api/session/<key>/transcript` | `?launch_id=&from_turn=&limit=&turn=` | The session viewer's transcript; see [Session viewer](#session-viewer). |
| `GET /api/export/session/<key>.html` | `?launch_id=&download=1` | The transcript as one self-contained HTML file. |
| `GET /api/resolve?id=<id>` | | `{ok, session_key, launch_id}` for a session key or a launch id (`launches.session_key`, else the traces that carry the launch); `launch_id` is set only when the session's traces carry it, so the pair passes `get_session`. `404` for an unknown id, `400` for an empty one. |
| `POST /api/usage/…` | `UsageArgs` | Token, cost and activity aggregates; see [Usage](#usage). |
| `/api/tools/…` | | A slot for the Tools area (no routes yet). |

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
(`#/sessions?provider=codex&since=24h`). With no hash the Overview opens (the page
with id `overview`, else the first nav page).

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
| `live` | `true` re-renders the page when the store changes: quietly (into a detached element swapped in when done, keeping the selected row), debounced, and never while a field has focus or an overlay is open. Overview, Usage and Sessions are live. |

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

**Pages.** `usage.js` registers Overview (`/overview`, the landing page:
`render` opens the page with id `overview` when the hash is empty, whichever
script registered it) and Usage (`/usage`); see [Usage](#usage). `trace.js`
registers Sessions (`/sessions`), Session (`/sessions/:key`), Search
(`/search`), Skills & Agents (`/skills`) and Health (`/health`); `swarm.js`
Swarms (`/swarms`) and Bus (`/bus`), `runs.js` Runs (`/runs`) and Inbox
(`/inbox`), `library.js` Library (`/library`) and Agents (`/agents`).
`tools.js` is an empty slot for the Tools area.

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
- **Session** is the session viewer; see [Session viewer](#session-viewer).
- **Search** is `search_traces` over prompts and outputs with harness and
  since filters. A match links to its session and, in the "where" column
  (`turn input` / `turn output`), to the exact turn it was found in
  (`#/sessions/<key>?turn=<trace id>&launch_id=<launch>`, from the match's
  `source_id` `trace:<id>:input|output`); `Enter` on a row opens the turn.
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

The Overview and Usage pages (`web/usage.js`) read `/api/usage/`
(`src/web/usage.rs`): aggregates computed in SQL over the trace store,
opened read-only per request on a blocking thread. Nothing writes.

### What is counted

- **Tokens and cost** are the `generation` observations whose own
  `start_ns` lies in the window, so a window holds only what was spent
  inside it: a session that spent $10 on Nov 1 and $0.10 on Nov 9 adds
  $0.10 to a Nov 8–9 window and to Nov 9's bucket. (Bucketing a session's
  lifetime total by its last activity, as the first Overview did, does not.)
- **Turns** are `traces` by `start_ns`; a **session** counts when it has a
  turn or a generation in the window; **launches** by `started_ns`, with
  their `reported_lines_added` / `reported_lines_removed`.
- **Token kinds**: `input` (uncached), `output`, `cache_read`, `cache_write`
  and `reasoning` (part of `output`, Codex). `total_tokens` is input +
  output + cache read + cache write.
- **Projects** group by the session's `cwd` (else its `project_slug`);
  **models** by `observations.model`.

### Which cost is used

`cost_usd` is always the sum of the generations' own costs: agent-mux's
price for the tokens (`total_cost_usd`, from the `models` price table), or,
for a call whose transcript carries the provider's own cost, that figure
(the store keeps it in `observations.provided_cost` and leaves the priced
columns empty; usage reads `COALESCE(total_cost_usd, provided_cost.total)`,
and the same for the input, output and cache parts). Only this cost can be
placed on a day, an hour, a model and a project.

The harness's own total (`launches.reported_cost_usd`, e.g. Claude Code's
status line) is a figure for the whole launch, not attributable to a day or
a model, and can differ a lot from the computed one (it covers the whole
launch, outside the window too, and calls agent-mux did not price). It is shown
beside the computed cost as `reported_cost_usd` (summed over the launches
started in the window, per harness; per session the largest of its
launches) and never replaces or adds to it. When the two differ, the
computed cost is the one every total, chart and ranking uses.
`unpriced_generations` counts calls with tokens but neither a price row nor
a provider cost: their tokens are counted and their cost is missing.

### Request

Every route is `POST /api/usage/<name>` with one closed JSON object (an
unknown field is a `400`; an empty body is `{}`):

| Field | Meaning |
|---|---|
| `since`, `until` | RFC 3339, or relative before now (`24h`, `7d`, `90m`, `2w`). Default: the 7 days before now. `since` after `until` is a `400`. |
| `provider` | Only this harness (`sessions.provider`, `launches.provider`). |
| `project` (alias `workspace`) | A case-insensitive fragment of the session's (launch's) `cwd` or `project_slug`. |
| `tz_offset_min` | Minutes east of UTC for day, hour and weekday buckets (the page sends `-new Date().getTimezoneOffset()`). Default 0, at most ±840. |
| `limit` | Rows of `sessions` (default 50, at most 500); the Overview top lists show at most 8. |

`agent-mux web --workspace DIR` scopes every aggregate: only sessions and
launches whose `cwd` the server's `TraceService` scope allows
(`Scope::allows_session_cwd`) count. Every answer carries `ok`, `window`
(`since`, `until`, `since_ns`, `until_ns`, `tz_offset_min`) and `filters`.
Errors are the shared JSON errors: `400` for bad arguments, `503` while the
store does not exist (it is never created).

### Routes

| Route | Answer |
|---|---|
| `overview` | `totals` (`sessions`, `launches`, `turns`, `generations`, the five token kinds, `total_tokens`, `cost_usd`, `unpriced_generations`, `reported_cost_usd`, `lines_added`, `lines_removed`); `days` (every local day of the window); `daily` (`day`, `cost_usd`, `total_tokens`, `input_tokens`, `output_tokens`, `generations`, `turns`, `sessions`; days with data only; a session counts on every day it started a turn or made a call, so a turn that crosses midnight counts on both days); `calendar` (`from`, `to`, `turns` per day, reaching at least 12 weeks back from `until`); `top_projects`, `top_models`, `top_harnesses` (group rows, below); `live` (`available`, `sessions`: the TUI snapshots' sessions that are not exited and pass the filters and the scope: `launch_id`, `session_key`, `provider`, `cwd`, `state`, `updated_at_ns`, `active_tools`). |
| `daily` | `days`, `models` (the 8 costliest, then `other`), `cost` and `tokens`: one `{name, values}` series per model, a value per day. |
| `breakdown` | `totals` as above; `models`, `projects`, `harnesses`. A group row has `generations`, `sessions` (for a project or a harness, every session that started a turn or made a call in the window, as in `totals`; for a model, the sessions that called it), the token kinds, `total_tokens`, `cost_usd`, `share` (of the window's cost) and `unpriced_generations`; a project row adds `turns`; a harness row adds `turns`, `launches`, `reported_cost_usd`, `lines_added`, `lines_removed`. |
| `cache` | `totals` and per-model rows: `hit_ratio` (cache read ÷ (input + cache read + cache write)), `input_tokens`, `cache_read_tokens`, `cache_write_tokens`, their costs, `input_rate_per_m` (the observed uncached input price: input cost ÷ input tokens), `savings_usd` (cache-read tokens at that price, less what they cost), `write_premium_usd` (what cache writes cost above that price) and `net_savings_usd`. A model with no priced input has no estimate (`null`); the totals sum the models' own estimates. |
| `sessions` | `total` (sessions in the window) and `sessions`, ranked by in-window `cost_usd`: `session_key`, `provider`, `cwd`, `title`, `turns`, `tools` (tool and agent calls in the window's turns), `tools_per_turn`, `first_ns`, `last_ns`, `duration_ms` (first to last activity in the window), the token kinds, `cost_usd`, `reported_cost_usd`. |
| `distributions` | `sessions`; `archetypes` (`archetype`, `description`, `sessions`, `cost_usd`, `total_tokens`, `avg_turns`); `duration_ms`, `turns` and `tools_per_turn`, each a `histogram` (`labels`, `counts`) and `percentiles` (`p50`, `p95`, `max`, `count`; nearest rank, `tracing::analysis::metrics::completed_percentiles`). |
| `activity` | `weekdays` (`Sun` … `Sat`); `turns`, `cost_usd` and `tokens` as 7 × 24 matrices (weekday × local hour); `turns_by_hour`, `turns_by_weekday`, `cost_by_hour`, `cost_by_weekday`, `tokens_by_hour`. |

A session's **archetype** is the first rule it fits, over its activity in
the window: `quick` (at most 2 turns and under one tool call per turn),
`tool-heavy` (10 or more tool calls per turn), `long-running` (an hour or
more from first to last activity), `conversation` (10 or more turns and
under 3 tool calls per turn), else `task`.

```sh
curl -s -X POST localhost:7331/api/usage/overview -d '{"since":"30d"}'
curl -s -X POST localhost:7331/api/usage/sessions -d '{"provider":"claude","limit":10}'
```

### Pages

Both pages take the shared filter bar (range, harness, workspace/project),
send the browser's time-zone offset, and are `live`.

- **Overview** (`#/overview`, the landing page): cards for sessions, launches
  (with lines changed), turns, tokens and cost (with daily sparklines) and
  the live sessions now; the token kinds, unpriced calls and the reported
  total; the daily spend as bars or a line (a toggle kept per viewer in
  `localStorage`; a bar opens Usage on that day); the activity calendar
  (turns per day; a day opens Usage on it); the top projects (opening
  Usage filtered to one), models and harnesses by cost; and the table of
  live sessions, linked to their session pages.
- **Usage** (`#/usage`): summary cards; cost and tokens per day stacked by
  model; sortable tables per model, per project/workspace (a row filters the
  page to it) and per harness (with launches, the reported cost and lines
  changed); the cache economics (cards and a per-model table); the 50
  costliest sessions in the window, each linked to its session page; the
  archetypes and the duration, turns and tools-per-turn histograms with
  their percentiles; and activity by weekday × hour, by hour, by weekday and
  cost by hour.

Tests: `tests/it/web/usage.rs` seeds a store at fixed dates and checks every
route's numbers, the filters, the scope, the time-zone buckets and the
errors, including that out-of-window spend is not counted.

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

`#/sessions/<key>` (`web/trace.js`, the `session` page) reads one session,
or one launch of it with `?launch_id=`, from two routes: `get_session` (the
runtime state, outcome, initial goal and current activity) and
`GET /api/session/<key>/transcript` (everything else).

**Header.** Cards for duration, turns, tokens (input, output and cache),
cost, lines added/removed (the launches' `reported_lines_*`) and tool
calls with their errors; then the harness, the models (generations' `model`,
most used first), the workspace, each launch with its profile, start and
termination, and links to what it belongs to, read from
`launches.metadata`: `swarm_id` → `#/swarms/<id>` (with the task key),
`loop_run_id` → `#/runs/loop/<id>`, `workflow_run_id` →
`#/runs/workflow/<id>` (with the step). A session traced with
`content_mode = metadata` says so in a banner: prompts, outputs and tool
inputs and outputs were not stored, and only turns, timings, tools, tokens
and cost are shown (`mixed` when its launches differ).

**Charts** (`M.charts`): tokens per turn (stacked input, cache read, cache
write, output), cost per turn (a bar opens its turn) and the context per
generation: input plus cache read plus cache write tokens of every
generation, labelled `<turn>.<n>`, which shows the context growing and
dropping at a compaction.

**Transcript.** One card per turn: the user prompt, the turn's thinking
(collapsed), then its observations as a tree (`parent_id`): a generation
shows its assistant text and thinking and holds its tool calls; a tool call
is a collapsed row (name, path or command, duration) with its input
(pretty-printed JSON) and output; a failed call (`level = ERROR`) is red,
opened, with its status message; a skill load (`kind = skill_load` or
`skill`) and the turn's `skills` carry a skill badge; an agent invocation
nests its sub-agent's turns and calls in an indented block with its prompt
and result. The turn's final output is shown when no generation carries
text. Long texts show the first 1,500 characters or 24 lines behind
**Show all**; the endpoint itself cuts a field at 32,000 characters
(`TRANSCRIPT_MAX_CHARS`) and the card says so. Each turn header links to
itself (`#`).

**Outline.** A sticky list of the turns beside the transcript (prompt,
errors in red, a bar for its tokens); a click scrolls to the turn. `j`/`k`
and `]`/`[` step to the next/previous turn (taken before the shell's row
keys while the page is open). `?turn=<trace id>` opens the page at that
turn, loading as many turns as it takes (the endpoint's `turn_index` says
where it is), so Search and the Tools pages link to an exact turn.

**Files touched.** Every `observations.path` of the session: calls, tools,
errors and when it was last touched, newest first.

**Live.** While the session is live (a `working`, `waiting_for_user` or
`idle` runtime state, an open turn, or activity read from the live
process) the page subscribes to `M.live`, and on a store change fetches the
turns from the first open one (or the last) to the end, page after page
however many arrived (`drainTurns`), and replaces those cards in place
(when the reader has not loaded every turn, only the header and the **Load
more** count change): the reader's scroll position (anchored on the first visible turn),
opened details and loaded turns stay; a reader at the bottom follows new
turns. It unsubscribes once the session is no longer live, and on leaving.

**Paging.** The page loads 200 turns and offers **Load more**.

**Export.** **Open export** and **Download HTML** fetch
`GET /api/export/session/<key>.html`: the whole transcript (every turn and every stored character, read 200
turns at a time; only the JSON transcript pages and clips) as one HTML
document with
inline CSS, light and dark, thinking and tool calls in `<details>`, no
script and no external resource (`Content-Security-Policy: default-src
'none'; style-src 'unsafe-inline'`). Every stored text is HTML-escaped
(`web::trace::escape_html`). `download=1` sends
`Content-Disposition: attachment; filename="session-<key>.html"` (the key
with characters outside `[A-Za-z0-9._-]` as `_`).

### Transcript API

`GET /api/session/<key>/transcript` opens the store read-only, applies the
server's workspace scope (403 outside it, as `get_session`), refuses a
`launch_id` the session's traces do not carry (400), and answers 404 for an
unknown session and 400 for an unknown query key.

| Query | Meaning |
|---|---|
| `launch_id` | Only that launch's turns, totals and files. |
| `from_turn`, `limit` | Turns `from_turn..from_turn+limit` in start order (default 0 and 200, at most 2,000). |
| `turn` | A trace id; its index comes back in `turn_index` (`null` when it is not this session's). |

The answer (`web::trace::Transcript`): `ok`, `session_key`, `launch_id`,
`title`, `provider`, `cwd`, `content_mode` (`full`, `metadata`, `mixed`,
`unknown`), `launches` (`id`, `provider`, `profile`, `content_mode`,
`started_ns`, `ended_ns`, `termination`, `exit_code`, `cost_usd`,
`lines_added`, `lines_removed`, `swarm {id, role, task, task_key}`,
`loop_run {run_id, loop_id, pattern}`, `workflow {run_id, name, step,
phase}`), `models`, `started_ns`, `ended_ns`, `duration_ms`, `totals`
(generation tokens by kind, `cost_usd`, `generations`, `tool_calls`,
`errors`, `lines_added`, `lines_removed`; over every turn, not the page),
`files` (`path`, `count`, `tools`, `errors`, `first_ns`, `last_ns`),
`total_turns`, `from_turn`, `turn_index` and `turns`. A turn has `index`,
`id`, `ordinal`, `launch_id`, `status`, times, `input`, `output`,
`thinking`, `skills`, its generation tokens and cost, `errors` and
`observations`; an observation has `id`, `parent_id`, `type`, `kind`,
`name`, times, `error`, `level`, `status_message`, `model`, `input` (never
for a generation: that is its whole context), `output`, `thinking`, tokens,
`cost_usd`, `tool_name`, `skill`, `mcp_server` and `path`. A cut text is
named in its item's `truncated` list with its stored length in `lengths`.

```sh
curl -s 'localhost:7331/api/session/claude%3As1/transcript?limit=5'
curl -s -o session.html 'localhost:7331/api/export/session/claude%3As1.html'
```

Tests: `tests/it/web/session.rs`.
