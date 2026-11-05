# Swarm: coordinator-first — design

Status: proposed, 2026-11-05 (operator decisions in §13). Supersedes, where
they conflict, `2026-11-02-swarm-orchestrator-design.md` §4 (planning), §5.1
(who spawns), §8 (the screen) and `2026-11-02-swarm-agents-layout-design.md`
§2 (the tree of every swarm), §3.1 (the plan written from a form) and §4.1
(the launch form). The bus (messages, tasks, leases, the `PreToolUse` guard),
the engine's supervision (nudges, lost workers, the final check), worktree
review and the store's `swarms`/`swarm_tasks`/`swarm_coordinators` rows stay.

Operator decisions (2026-11-05): the swarm **starts as a conversation** with
a coordinator on the harness the operator picks; the plan is **shaped and
confirmed in that chat**; the coordinator **launches the workers through the
agent-mux MCP** and agent-mux **supervises**; the screen shows **one swarm**,
its coordinator and the agents it started; the swarm is **one trace**, in the
store, on the screen and in Langfuse.

## 1. Why

Three swarms have run under the 2026-11-02 design. What the operator saw:

| # | Finding | Cause |
| --- | --- | --- |
| 1 | "The plan happens away from me." The goal is typed into a seven-field form, a coordinator plans alone in a terminal, and the operator approves a list of tasks they never shaped. | The plan is a one-way submission (`agent_mux_swarm_plan`) and approval is a key on a board. Questions the coordinator has for the operator go through a bus message. |
| 2 | Too many verbs and states: `y x r p e n d m b > ! a space`, fourteen task glyphs, and `y` meaning four things. | Every operator decision became a verb on the screen because the screen was the only place to decide. |
| 3 | The tree is noisy: every swarm of every folder, ended ones, `★` rows, attempt rows. | The screen was designed as mission control for all swarms. |
| 4 | Hard to follow what the agents do: a terminal shows raw output; nothing shows what each worker did, said, changed or cost over time, or how its sub-agents relate to it. | Workers are plain sessions under a `⇄` bus group. The store does not know a worker belongs to a swarm (`launches.metadata` has `loop_*` and `workflow_*` keys, no `swarm_*`), so neither `agent-mux trace`, Heimdall nor Langfuse can show the swarm as one thing. |
| 5 | The coordinator cannot start anything. It plans, then waits; the operator must come to the screen to approve; agent-mux dispatches. | §3.2 of the layout spec: "What it cannot do: approve, start workers, merge, close". Decided when the coordinator was a wake-dependent LLM turn; the engine now supervises, so the reason has gone. |

The 2026-11-02 stall (a coordinator idle for an hour with three open tasks)
still rules the design: whatever the coordinator is allowed to do, the swarm
must progress when it does not do it.

## 2. Principles

- **The swarm is a conversation with a coordinator.** The operator opens a
  harness, asks for a plan, answers its questions, shapes the plan and says
  "go" in the same chat. Nothing is typed into a form that could be said.
- **The coordinator acts; agent-mux supervises.** The coordinator launches
  the workers through MCP tools, relays the operator's decisions and talks to
  the workers. agent-mux records every decision with who made it, nudges idle
  workers, detects lost ones, starts a ready task the coordinator forgot after
  a grace period, and tells the coordinator what happened. **Every wait has
  an owner**, as before.
- **The screen shows one swarm, live.** Its coordinator, the agents it
  started and their sub-agents, each a real terminal you can watch and type
  into — the coordinator and every worker, on any harness, at any moment of
  the swarm. Other swarms live in the Agents sidebar.
- **Fewer verbs: the screen confirms, the chat decides.** On the screen you
  confirm what is highlighted (`y`), stop (`x`), retry (`r`), talk (`Enter`,
  `>`) and jump to what needs you (`!`). Changing the plan, moving a task to
  another harness, deciding what to merge: you tell the coordinator.
- **One swarm, one trace.** Every launch of the swarm knows its swarm and its
  parent; the store, the CLI, Heimdall and Langfuse can report the swarm as a
  tree with totals.
- Rust owns the state machine, the records and the refresh rule (on
  `trace_changes`, never on a timer): unchanged.

## 3. The flow, from the operator's chair

```
 operator                         coordinator (harness of choice)            agent-mux
 ─────────                        ───────────────────────────────            ─────────
 F1 → New swarm · pick harness ─▶ session opens, /swarm-coordinator loaded   swarms row: talking
 "add OAuth login to the API"  ─▶ reads the code, asks 2–3 questions
 answers                       ─▶ proposes the plan as a table               agent_mux_swarm_plan → review (mirrored on the Board tab)
 "move task 2 to codex, go"    ─▶ resubmits, then                            agent_mux_swarm_launch(operator_said) → running
                                  starts the first wave                      SpawnWorker ×n, launched_by=coordinator
                                  waits (agent_mux_swarm_wait), relays       nudges, lost, DISPATCH_GRACE safety net,
                                  questions, starts the next wave            swarm-events wake when the turn ended
 "merge 1 and 3, drop 2"       ─▶ agent_mux_swarm_action(merge|set-aside)    merge_branch, swarm_reviews, decisions recorded
                                  reports the summary                        check runs → finished
 "close it"                    ─▶ agent_mux_swarm_action(close)              sessions stopped, leases released
```

Every arrow on the right is also a verb on the screen or the CLI
(`agent-mux swarm …`), so a swarm whose coordinator exited, or an operator who
prefers keys, is never stuck.

