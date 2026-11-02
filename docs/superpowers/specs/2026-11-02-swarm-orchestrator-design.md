# Swarm: agent-mux as the orchestrator — design

Status: draft for review, 2026-11-02. Replaces the coordinator-session model of
`2026-10-29-agent-swarm-screen-design.md` and the lane layout of
`2026-10-30-swarm-mission-control-ux-design.md`. The bus (messages, tasks,
leases, the `PreToolUse` guard, the MCP tools) stays.

Operator decisions (2026-11-02): **agent-mux drives** the swarm; workers are
**interactive PTY sessions**; the screen centres on the **task board**;
delivered as **this spec, then phased PRs**.

## 1. Why

On 2026-11-02 a three-task swarm stalled for over an hour after the plan was
approved: coordinator idle, two unread messages, three tasks `open`, no
worker. The cause is structural, not one bug:

1. **Progress lives inside one LLM turn.** The coordinator advances the
   swarm only while its turn is running and polling
   `agent_mux_recv_messages(wait_ms = 30000)`. When an interactive Claude
   Code turn ends (it always does), nothing advances until something types
   a new prompt into its terminal.
2. **Typing into a terminal is not a reliable signal.** `WakeSession`
   writes `prompt\r` in one burst (`App::handle_bus_control`,
   `flush_pending_bus_wakes`). Claude Code takes the burst as a paste, so
   the `\r` doesn't submit. A session flagged `NeedsAttention` keeps a
   queued wake forever, because only attaching clears the flag.
3. **The plan gate is a prompt.** "Wait for `plan-approved`, then delegate"
   is an instruction to a model. Rust can refuse early spawns, but it
   can't make the late ones happen.
4. **The screen is drawn from agents.** Before a worker exists, the lanes
   show nothing about the plan. The operator approves tasks they see only
   as a message body.

## 2. Principles

- **agent-mux owns the state machine.** Plan → review → dispatch → track →
  integrate → finish are Rust transitions, recorded in rows, and testable
  without a TUI.
- **A model gets all its work in its opening prompt.** The planner is a
  one-shot headless run. A worker is an interactive session whose first
  prompt is its whole task card. No step of the swarm depends on injecting
  text into a terminal.
- **Typing into a terminal is only for steering** (operator messages,
  nudges), and the swarm still progresses when it fails.
- **Every wait has an owner.** Each task that is not moving is either
  waiting on a dependency, waiting for a slot, or listed in *Needs you*.
- The screen refreshes from `trace_changes` and session state, never on a
  timer (unchanged).

## 3. Lifecycle

### 3.1 Swarm states (`swarms.status`)

| State | Meaning | Leaves by |
| --- | --- | --- |
| `planning` | the planner run is working on the goal (or on a re-plan) | its result → `review`; its failure → `review` with the error and an empty board |
| `review` | the plan is on the board for the operator to edit | `y` approve → `running`; `p` re-plan → `planning` |
| `running` | the dispatcher starts ready tasks | all tasks terminal → `integrating`, or `finished` when nothing is left to review |
| `paused` | like `running`, but nothing new starts; running workers continue | `space` → `running` |
| `integrating` | every task is terminal; worktree branches wait to be merged or set aside | all reviewed → `finished` |
| `finished` | summary shown | `y` close → row ended (`ended_ns`), sessions stopped, leases released |

With the plan gate off, `review` passes straight to `running` when the plan
has no errors (§4.3). Warnings are listed in Activity.

### 3.2 Task states, as the board shows them

Stored state is `agent_tasks.status` plus the `swarm_tasks` columns (§7).
The board derives the shown state (`bus::swarm::TaskView::state`), first
match wins:

| Shown | When | Glyph |
| --- | --- | --- |
| `draft` | swarm in `planning`/`review` | `·` |
| `blocked` | a dependency is not `done` (or, for a worktree dependency, not merged: §5.4) | `⋯` |
| `ready` | deps satisfied, waiting for a worker slot or a pause to end | `◦` |
| `starting` | spawned, no hook event yet | `◌` |
| `working` | session working | `●` |
| `asking` | it sent an unanswered `question` | `◷` |
| `stalled` | session idle with the task unfinished, past the grace period (§5.3) | `○` |
| `prompt` | session waiting on a prompt in its terminal (permission, bell) | `⚑` |
| `done` | `done`, nothing to review | `✓` |
| `review` | `done` in a worktree, branch not yet merged or set aside | `⇡` |
| `failed` | `failed` | `✗` |
| `lost` | not terminal, session exited | `!` |
| `cancelled` | `cancelled` | `–` |

## 4. Planning

### 4.1 The planner run

`r` on the screen (or `agent-mux swarm start`) creates the `swarms` row and
starts a **headless one-shot planner**. It's built the same way as
`App::start_workflow_plan`: `LaunchOptions::one_shot`, `bypass_approvals`, an
output file, and `spawn_traced_full`, so its tokens and cost are traced. The
planner harness is chosen in the launch form (default: the first installed
of claude, codex, opencode, agy).

- **Skill:** `swarm-plan`, compiled in, `hidden = true`, read-only. Registered
  in `skill::builtin_packages` and `Catalog::load`, so it can be shadowed and
  edited.
- **Context file:** `$AGENT_MUX_SWARM_PLAN` is a JSON file under
  `<runtime>/swarms/<id>/plan-<n>.json`. It holds:
  - the goal
  - `workflows::planner::inventory(workspace)`
  - the installed harnesses
  - the agent catalog (names and descriptions)
  - `max_workers` and the default isolation
  - whether the workspace is a git repository
  - on a re-plan, the current board and the operator's notes
- **Answer:** one fenced `swarm-plan` block:

```json
{
  "summary": "why this split, in two sentences",
  "tasks": [
    {
      "key": "bus-fix",
      "title": "Bus: all_workspaces lists every workspace",
      "instructions": "…",
      "acceptance": "cargo test bus:: passes; new test covers all_workspaces",
      "files": ["src/mcp/server.rs", "src/bus/store.rs"],
      "depends_on": [],
      "harness": "codex",
      "agent": "implementer",
      "isolation": "worktree"
    }
  ]
}
```

`harness`, `agent` and `isolation` are optional and default to the launch
form's choices.

### 4.2 Turning the answer into a board

`bus::swarm::plan::parse` reads the block, and `plan::check` validates it:

- **Errors** block approval:
  - a key that isn't unique, or a dependency that's unknown or forms a cycle
  - an empty title or instructions
  - a harness that isn't installed
  - an agent that's unknown
  - `worktree` isolation when the workspace isn't a git repository
- **Warnings** don't block: two tasks that may run at the same time (neither
  depends on the other) with overlapping `files` globs under `shared`
  isolation, or more than 12 tasks.

The tasks go onto the board as `agent_tasks` rows (`status = 'open'`), each
with its `swarm_tasks` extension. A re-plan replaces the tasks still in
draft. It never touches a task that has started.

### 4.3 Review

In `review`, the board can be edited. You can:

- edit a task (title, instructions, acceptance, files, dependencies,
  harness, agent, isolation)
- add a task (`n`) or delete one (`D`)
- re-plan with notes (`p`: the command line opens as `notes: `)
- approve (`y`), which is refused while `plan::check` reports errors

The panel shows the selected task's card with its check problems inline.
The planner's `summary` heads the board.

## 5. Running

### 5.1 The dispatcher (`bus::swarm::engine`)

The dispatcher is a pure function:

```rust
fn next_actions(s: &SwarmSnapshot, now_ns: i64) -> Vec<Action>
enum Action { Spawn(TaskId), Nudge(TaskId), MarkLost(TaskId), Advance(SwarmStatus) }
```

`App` calls it after every swarm refresh and every session status change,
then carries out the actions. All decisions live in `next_actions`, so the
tests cover them with table-driven snapshots and no PTY.

- **Spawn:**
  - the task is `open`, its dependencies are satisfied, and the swarm is
    `running`
  - the number of live workers is below `max_workers`
  - tasks start in plan order
  - spawning goes through one function, `App::spawn_swarm_worker`. It is the
    current `SpawnWorker` body, extracted: worktree creation and reuse,
    profile, agent, tracing and the MCP server.
  - the spawned session claims the task for its launch id **before** its
    first turn, so a lost session is detected from that moment
- **Nudge** and **MarkLost** are described in §5.3.
- **Advance** moves the swarm to its next state (§3.1).

### 5.2 The worker's opening prompt

`bus::swarm::worker_prompt(task, deps)` builds the session's first message,
passed on the command line like any opening prompt (`--prompt`, a
positional prompt, `-p`, depending on the harness), never typed into the
terminal. It contains:

- the task card: title, instructions, acceptance, files and isolation
- the results of its dependencies: each one's `result` summary, and the
  merge commit for worktree dependencies