## 4. Starting a swarm

### 4.1 New swarm

`F1 → New swarm` (and `n` on the Swarm screen, `n` on a swarm group in the
Agents sidebar) opens a one-field dialog:

```
┌ New swarm in ~/workspace/agent-mux ───────────────────────────┐
│ Coordinator   ◉ claude   ○ codex   ○ agy   ○ opencode          │
│                                                               │
│ The coordinator opens as a session. Tell it the goal there;   │
│ it will ask what it needs, propose the plan and start the     │
│ workers once you confirm.                                     │
│                                                               │
│ ▸ Advanced (goal, workers' pool, limits, isolation, gate…)    │
│                                                 ⏎ start  Esc  │
└───────────────────────────────────────────────────────────────┘
```

- **Coordinator:** the harnesses installed, default Claude when installed
  (`App::default_swarm_harness`), else the selected session's. A coordinator
  needs the agent-mux MCP server; a profile with `[agents] mcp = "off"` is
  refused with the reason in the dialog.
- **Workspace:** the selected session's folder, else the launch folder
  (unchanged, `App::open_swarm_view`).
- **Advanced** unfolds today's form (goal, workers may run on, max parallel
  workers, default isolation, gate, check). Anything set there is a **limit
  the coordinator cannot exceed** and is in its context; what is left empty
  is negotiated in the chat. A goal typed here is given to the coordinator in
  its opening prompt, which is also how `--headless` works (§10).
- **Enter** starts the coordinator at once (`App::start_swarm_coordinator`),
  creates the `swarms` row in `talking` (§8), and opens the Swarm screen on it
  with the keyboard in the coordinator's terminal, so the operator's next
  keystrokes are the goal.

### 4.2 Any session can become a coordinator

The `swarm-coordinator` skill (§5.1) is installed user-level for every
harness, visible in the Agents sidebar like other skills. In any agent-mux
session the operator types `/swarm-coordinator` (`$swarm-coordinator` on
Codex, `load skill swarm-coordinator` on OpenCode) and asks for a swarm. The
skill's first tool call is `agent_mux_swarm_start` (§6), which creates the
`swarms` row for the caller's launch and workspace, makes that launch the
coordinator and moves the session into the sidebar's swarm group. Everything
after that is the same as §4.1. This replaces the `coordinator_prompt`'s
monopoly on the role: the role lives in the skill, the opening prompt only
names the swarm and the context file.

## 5. The coordinator

### 5.1 The skill: `swarm-coordinator`

Compiled in, replaces `swarm-plan` (the planning section of `swarm-plan`
becomes `reference/plan.md`; `skill.toml` keeps `hidden = false` so the
sidebar lists it, `auto_approve = true`, `hydrate = ["swarm"]`). Registered in
`skill::builtin_packages` and `Catalog::load`, so it can be shadowed and
edited from the Configuration view. Its playbook has five parts:

1. **Understand.** Read `$AGENT_MUX_SWARM_CONTEXT` (§5.2). If the goal is not
   in it, ask for it. Look at the code the goal touches (a dozen commands at
   most). Ask the operator **at most three questions in one message**, only
   what changes the split: scope, what "done" looks like, what must not be
   touched. Use the harness's own question tool where it has one (Claude's
   `AskUserQuestion`); plain text otherwise.
2. **Propose.** Submit the plan with `agent_mux_swarm_plan` (unchanged checks:
   keys, cycles, installed harnesses, agents, worktrees, the agy–writing-agent
   refusal; the pool and the limits from the context). Fix the errors it
   answers. Then **show the plan to the operator as a table** — number, key,
   title, harness·agent, isolation, depends on, done-when — followed by the
   workers' cap, the check command and the open warnings, and ask one
   question: *"Start these N workers? Say go, or tell me what to change."*
   The tool's answer carries that table rendered (`Submitted::table`), so the
   coordinator prints what the Board tab shows.
3. **Confirm and launch.** On a clear yes, call
   `agent_mux_swarm_launch(operator_said = "<their words>")`. On changes,
   edit and resubmit (a resubmission replaces draft tasks, never started
   ones: `record::replace_draft`), show the table again and ask again. The
   coordinator never launches without an answer it can quote; the tool
   refuses an empty `operator_said`.
4. **Run.** Within a turn: `agent_mux_swarm_wait(wait_ms)` until something
   happens (a task done, failed, lost, a question, a proposal, every task
   settled), then act: answer a worker, forward a question to the operator
   with a recommendation, launch the next wave with
   `agent_mux_swarm_launch(tasks = [...])`, propose follow-ups. Report to the
   operator in one short paragraph per event batch. When a wait returns
   on its timeout with workers still busy, say so in the chat (which tasks,
   for how long, what they last did) and ask whether to keep waiting or
   stop something; the operator decides (§5.5). When the turn ends,
   agent-mux wakes the coordinator with the events it missed (§5.4).