- the bus rules: leases, and `agent_mux_send_message(to = "user",
  topic = "question")` for anything it can't decide
- how to finish: `agent_mux_task_update(action = "done", result = …)` or
  `(action = "fail", error = …)`. The prompt says the task isn't over until
  one of those is called.

### 5.3 Noticing that a worker stopped

| Signal | Action |
| --- | --- |
| `done`/`fail` through the bus | the task is terminal, the session is stopped after `STOP_AFTER_DONE` (30 s, which leaves its last output readable), and the slot frees |
| the session exits with the task not terminal | `MarkLost` → Needs you `lost` (`r` retries, reusing the worktree) |
| session `Idle` with the task unfinished for `STALL_GRACE` (90 s) | one `Nudge`: a fixed wake (§5.5) saying "You stopped before finishing: call `agent_mux_task_update` with done or fail, or ask the user a question." `swarm_tasks.nudged_ns` records it |
| still idle `STALL_GRACE` after the nudge, or the nudge couldn't be delivered | Needs you `stalled`: attach, message, mark it done or failed for the worker (`y`/`f` on the item), or retry |
| `NeedsAttention` (a bell: usually a permission prompt) | Needs you `prompt`. Nothing is injected, because typing into a permission prompt answers it |

### 5.4 Worktrees and dependencies

A worktree task that depends on another worktree task starts once its
dependency's branch is **merged** (§6), so it branches from a base that
already contains that work. Reviews therefore happen as tasks finish, not
all at the end. A `review` task appears in Needs you as soon as it's done,
and merging it unblocks its dependents. *Alternative, not chosen:* stack
the dependent's branch on its dependency's branch and merge them all at the
end. It's faster, but a rejected dependency invalidates everything stacked
on it.

### 5.5 Fixing the wake (also fixes plain bus messages)

`WakeSession` is kept for steering: operator messages, nudges and
`send_message(wake = true)` between peers.

- **Write it in two steps:** first the text as a bracketed paste
  (`paste_bytes`), then `\r` as a separate write after `SUBMIT_DELAY` (150 ms).
  `pending_bus_wakes` gets a `submit_at` field, and the tick flushes it.
- **A NeedsAttention session isn't sent the wake.** The wake stays queued,
  and Needs you lists the session as `prompt`.
- **A queued wake gets a TTL** (10 min). When it expires, the message stays
  unread on the bus and Activity records the drop.
- **Probe each harness before relying on this.** For claude, codex, opencode
  and agy, check that paste plus a delayed `\r` submits, using an isolated
  `HOME` and recording with `AGENT_MUX_PTY_LOG`. Record the results in the
  module docs. Run the probe only with the operator's go-ahead, since it
  launches real CLIs.

### 5.6 Questions and new work from workers

- **Questions:** a `question` sent to `user`, or to the old `coordinator`
  address, appears in Needs you right away. The operator answers on the
  `>` command line. The reply carries `reply_to` and wakes the worker.
- **Delegation inside a swarm:** `agent_mux_task_split` or
  `agent_mux_delegate` called by a swarm worker doesn't spawn anything. It
  adds a task with `swarm_tasks.proposed = 1`, which appears in Needs you as
  `proposed`. With the plan gate on, the operator accepts it (`y`) or drops
  it (`x`); with the gate off, it is accepted automatically. After that, the
  dispatcher schedules it like any other task.

## 6. Integrating and finishing

Unchanged from the 2026-10-30 design, but per task:

- **Merge or set aside:** `y` merges a `review` task's branch with
  `merge_branch` (`--no-ff`, aborted on conflict), and `x` sets it aside
  (recorded in `swarm_reviews`).
- **Finished:** once every task is terminal and every branch reviewed, the
  board header shows a summary:
  - done, failed and cancelled tasks
  - merged commits and branches set aside
  - time, tokens and cost, workers included, from the traced launches
- **Close:** `y` in `finished` closes the swarm.
- **Optional final check:** a `check` command can be set in the launch form
  (for example `cargo test`). agent-mux runs it in the workspace when the
  last branch is merged and shows its exit status and the end of its output.
  It isn't a model run.

## 7. Data model (schema v21)

New tables. `swarm_state` and `swarm_plans` are left unused, and the
migration ends any open swarm: open tasks cancelled, leases released.

```sql
CREATE TABLE swarms (
  id              TEXT PRIMARY KEY,           -- swarm-<short>
  workspace       TEXT NOT NULL,
  goal            TEXT NOT NULL,
  status          TEXT NOT NULL CHECK (status IN
                  ('planning','review','running','paused','integrating','finished')),
  planner_harness TEXT NOT NULL,
  planner_launch  TEXT,                       -- the latest planner run
  plan_summary    TEXT,
  plan_error      TEXT,
  max_workers     INTEGER NOT NULL,
  isolation       TEXT NOT NULL CHECK (isolation IN ('shared','worktree')),
  gate            INTEGER NOT NULL DEFAULT 1,
  check_cmd       TEXT,
  check_result    TEXT,
  started_ns      INTEGER NOT NULL,
  updated_ns      INTEGER NOT NULL,
  ended_ns        INTEGER
);
CREATE UNIQUE INDEX swarms_live ON swarms (workspace) WHERE ended_ns IS NULL;

CREATE TABLE swarm_tasks (
  task_id         TEXT PRIMARY KEY REFERENCES agent_tasks(id),
  swarm_id        TEXT NOT NULL REFERENCES swarms(id),
  plan_key        TEXT NOT NULL,
  position        INTEGER NOT NULL,           -- plan order, the board's order
  depends_on_json TEXT NOT NULL DEFAULT '[]', -- plan keys
  acceptance      TEXT,
  proposed        INTEGER NOT NULL DEFAULT 0,
  attempts        INTEGER NOT NULL DEFAULT 0,
  nudged_ns       INTEGER,
  UNIQUE (swarm_id, plan_key)
);
```

- **One live swarm per workspace.** A partial unique index enforces it, and
  old swarms stay for history.
- **Refresh:** `trace_changes` triggers on both tables keep the
  refresh-on-change rule.
- **Plan gate:** `plan_blocks` reads `swarms.status`, and claims stay
  refused outside `running`.

## 8. The screen

### 8.1 Layout

```
┌ ◆ Ship three follow-ups · running · 1/3 done ██████░░░░░░ · 2/3 workers · 18m · 41k tok $1.24 ─────────┐
│ ⚑ NEEDS YOU (1)  ◷ "F1 search" asks: match section titles case-insensitively?  2m   ! to act            │
│┌ Plan (3) ─ planner: claude · "split by subsystem, no overlap" ─┐┌ F1 search · implementer·codex ────────┐│
││ ✓ 1 bus-fix      Bus: all_workspaces …     codex   12m  $0.31  ││ 1 Task  2 Live  3 Thread (2)  4 Changes ││
││▸◷ 2 f1-search    F1: search section titles  codex   6m  $0.12  ││                                         ││
││ ⋯ 3 lf-cost      Langfuse: trace cost   ← 1 claude      —      ││ …the task card, its terminal, its        ││
││                                                                 ││ messages or its diff                    ││
│└─────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────┘│
│ ↑↓ task  Tab panel  1-4 view  Enter attach  > message  space pause  a activity  F1 more                  │
```

- **Mission strip:** the goal, the swarm state, tasks done out of the
  total, live workers against the cap, elapsed time, and tokens and cost
  (planner and workers).
- **Needs you:** as today, with the kinds `question`, `prompt`, `stalled`,
  `lost`, `failed`, `review`, `proposed`, `plan` (review is waiting on you),
  `planner-failed` and `finished`.
- **Board** (left, about 55% wide):
  - one row per task in plan order: glyph, number, key, title,
    `← n` dependencies, harness (or `persona·harness` once spawned), elapsed
    time and cost
  - the planner's summary in the title
  - in `review`, `!`/`?` marks on rows with check errors or warnings
- **Panel** (right), on the selected task:
  - `1 Task` is the card. It's editable in `review`, and shows the result or
    error once the task ends.
  - `2 Live` is the worker's terminal.
  - `3 Thread` shows the task's messages.
  - `4 Changes` is the `git diff` (the worktree's, or the workspace's for a
    shared task).
  - The panel opens on the tab the state calls for: Thread for `asking`,
    Live for `working`/`stalled`/`prompt`, Changes for `review`, and Task
    otherwise.
- **Activity** (`a`): the swarm's events, which are planner runs, spawns,
  claims, nudges, leases, messages, merges and dropped wakes.
- **Empty state:** the launch card, with the goal, planner harness, max
  workers, isolation, plan gate and an optional check command.
- **While planning:** the board shows a spinner with the planner's last
  output line, and `2 Live` streams the planner's output.

### 8.2 Keys