5. **Finish.** When every task is settled, summarise: what each task did
   (the workers' `result` lines), the branches awaiting review with their
   diffs' size, the check result, time, tokens and cost. Ask which branches to
   merge; relay with `agent_mux_swarm_action(verb = "merge" | "set-aside",
   task, operator_said)`. When the operator says to close, `close`.

The skill also says what the coordinator must not do: edit files, run the
workers' tasks itself, launch without a quotable yes, or pretend a tool
succeeded. The `[agent] preamble` rules of `prompts.toml` are included.

### 5.2 Context and opening prompt

`$AGENT_MUX_SWARM_CONTEXT` (renames `$AGENT_MUX_SWARM_PLAN`; the old name is
set too for one release) is a JSON file under
`<runtime>/swarms/<id>/context-<n>.json` with today's `PlannerContext` plus:

- `goal`: optional now (empty when the operator will say it in the chat)
- `limits`: `max_workers`, `pool`, `default_isolation`, `check`, `gate` — what
  Advanced or the CLI fixed; `null` where the coordinator may propose
- `tools`: the swarm MCP tools and the `agent-mux swarm` fallbacks, so a
  harness without MCP can still drive the swarm from a shell
- `screen`: one line telling the coordinator what the operator sees ("the
  Swarm screen mirrors your plan read-only; confirmation happens here, in
  this chat")

The opening prompt (`prompt::coordinator_prompt`, rewritten) is three lines:
the swarm id and workspace, the skill invocation, the context path. The
template stays in `prompts.toml` under `[swarm] coordinator` so it can be
edited. A restart (`r` on `★`, `swarm restart`) adds the board as today.

### 5.3 What the coordinator may do, and who records it

| Decision | Chat (through the coordinator) | Screen | CLI | Recorded as |
| --- | --- | --- | --- | --- |
| propose or change the plan | `agent_mux_swarm_plan` | — (`>` tells the coordinator) | `swarm submit-plan` | plan revision |
| approve and launch | `agent_mux_swarm_launch(operator_said)` | — (the Board says *confirm in the chat*) | `swarm approve` (scripts), `swarm launch` | `swarm_decisions`: `launch`, `by = chat|cli`, `said` |
| start a specific task / next wave | `agent_mux_swarm_launch(tasks)` | — | `swarm launch <task>` | `swarm_tasks.launched_by = coordinator|dispatcher|operator` |
| answer a worker | `agent_mux_send_message` | `>` / `m` | `bus send` | message |
| pause, resume | `agent_mux_swarm_action` | `space` | `swarm pause|resume` | decision |
| retry, cancel a task | `agent_mux_swarm_action(operator_said)` | `r`, `x` | `swarm retry|cancel` | decision |
| merge, set aside a branch | `agent_mux_swarm_action(operator_said)` | `y`, `x` on the task | `swarm merge|set-aside` | decision + `swarm_reviews` |
| accept, drop a proposal | `agent_mux_swarm_action` | `y`, `x` | `swarm accept|drop` | decision |
| close | `agent_mux_swarm_action(operator_said)` | `y` on `finished` | `swarm close` | decision |

- **`operator_said` is required** for launch, merge, set-aside, cancel and
  close through MCP: the operator's words, quoted. It is shown in Activity,
  in the Log tab and in the decision's row, so the operator can always see
  what the coordinator took as a yes. Pause, resume, retry, accept and drop
  do not need it (they are reversible or already the coordinator's call).
- **Gate modes** (`swarms.gate_mode`, §8): `chat` (default: the coordinator
  relays your yes; the Board tab is read-only and says *confirm in the
  coordinator's chat*, and `y` there focuses the coordinator's terminal) and
  `off` (a plan without errors runs at once; the coordinator still shows the
  table). There is no screen approval: today's `y`-on-the-board gate is
  removed, not kept as an option. `swarm approve` on the CLI remains for
  scripts and records `by = cli`. Set in Advanced or with `--gate`.
- **The engine refuses what the state forbids**, whoever asks: launching a
  task whose dependency is unmet, merging a task not in review, closing a
  swarm with live workers without `force`. The tools answer the reason, as
  `agent_mux_swarm_plan` does for plan errors.

### 5.4 Supervision: the swarm progresses when the coordinator does not

`bus::swarm::engine::next_actions` keeps every action it has (`Spawn`,
`Nudge`, `MarkLost`, `RunCheck`, `Advance`) and changes who `Spawn` is for:

- **The coordinator launches**; a `Spawn` the engine computes for a ready task
  is first **offered**: the task is listed in the next `swarm-events` batch
  as `ready`, and `agent_mux_swarm_wait` returns it. `swarm_tasks.offered_ns`
  records when.
- **`DISPATCH_GRACE` (2 min, `[swarm] dispatch_grace_s` in profiles.toml):**
  a ready task with a free slot, offered that long ago and still open, is
  started by the engine itself (`launched_by = dispatcher`) and the Log says
  so. The first wave after `launch` without `tasks` is started by the tool
  call itself, so a coordinator that says "go" and ends its turn starts all
  it can at once.
- **Waking:** `tell_coordinator_events` keeps batching (one wake per minute at
  most) and now also carries `ready` tasks, unanswered questions older than
  `STUCK_AFTER` and the final `settled` event. The wake is a paste then a
  separate `\r` (`WAKE_SUBMIT_DELAY`), as today.
- **An exited coordinator** does not stop the swarm: the engine's safety net
  runs the dependents, questions to the coordinator fall to the operator
  after `STUCK_AFTER`, and `★` offers `r` to restart it with the board.
- **Workers' sub-agents** (a worker's Claude `Task` runs, Codex sub-agents)
  are not swarm tasks: they are traced under the worker (`observations.type =
  'agent'`) and shown under it (§7, §9), never dispatched or nudged.

### 5.5 Waiting inside a turn

`agent_mux_swarm_wait(wait_ms)` blocks the coordinator's tool call until a
swarm event or the timeout, so a coordinator can "wait for the workers"
without ending its turn. **The timeout is not a limit on the swarm and never
stops anything.** It exists so the coordinator comes back to the chat at a
known interval and tells the operator where things stand — which tasks are
still working and for how long, what each last did (`agent_mux_swarm_board`),
what is waiting on whom — and asks whether to keep waiting or stop a worker.
The operator decides: "wait" calls `swarm_wait` again, "stop 3" becomes
`agent_mux_swarm_action(cancel, …)`. The default `wait_ms` in the context
(`tools.wait_ms`) is 5 minutes; the operator can say "check back every two
minutes" and the coordinator passes that.

The only hard ceiling is the harness's own MCP tool-call timeout, which would
kill the call instead of returning it: before PR B picks `tools.wait_ms_max`,
probe each harness with the real CLI (Claude Code
`MCP_TIMEOUT`/`MCP_TOOL_TIMEOUT`, Codex's MCP `tool_timeout_sec`, agy and
OpenCode defaults) and record the results in `src/mcp/server.rs`'s module
docs; `wait_ms` is clamped under it. A turn that ends mid-wait loses nothing:
the next wake carries the events.

## 6. MCP tools and CLI

New and changed tools in `src/mcp/server.rs`, all under `bus::swarm::record`
like `agent_mux_swarm_plan`; each has an `agent-mux swarm` twin for a harness
without MCP.

| Tool | Does | CLI |
| --- | --- | --- |
| `agent_mux_swarm_start(goal?, coordinator_harness?, limits?)` | the caller's launch becomes the coordinator of a new swarm in its workspace (§4.2). Refused when the caller is a swarm worker. | `swarm start` (opens a coordinator) |
| `agent_mux_swarm_plan(plan)` | unchanged, plus: the answer carries `table` (the plan rendered for the chat) and `warnings`; a plan after launch may add tasks (they join as `proposed` = accepted when the coordinator submits them, since the coordinator is trusted to relay) | `swarm submit-plan --file` |
| `agent_mux_swarm_launch(operator_said, tasks?)` | approves the plan (`review → running`) and starts the first wave; with `tasks`, starts those ready tasks now (within the cap) | `swarm approve`, `swarm launch [<task>…]` |
| `agent_mux_swarm_wait(wait_ms, since?)` | blocks until the next swarm event; answers the events and the board delta | `swarm wait [--timeout]` |
| `agent_mux_swarm_board(swarm?)` | the board as the screen shows it: each task's shown state, worker, elapsed, cost, result or error, branch, needs; the swarm's totals | `swarm status --json` |
| `agent_mux_swarm_action(verb, task?, operator_said?, notes?)` | pause, resume, retry, cancel, merge, set-aside, accept, drop, close (`SwarmVerb`), with the recording of §5.3 | `swarm <verb>` |
| `agent_mux_get_swarm(swarm, view = tree|timeline|log)` | read-only analysis over the trace store (§7); also for Heimdall | `trace swarm <id>` |

- `agent_mux_delegate` and `agent_mux_task_split` **from the coordinator**
  add tasks to its board and, once the plan is launched, start them like
  `swarm_launch(tasks)`; from a worker they stay proposals (`record::propose`).
- `agent_mux_send_message(to = "coordinator")` and `to = "user"` are
  unchanged. A worker's question to `user` also reaches the coordinator's
  next events so it can answer or recommend.
- The `swarm` is found as today: the caller's coordinator or worker row,
  `$AGENT_MUX_SWARM_ID`, or `--swarm` (`record::resolve_swarm`).
- `ControlRequest::SwarmAction` gains `by: DecidedBy` and `said:
  Option<String>`; `SpawnWorker` gains `swarm: Option<SwarmLink>` (§7.1).

CLI: `swarm start` opens a coordinator session in the running TUI
(`--coordinator`, `--gate chat|off`, `--headless "<goal>"` for §10;
`--planner` kept as an alias one release); `swarm launch`, `swarm wait`,
`swarm events`, `trace swarm` are new; the rest keep their names.

## 7. One swarm, one trace

### 7.1 The store knows the swarm

Every launch of a swarm carries, like `loop_*` and `workflow_*`
(`LaunchPlan`, `map::launch_metadata`), the keys
`launches.metadata.swarm_id`, `swarm_role` (`coordinator` | `worker`),
`swarm_task` (the task id, workers only), `swarm_task_key` and
`parent_launch` (the coordinator's launch for a worker; the operator's session
for a coordinator started from one, §4.2). `SpawnWorker.swarm` and
`start_swarm_coordinator` set them through `LaunchPlan::attach_swarm`. A
worker's sub-agents already hang under its turns (`observations.parent_id`).

With that, one query gives the swarm's tree:

```sql
SELECT l.id, json_extract(l.metadata,'$.swarm_role') AS role,
       json_extract(l.metadata,'$.swarm_task_key') AS task,
       SUM(s.total_tokens) AS tokens, SUM(s.total_cost_usd) AS cost,
       SUM(s.unpriced_generations) > 0 AS unpriced,
       l.started_ns, l.ended_ns
FROM launches l LEFT JOIN trace_stats s ON s.launch_id = l.id
WHERE json_extract(l.metadata,'$.swarm_id') = ?1
GROUP BY l.id
ORDER BY role = 'coordinator' DESC, l.started_ns;
```

`agent-mux trace swarm <id> [--tree | --timeline | --log | --json]` and
`agent_mux_get_swarm` answer from it (`tracing::swarm_report`), with each
launch's turns, tools, sub-agents and totals, and a `trace-sql-examples.md`
section. Heimdall's dossier gains a `swarms` list (last 24 h: id, goal,
state, tasks done/total, tokens, cost, what needs the operator) and
`reference/sessions.md` a swarm paragraph.

### 7.2 The log is a table

`swarm_events` (§8) records what Activity derives today, with the actor:
`plan.proposed`, `plan.revised`, `launch` (by chat/cli, `said`),
`task.offered`, `task.spawned` (by coordinator/dispatcher/operator),
`task.nudged`, `task.lost`, `task.done`, `task.failed`, `question.asked`,
`question.answered`, `message`, `review.merged`, `review.set_aside`, `check`,
`closed`. The screen's Log tab, Activity, `swarm events` and the Langfuse
export read the same rows.

### 7.3 Langfuse: one session per swarm

The exporter already makes one Langfuse trace per turn (`map::root_span_id`)
with `session.id`, tags and metadata attributes (`map.rs`). For a launch with
`swarm_id`:

- `session.id = swarm-<id>` on every turn trace of the coordinator and the
  workers, so Langfuse's session view lists the whole swarm in order and
  `agent-mux langfuse traces -s swarm-<id>` / `langfuse web -s` already open
  it;
- tags `swarm`, `swarm:coordinator` or `swarm:worker`; metadata
  `swarm_task_key`, `parent_launch`;
- on close, one **summary trace** named `swarm <goal>` with a root span for
  the swarm and one child span per task (its timing, state, worker launch
  id, tokens and cost, result or error) and one per decision from
  `swarm_events`. OTLP cannot parent a span across traces, so the child spans
  link to the workers' traces by `trace_id` in metadata. `agent-mux langfuse
  export-swarm <id>` re-sends it, for a swarm that ran with Langfuse off.

## 8. Data model (schema v24)

Replay-safe, as v22 (`pragma_table_info` guards every `ADD COLUMN`).

```sql
-- the swarm's gate and its talking phase
-- Side tables, as v14 and v22 (SQLite has no ADD COLUMN IF NOT EXISTS and
-- migrations replay): the gate and the headless flag per swarm, the engine's
-- per-task dispatch bookkeeping. `swarms.gate` is backfilled into gate_mode.
CREATE TABLE IF NOT EXISTS swarm_settings (
  swarm_id    TEXT PRIMARY KEY REFERENCES swarms (id) ON DELETE CASCADE,
  gate_mode   TEXT NOT NULL DEFAULT 'chat' CHECK (gate_mode IN ('chat','off')),
  headless    INTEGER NOT NULL DEFAULT 0,
  updated_ns  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS swarm_dispatch (
  task_id       TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  offered_ns    INTEGER,                                      -- the engine offered the ready task
  launched_by   TEXT,                                         -- 'coordinator' | 'dispatcher' | 'operator'
  requested_ns  INTEGER,
  updated_ns    INTEGER NOT NULL
);
-- `talking` is shown, not stored: a swarm in `planning` with a coordinator
-- launch and `headless = 0` reads as talking.

CREATE TABLE IF NOT EXISTS swarm_decisions (
  id          INTEGER PRIMARY KEY,
  swarm_id    TEXT NOT NULL REFERENCES swarms (id) ON DELETE CASCADE,
  task_id     TEXT,                                           -- NULL for swarm-level decisions
  verb        TEXT NOT NULL,                                  -- launch, merge, set-aside, cancel, close, …
  by          TEXT NOT NULL CHECK (by IN ('chat','tui','cli')),          -- tui: merge, retry, close… from the screen
  said        TEXT,                                           -- the operator's words, when relayed
  launch      TEXT,                                           -- the coordinator's launch, for 'chat'
  created_ns  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS swarm_events (
  id          INTEGER PRIMARY KEY,
  swarm_id    TEXT NOT NULL REFERENCES swarms (id) ON DELETE CASCADE,
  task_id     TEXT,
  kind        TEXT NOT NULL,                                  -- §7.2
  actor       TEXT,                                           -- a launch id, 'operator', 'engine'
  detail      TEXT,                                           -- JSON
  created_ns  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_events_swarm ON swarm_events (swarm_id, created_ns);
-- trace_changes triggers on both tables, as for swarms/swarm_tasks
```

`swarms.status` lifecycle: `talking` → `review` (a plan is on the board) →
`running` ⇄ `paused` → `integrating` → `finished`; `planning` only under
`--headless`. The migration leaves live swarms as they are (`planning` with a
coordinator launch reads as `talking`).

## 9. The screen

### 9.1 Scope and entry

`Mode::Swarm` shows **one swarm**: `SwarmBoard` keeps `swarm: String` instead
of `swarms: Vec<SwarmData>`. It opens:

- from `F1 → New swarm` on the swarm just started (§4.1);
- from `F1 → Open swarm` on the swarm of the selected session (its
  coordinator, a worker, or a session in a folder with one live swarm; with
  several, a picker lists them);
- from the Agents sidebar: `Enter` on a swarm group;
- from `⚑ swarm needs you (n)` in the sidebar and the status bar, on the
  swarm with the need.

The Agents sidebar lists every live swarm as a group `◆ <goal>` with its
coordinator `★` and workers nested (`GroupKind::Swarm`, replacing `Bus` for
swarm workers; delegated workers outside a swarm keep `Bus` `⇄`), folded by
default once it is `finished`, with `⚑n` while it needs you. That is where
"the other swarms" live now; the Swarm screen has no cross-swarm tree.

### 9.2 Layout

```
┌ ◆ add OAuth login · running · 2/5 ─────┐┌ 1 Board  2 Live  3 Timeline  4 Log  5 Changes ──────── 18m · 41k tok · $1.24 ┐
│ ★ coordinator · claude        ● talking││ ✓ 1 oauth-schema      claude            done 6m $0.31  merged              │
│ ✓ 1 oauth-schema   claude   6m   $0.31 ││ ● 2 oauth-routes      codex·implementer working 4m $0.12 ← 1                │
│ ● 2 oauth-routes   codex    4m   $0.12 ││   ↳ Task: write route tests            sub-agent 1m                          │
│   ↳ sub-agent · tests          1m      ││ ◷ 3 oauth-ui          claude            asking   ! "which provider first?" │
│ ◷ 3 oauth-ui       claude   3m   $0.08!││ ◦ 4 docs              agy               ready    waits for a slot            │
│ ◦ 4 docs           agy           —     ││ ⋯ 5 e2e               codex             blocked  ← 2 3                        │
│ ⋯ 5 e2e            codex         —     ││                                                                               │
│                                        ││ Needs you (1)  ◷ 3 asks "which provider first?"      m answer · ! jump      │
│ ⚑ needs you 1                          ││ Plan: confirmed in chat 12:31 "go" · cap 3 · check: cargo test               │
└────────────────────────────────────────┘└───────────────────────────────────────────────────────────────────────────────┘
[F1 Commands] Swarm ◆ add OAuth login  ↵ talk · y confirm · x stop · r retry · > message · ! needs you · Esc back
```

- **The tree** (left, `ui::SIDEBAR_WIDTH`): the swarm's title row, `★` the
  coordinator, then **one row per agent the swarm started**, in plan order:
  state glyph, number, key, harness, elapsed, cost, `!` when it needs you. A
  worker's **sub-agents** (its `observations.type = 'agent'` rows, live or
  ended) nest under it as `↳` rows with their name and elapsed time, folded
  once they end. Tasks not yet started show without a harness column filled
  in. Nothing else: no other swarms, no attempt rows (a retry replaces the
  row's session; the Log keeps the history).
- **The main pane** shows the selected row, as on the Agents screen:
  - **the swarm row:** tabs `1 Board · 2 Live · 3 Timeline · 4 Log · 5
    Changes` (§9.3); Live is every terminal of the swarm at once;
  - **`★` coordinator:** its terminal (`ui::draw_session_pane`), and a second
    tab `2 Plan` with the current plan table and the decision that confirmed
    it;
  - **a worker:** `1 Task · 2 Live · 3 Thread · 4 Trace` — Trace is new: its
    turns, tool calls and sub-agents from the store, with tokens and cost
    (the per-launch timeline `agent-mux trace show --timeline` draws);
  - **a sub-agent:** its trace rows.
  - The pane opens on the tab that matters (unchanged rule: Thread when it
    asked, Live while it works, Changes when its branch waits, Task
    otherwise; Board for the swarm).
- **Shown states** shrink from fourteen to seven words, each with its glyph:
  `waiting` (`·` draft, `⋯` blocked, `◦` ready — the glyph still tells which),
  `working` (`●`, `◌` starting), `asking` (`◷`), `needs you` (`!`: prompt,
  stalled, lost, failed — the word after it says which), `done` (`✓`),
  `review` (`⇡`), `off` (`–` cancelled, set aside). The word is always drawn
  next to the glyph on the Board; the tree shows the glyph.
- **Needs you** is unchanged in substance (`swarm_board::needs_for`): `!` on
  rows, `⚑n` on the swarm, the list at the foot of the Board. New kinds:
  `confirm` (the coordinator asked "start these N workers?" and the operator
  has not answered in the chat for 5 min: `Enter` on `★`), `waiting` (a
  `swarm_wait` timed out and the coordinator asked "wait more or stop?" with
  no answer yet: `Enter` on `★` to answer; nothing stops on its own) and
  `coordinator quiet` (ready tasks the dispatcher started itself, so the
  operator knows the coordinator is not steering).

### 9.3 The swarm's tabs

- **Board:** the plan as a table (what the coordinator printed in the chat),
  the live state per task, Needs you, then the plan line: how it was
  confirmed (`confirmed in chat 12:31 "go"`, `approved from the CLI`, `gate
  off`), the cap, the check. In `review` with gate `chat`, the Board says
  *waiting for your confirmation in the coordinator's chat — Enter on ★* and
  draws the table read-only.
- **Live:** the mosaic of every live terminal of the swarm, the coordinator
  first; `Enter` on a tile connects to that session (§9.5).
- **Timeline:** one lane per agent (coordinator first, then workers in plan
  order, sub-agents indented under their worker), time left to right from the
  swarm's start, in the Markdown viewer's Unicode style: a bar per session
  lifetime shaded by state (working, idle, asking), marks for decisions
  (`▲ launch`, `◆ merge`), messages between lanes as `│` ticks with the
  topic, the check as the last bar. `←`/`→` pan, `-`/`+` zoom, `Enter` on a
  bar opens that agent's row. Drawn from `launches` + `swarm_events` +
  `agent_messages`, cached per refresh.
- **Log:** `swarm_events` newest last, one line each: time, actor, kind,
  detail; `/` filters by text, `t` cycles kinds (all, decisions, tasks,
  messages), `Enter` opens the row the event is about.
- **Changes:** the combined diff: each worktree task's branch against the
  base, and the shared workspace's `git diff`, one section per task.

### 9.4 Keys

| Key | Does |
| --- | --- |
| `↑` `↓` `g` `G` | choose a row |
| `←` `→` | fold or unfold a worker's sub-agents |
| `Enter` | type into the row's terminal (the `★` is the way to change anything about the plan) |
| `Tab` | tree ↔ pane; `1`–`5` the pane's tab |
| `y` | confirm the one thing highlighted on the row: merge a branch in `review`, accept a proposal, close when `finished`. Never the plan: on a plan in `review`, `y` focuses the coordinator's terminal. The status line says what `y` will do before you press it. |
| `x` | stop a worker · cancel a task · set a branch aside · drop a proposal (confirms with `y`) |
| `r` | retry a failed, lost, cancelled or stalled task (opens the retry form: harness, agent, isolation — the one form left) · restart an exited coordinator |
| `space` | pause or resume |
| `>` `m` | message: the selected worker, `@<key>`, `@all`, `@coordinator` (back: the coordinator is addressable) |
| `!` | the next row that needs you |
| `n` | a new swarm (opens §4.1) |
| `Ctrl+R` | reload |
| `⌘L` | from a terminal, back to the tree |
| `Esc` | back to the tree, then back to the Agents screen |

Gone: `e`, `d`, `p`, `b`, `a` (edit, delete, re-plan, broadcast and Activity:
the first three are said to the coordinator, `b` is `> @all`, Activity is the
Log tab). Registered in `keymap.rs` under the Swarm screen so F1 and Help
list them.

### 9.5 Live: watching and connecting to every session

Every session of the swarm is reachable from the screen, as a terminal, not a
log:

- **Each row is its terminal.** `★` and every worker row draw their PTY in
  the main pane at full size (`ui::draw_session_pane`, the Agents screen's
  pane, so no harness's output is cut), on any harness: Claude Code, Codex,
  agy, OpenCode. `Enter` **connects**: the keyboard goes to that session (the
  pane's title says `Keyboard goes to …`), every key is forwarded as on the
  Agents screen (`Action::ForwardBytes`, including Ctrl, Alt, Esc and Tab),
  and `⌘L` (`Ctrl+Shift+L`) brings it back to the tree. So the operator can
  answer the coordinator, answer a worker's permission prompt, or type a
  follow-up into a finished worker, without leaving the screen. Scrolling the
  pane's scrollback works as on the Agents screen.
- **The swarm's Live tab** (`2` on the swarm row) is a **mosaic** of every live
  terminal of the swarm: the coordinator's tile first, then one tile per
  worker in plan order, laid out in a grid (`ui::swarm_live`: 1 → 1×1, 2 →
  1×2, 3–4 → 2×2, 5–6 → 2×3, more → pages, `PgUp`/`PgDn`). A tile shows the
  bottom-left of that session's screen (its PTY stays sized to the main pane;
  a tile never resizes a PTY, because the harness would redraw and the
  operator attached elsewhere would see it), its title (`★ coordinator ·
  claude · talking`, `2 oauth-routes · codex · working 4m`) and its `!` when
  it needs you. `↑↓←→` move between tiles, `Enter` connects to the tile's
  session (the pane becomes that terminal at full size; `Esc` returns to the
  mosaic), `>` messages it. Tiles refresh with the sessions (`AppEvent` from
  the PTY), not on a timer.
- **Sub-agents** (`↳` rows) have no PTY of their own: `Enter` on one connects
  to its worker's terminal and opens the sub-agent's rows in the `Trace` tab.
- **A session that ended** (a worker whose harness exited, a coordinator that
  quit) keeps its row. Its Live view shows the last screen with its
  scrollback. `Enter` offers to **reconnect**: the same harness resumes the
  same conversation in the same place (`harness::Resume::Id`, Claude Code
  `--resume <id>`, Codex `resume <id>`, OpenCode `--session <id>`; agy has
  none and instead gets a handoff of its transcript, `src/handoff.rs`), in
  the swarm's group, with its `swarm_*` launch metadata and task carried
  over, so the swarm's trace continues under the same swarm id. For the
  coordinator this is what `r` on `★` does, with the board added to its
  opening message; for a worker it is a `retry` that keeps the conversation,
  recorded as `task.reconnected` in the Log.
- **From the Agents screen too:** every swarm session is also a row in the
  swarm's sidebar group (§9.1), so attaching there is the ordinary session
  pane, and `F1 → Open swarm` from it comes back to this screen on that row.
- **Nothing is injected without you seeing it.** A nudge, a wake or a message
  the engine or the coordinator types into a worker shows up in that
  worker's terminal like any other input, and in the Log with its actor.

## 10. Headless fallback (kept)

`agent-mux swarm start --headless "<goal>" [--coordinator …] [--worker-harness …]
[--workers N] [--isolation …] [--gate chat|off] [--check CMD]` and the dialog's
Advanced with a goal: the coordinator gets the goal in its opening prompt,
plans without asking (the skill's Understand step is skipped when
`limits.headless` is set), submits, and then either runs (`--gate off`) or
waits for `agent-mux swarm approve` from a shell (`chat`: the coordinator
also asks in its chat, so whichever answer comes first wins, and both are
recorded). Scripts and the end-to-end tests use it, so no test needs a
conversation. The one-shot print-mode planner of the
orchestrator spec §4.1 is not brought back: the coordinator session is the
planner in both modes.

## 11. Harness notes

- **All four harnesses coordinate:** Claude Code (MCP via `--mcp-config`),
  Codex (`-c mcp_servers…`), agy (`mcp add`), OpenCode
  (`OPENCODE_CONFIG_CONTENT` with `--standalone`, `opencode::adapt_launch`),
  as `mcp::register` wires today. The skill invocation per harness is
  `skill::render::invocation`.
- **Asking the operator:** Claude Code has `AskUserQuestion`; Codex, agy and
  OpenCode ask in text. The skill phrases the confirmation so a plain-text
  answer is unambiguous ("say go").
- **Tool-call timeouts** bound `agent_mux_swarm_wait` (§5.5): probe before
  relying on them.
- **agy workers** stay read-only under a custom agent (`plan::check`,
  unchanged). agy as a **coordinator** is fine: it edits nothing.
- **Wakes** stay paste + delayed `\r` (`pty-wake-submit-probe`).

## 12. Delivery

| PR | Contents | Tests |
| --- | --- | --- |
| **A: store and tools** | Schema v24 (§8); `launches.metadata.swarm_*` (§7.1) through `LaunchPlan::attach_swarm` on `SpawnWorker` and `start_swarm_coordinator`; `swarm_events` written by `record` and the engine; `agent_mux_swarm_launch`, `_wait`, `_board`, `_action`, `_start` and their CLI twins; `DecidedBy` on `SwarmAction`; `offered_ns`, `launched_by` and `DISPATCH_GRACE` in `engine::next_actions`. | engine table tests (offer, grace, coordinator launch vs dispatcher); MCP round-trips against a temp store; migration replay from a v23 fixture |
| **B: the coordinator** | The `swarm-coordinator` skill and `reference/plan.md`; the new opening prompt and `[swarm] coordinator` template; `$AGENT_MUX_SWARM_CONTEXT` with `limits` and `tools`; gate modes `chat`/`off` and the removal of screen approval; `tell_coordinator_events` with `ready` and `settled`; the one-field dialog with Advanced; `swarm start --headless`; the tool-timeout probe recorded. | end to end with a fake coordinator (a script answering MCP over the socket) that proposes, is told "move 2 to codex", resubmits, launches with `operator_said`, waits, merges and closes; a second run where it never launches and the grace net does |
| **C: the screen** | `SwarmBoard` on one swarm; the tree with `★`, workers and sub-agents; the Board tab with the confirmation line; connecting to every session and the Live mosaic (§9.5), reconnecting an ended one through `harness::Resume::Id` and the handoff; the reduced keys; `GroupKind::Swarm` in the Agents sidebar; entry points (§9.1); the retry form kept; `e`/`d`/`p`/`b`/`a` removed and their keymap entries. | render tests at 80/120/180 columns from a store fixture; sidebar grouping tests through `App::select_first_row_of` |
| **D: the trace** | `tracing::swarm_report`, `agent-mux trace swarm`, `agent_mux_get_swarm`; the Timeline, Log and worker Trace tabs; Langfuse `session.id`, tags and the summary trace; `langfuse export-swarm`; Heimdall's `swarms` dossier section and `reference/sessions.md`. | report tests on a fixture swarm; timeline render tests; Langfuse mapping tests on the attributes (no network) |
| **E: docs and cleanup** | `docs/swarm.md` rewritten around the conversation; `docs/bus.md` swarm section; AGENTS.md; status lines on the 2026-11-02 specs; `swarm-plan` removed from `builtin_packages` (shadow packages at `~/.agent-mux/skills/swarm-plan` still load); `--planner` alias removed the release after. | doc links checked |

A and B ship together or not at all (B's tools are A's); C and D can follow in
either order.

## 13. Decisions (operator, 2026-11-05)

1. **Dispatch:** the coordinator launches through MCP; Rust supervises
   (liveness, nudges, lost workers, the grace-period safety net, events).
2. **Approval:** in the coordinator's chat, relayed with the operator's words;
   the screen mirrors it read-only and never approves. Today's screen gate is
   removed (the operator: "I don't want to restore today's behaviour, it is
   bad"); `off` stays as an option, and `swarm approve` for scripts.
2a. **Waiting:** `swarm_wait`'s timeout only brings the coordinator back to
   the chat to report; the operator decides whether to wait more or stop a
   worker. Nothing is killed by a timeout.
3. **Screen:** one swarm per screen — its coordinator and the agents it
   started; the all-swarms tree is replaced by swarm groups in the Agents
   sidebar. The screen must let the operator watch and connect to a live
   session with the coordinator and every agent/harness (§9.5), including a
   mosaic of all of them and reconnecting one that ended.
4. **Tracing:** all four — store linkage, a Timeline tab, a Log tab, and the
   swarm as one Langfuse session with a summary trace.
5. **Entry:** pick the harness only; the goal is said in the chat. Advanced
   keeps the limits for those who want them.
6. **Pain points to fix:** the plan happening away from the operator, too many
   verbs and states, a noisy tree, and no view of what the agents did.
7. **Kept:** the screen's operator verbs (merge, retry, stop, close, pause,
   message), the `agent-mux swarm` CLI, a headless fallback, and the
   plan-gate-off option.

## 14. Not in this design

- A coordinator that writes code itself: it plans, launches, relays and
  summarises; the workers do the work.
- Cross-workspace swarms, or a worker on a remote machine.
- Resuming a swarm after agent-mux restarts: the engine's existing behaviour
  (workers found by their launches, `owner_run`) applies, unchanged.
- OpenCode as a worker's harness with a `PreToolUse` guard (still no guard on
  2.0.32; leases are not enforced for it).