| Key | Board | Notes |
| --- | --- | --- |
| `↑↓` `j/k` `g/G` | choose a task | |
| `Tab` / `Shift+Tab` | panel ↔ board | |
| `1`–`4` | panel tab | remembered per task |
| `Enter` | attach to the worker (running) · edit the card (review) | |
| `y` | approve the plan · merge a review · accept a proposal · close a finished swarm | confirms first, and `y` alone confirms |
| `p` | re-plan with notes | review only |
| `n` / `e` / `D` | add, edit or delete a task | add and edit at any time (an added task starts if it's ready); delete only for tasks that haven't started |
| `r` | retry a failed or lost task · start a swarm when none is live | |
| `x` | stop the worker · cancel a task · set a review aside · drop a proposal | `y` confirms |
| `space` | pause or resume dispatch | |
| `>` `m` `b` | command line (unchanged: `[!] [@who] [#topic] body`) | `@coord` is gone. `@all` and task keys (`@f1-search`) address workers |
| `!` | focus Needs you | |
| `a` | Activity | |
| `Ctrl+R` | reload | |
| `Esc` | back, then close | |

All of these are registered in `keymap.rs` under the Swarm screen, with
their themes, so F1 and Help list them.

## 9. CLI and control socket

```sh
agent-mux swarm start "GOAL" [--planner claude] [--workers 3] [--isolation worktree] [--no-plan-gate] [--check "cargo test"]
agent-mux swarm status [--json]       # swarm, board with shown states, needs
agent-mux swarm plan [--json]         # the board and check problems
agent-mux swarm approve
agent-mux swarm pause | resume
agent-mux swarm retry <task|key>
agent-mux swarm merge <task|key> | set-aside <task|key>
agent-mux swarm broadcast "…" [--urgent]
agent-mux swarm stop
```

- **Control requests:** `SpawnCoordinator` is replaced by
  `StartSwarm { workspace, goal, planner, max_workers, isolation, gate, check }`.
  `SwarmAction { workspace, action }` covers approve, pause, resume, retry,
  merge and set-aside, so the CLI and the screen take the same path.
- **Without a TUI:** the commands that only change rows (approve, pause,
  set-aside) work and say that dispatch waits for the TUI. `start` still
  needs a running TUI.

## 10. What goes away

- The coordinator session:
  - `compose_coordinator_prompt`, `PLAN_GATE_PROMPT`,
    `STANDBY_WORKER_INSTRUCTIONS` and standby workers
  - `SpawnCoordinator`, and the `plan`, `plan-approved` and `plan-changes`
    messages
  - the `c` (make coordinator) and `w` (wake) keys
- `swarm_state` and `swarm_plans` (left in the schema, unused) and
  `set_coordinator`.
- The agent-first lanes (`SwarmSnapshot::lanes`, `LaneState`). The board's
  `TaskView` replaces them, and `member_label` naming stays for workers.

`agent_mux_delegate` and the bus remain for ad-hoc lead/worker work outside
a swarm.

## 11. Delivery

| PR | Contents | Visible result |
| --- | --- | --- |
| **1 Wake fix** | §5.5: two-step submit, NeedsAttention handling, TTL, the harness probe (with the operator's go-ahead) | `send_message(wake)` and operator messages reach idle Claude sessions |
| **2 Model + engine** | schema v21, `bus::swarm::plan` (parse, check), `engine::next_actions`, the `swarm-plan` skill, the planner run, `App::spawn_swarm_worker`, the worker prompt, the stall and lost handling, CLI start/status/plan/approve/pause/retry/stop | a swarm runs end to end from the CLI, and the old screen shows it read-only |
| **3 Screen** | §8: board, panel, Needs you kinds, review editing, keys and keymap, render tests at 80/120/180 columns | the new Swarm screen |
| **4 Integration + docs** | per-task merge unblocking dependents, the proposed tasks, the final check, the summary; `docs/swarm.md`, `docs/bus.md`, AGENTS.md; old code removed | the swarm feature is complete |

**Tests** use an isolated `HOME` and DB. The planner and workers are fake
harness scripts like the existing fake-claude ones. The engine is tested
with pure snapshot tables, never with real CLIs.

## 12. Open questions

1. **Worktree dependencies:** wait for the merge (§5.4, proposed) or stack
   branches?
2. **Nudges:** one, then Needs you (proposed), or none?
3. **Stopping a finished worker:** after 30 s (proposed), or keep it until
   the swarm closes so it can still be asked follow-ups?
4. **Planner harness default:** the first installed CLI (proposed), or
   always the harness of the selected session?
