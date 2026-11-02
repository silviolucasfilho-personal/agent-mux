# Swarm: the Agents layout and a coordinator agent — design

Status: draft for review, 2026-11-02. Replaces §8 (the screen) of
`2026-11-02-swarm-orchestrator-design.md` and amends its §4 (planning) and
§5.6 (questions). The engine (§5), the store (§7) and the CLI (§9) stay.

Operator decisions (2026-11-02):
- **Placement:** the Swarm screen stays a screen of its own, built from the
  Agents screen's parts, and a swarm that needs the operator shows in the
  Agents sidebar.
- **Coordinator:** each swarm has a coordinator **agent**, a live session you
  can attach to and talk to. agent-mux still dispatches.
- **Fixes in scope:** the needs badge, a workers' harness and per-task
  edits, the agy agent tools, planner progress, and honest cost.

## 1. What the first real run showed

The swarm "How to add grok to agent-mux" (`swarm-5d2d0f16`, 2026-11-02) ran
under the board screen of PR #104. Its board, drawn at 140×34 from a copy of
the store, and its rows showed:

| # | Finding | Cause |
| --- | --- | --- |
| 1 | `grok-spec` waited 4 h on a question nobody saw | Needs you lives only on the Swarm screen; the Agents sidebar and status bar say nothing |
| 2 | `grok-spec` (agy, agent `doc-writer`) had no write or shell tools; `harness-core` (agy, no agent) wrote code | agy with a custom agent loses tools; the mapping was never probed, and the plan check cannot see it |
| 3 | Every task ran on agy | Tasks without a harness inherit the planner's, and the planner was the selected session's harness |
| 4 | The stuck task could not be moved to another harness | The task form edits title, instructions, files, dependencies and acceptance only |
| 5 | "The planning session is freezing" | A print-mode planner shows nothing until it ends |
| 6 | Live terminals clipped to about 60 columns | The panel is about 45% of the width, while the worker's PTY is sized for the main pane |
| 7 | Two hint bars that disagree | The screen draws its own footer above the global one |
| 8 | "Question regarding task-b1bce2a4e1 (grok-spec):" as the question | The need shows the first line, and the worker's first line was boilerplate |
| 9 | `$0.00` for agy | agy's cost is unpriced, and the strip sums it as zero |
| 10 | Swarms of other folders were invisible | The screen opens on one workspace |

## 2. The screen

### 2.1 Layout

The Swarm screen (`Mode::Swarm`) uses the Agents screen's geometry: the
sidebar at `ui::SIDEBAR_WIDTH` on the left, the main pane on the right at
`ui::PANE_ORIGIN`, and the global status bar as its only footer. Sessions
are already sized to the main pane, so a worker's terminal draws at its own
size, uncut.

```
┌ Swarms ────────────────────┐┌ grok-spec · agy · asking ───────────────────────────────────┐
│ ▾ ◆ add grok   ⚑2   1/3    ││                                                             │
│     ★ coordinator·claude ● ││   the selected row's terminal, full size (the main pane),   │
│     ◷ 1 grok-spec · agy  ! ││   or its card when it has no live terminal                  │
│     ⇡ 2 harness-core     ! ││                                                             │
│     ⋯ 3 workflows-and-…    ││                                                             │
│ ▸ ◆ fix the bus (done)  ✓  ││                                                             │
│                            ││                                                             │
│ + new swarm  n             ││                                                             │
└────────────────────────────┘└─────────────────────────────────────────────────────────────┘
[F1 Commands] Swarm · ↵ attach · y approve/merge · > message · ! needs you · Esc back
```

### 2.2 The tree (`swarm_tree::rows`)

| Row | Depth | Shows | Selected, the pane shows |
| --- | --- | --- | --- |
| `◆` swarm | 0 | short goal, `⚑n` when it needs you, done/total | the **swarm card** (§2.3) |
| `★` coordinator | 1 | `coordinator·<harness>`, its session state | its terminal |
| task | 1 | state glyph (`engine::shown`), number, key, worker harness, `!` when it needs you | its worker's terminal when live, else the **task card** |
| attempt | 2 | one row per worker session of the task (folded by default) | that session's terminal |

- **Which swarms:** every live swarm in the store, from every workspace,
  then the swarms that ended in the last 7 days, folded.
- **Order:** the swarm opened from a session's folder comes first and is
  selected.
- **Folding:** `←` and `→` fold and unfold, as in the Agents list.
- **Rendering:** rows reuse the Agents list's row drawing, with the same
  glyph and badge columns and the same fold arrows.

### 2.3 The cards (`ui::swarm_cards`)

- **Swarm card:**
  - goal, state, workspace, elapsed time, workers against the cap, tokens and cost
  - Needs you, the full list (§2.4)
  - plan summary, check result
  - the swarm verbs: `y` approve, `p` re-plan, `space` pause, `y` close when finished
- **Task card:**
  - its sections in order: Task (instructions, done-when, files,
    dependencies), Result or Error, Branch and review, Thread, Changes
  - `1`–`4` jump to a section
  - the plan's problems at the top
  - its verbs: `e` edit, `r` retry, `x` stop or cancel, `y` merge, `x` set aside

### 2.4 Needs you

- **Badges, not a strip:** a task that needs the operator carries `!` on its
  row, and its swarm carries `⚑n`.
- **The list:** the swarm card lists them all, most urgent first. The kinds
  are unchanged (`swarm_board::compute_needs`).
- **Outside the Swarm screen:** when any live swarm needs the operator, the
  Agents sidebar shows one row, `⚑ swarm needs you (n)`, above the History
  box, and the status bar shows `⚑ swarm n`. `Enter` on the row opens the
  Swarm screen on the first need.
- **Question text:** a question's line shows its first line that is not a
  header (it skips lines matching `^question regarding` and lines that end
  with `:`). The worker prompt asks for the question itself on the first
  line.

### 2.5 Keys

| Key | Does |
| --- | --- |
| `↑↓` `j/k` | choose a row |
| `←→` | fold or unfold |
| `Enter` | focus the pane: type into the terminal, as on the Agents screen (F1 and the focus key come back) |
| `y` | approve the plan (swarm row) · merge a branch (task) · accept a proposal · close a finished swarm |
| `p` | re-plan with notes |
| `n` | new swarm (a swarm row or empty space) · new task (inside a swarm) |
| `e` | edit a task (§4) |
| `d` | delete a task that never started |
| `r` | retry a task |
| `x` | stop a worker · cancel a task · set a branch aside · drop a proposal |
| `space` | pause or resume |
| `>` `m` | message: the selected row's session, `@coord`, `@all`, `@<task key>` |
| `!` | jump to the next row that needs you |
| `1`–`4` | a task card's section |
| `Esc` | back to Agents |

The keys are the local verbs of `docs/keyboard.md` (n, e, d, x, r) and are
registered in `keymap.rs`, so F1 and Help list them.

## 3. The coordinator agent

agent-mux keeps the state machine (`engine::next_actions`). The coordinator
is a live session that plans, advises and answers. It never needs a wake to
make the swarm progress.

### 3.1 It plans

- **Start:** the swarm starts with the coordinator, an interactive session in
  the workspace, on the coordinator harness (the launch form's **Coordinator**,
  default Claude when installed, §4.1). The built-in `coordinator` agent comes
  back, rewritten for this role.
- **Opening prompt:** the goal and the planner context (as in the old §4.1),
  and the instruction to submit the plan with the new MCP tool
  `agent_mux_swarm_plan(plan = {summary, tasks})`.
- **The tool's answer:** it runs `plan::check`.
  - **Errors:** the tool answers them, and nothing goes on the board. The
    coordinator fixes them itself and submits again.
  - **No errors:** the plan goes on the board (`record::replace_draft`), and
    the swarm moves to `review` (or `running` with the gate off).
- **What you see:** the coordinator plans in its own terminal, which you can
  watch and attach to. This replaces the headless planner run (finding 5).
  The `swarm-plan` skill becomes the coordinator's planning reference.
- **Re-plan:** `p` sends the coordinator a message with your notes (a wake,
  PR #102). It submits again, and the plan check keeps the tasks that ran.
- **If it never submits:** after `PLAN_TIMEOUT` (15 min) the swarm card says
  so and offers `p` (nudge it with notes) or `x` then `r` (restart the
  coordinator).

### 3.2 It advises

- **Questions go to the coordinator first.** A worker asks `to = "coordinator"`
  (the worker prompt says so), and agent-mux wakes the coordinator. The
  coordinator answers, or forwards the question to `user` with its own
  recommendation.
- **When it escalates:**
  - a question to `user`
  - a question to the coordinator that stays unanswered for `STUCK_AFTER`
    (5 min)
- **Events it is told about:** agent-mux sends it one short bus message per
  event, a task done, failed or lost, or a proposal. It is woken only if it
  is idle and the swarm is not paused. It may answer with advice to the
  operator or with proposals (`agent_mux_task_split` → `proposed`).
- **What it cannot do:**
  - approve its own plan
  - start workers
  - merge
  - close the swarm

  Those stay with the operator and agent-mux (`plan_blocks`, and the verbs).
- **If it exits:** the swarm keeps running. Its row shows `exited`, and `r`
  on it restarts it with a summary of the board as its opening prompt.

## 4. Workers' harness and per-task edits

### 4.1 Launch form

| Field | Default |
| --- | --- |
| Goal | — |
| Coordinator | Claude when installed, else the selected session's harness |
| Workers | the coordinator's harness |
| Max parallel workers | 3 |
| Default isolation | shared |
| Approve the plan before workers start | yes |
| Check when done | — |

- **Store:** `swarms.worker_harness` (schema v22) holds the default for tasks
  that name none. The planner context lists it as `default_harness`.
- **Retiring a field:** `planner_harness` becomes the coordinator's harness
  (`coordinator_harness`, renamed in the migration).

### 4.2 Task form

`e` edits the title, instructions, done-when, files and dependencies, and
also the **harness**, **agent** and **isolation**, for a task that has not
started or is about to be retried. A task that is running shows those three
read-only. Retry (`r`) on a failed, lost or stalled task opens the form with
them editable, so a stuck agy task moves to Claude in one step (finding 4).

## 5. agy with a custom agent (finding 2)

- **Probe:** with the operator's go-ahead, as for the wake fix.
  - Launch agy 1.4.12 with `--agent` and a managed agent file whose
    `tools = ["read", "edit", "shell"]`.
  - Check which tools the model is offered: ask it to list its tools, then
    to write a file in a temporary folder.
  - Record the result in `src/agents/launch.rs`'s module docs and in memory.
- **If the mapping is fixable** (the agent file's tool names or its format),
  fix `agents::launch::plan` for agy.
- **Otherwise,** `plan::check` gives an **error** for an agy task with an
  agent whose tools include `edit` or `shell`: "agy loses write tools under a
  custom agent; drop the agent or use another harness". The task form shows
  the same error.

## 6. Planner progress and honest cost (findings 5, 8, 9)

- **Progress:** the coordinator's terminal replaces the blank print-mode
  pane. While the swarm is `planning`, its card also shows how long planning
  has run, out of `PLAN_TIMEOUT`.
- **Cost:** a harness whose cost is unpriced (`trace_stats.total_cost_usd`
  NULL, or `unpriced_generations > 0`) shows `—` in cost columns, and the
  swarm's total shows `≥ $x` when only part of it is priced.
- **Questions:** §2.4.

## 7. Data model (schema v22)

```sql
ALTER TABLE swarms RENAME COLUMN planner_harness TO coordinator_harness;
ALTER TABLE swarms ADD COLUMN worker_harness TEXT;      -- NULL: the coordinator's
ALTER TABLE swarms ADD COLUMN coordinator_launch TEXT;  -- the live coordinator
```

`ADD COLUMN` isn't replay-safe in SQLite, so the migration checks
`pragma_table_info` first, the way earlier migrations guard their changes.

## 8. Delivery

| PR | Contents |
| --- | --- |
| **A: the screen** | The Agents layout (§2), the tree and cards, the keys and keymap, the Needs badge in the Agents sidebar and status bar (§2.4), question text, honest cost. Render tests at 80/120/180 columns, against a store fixture like `swarm-5d2d0f16`. |
| **B: the coordinator** | Schema v22, `agent_mux_swarm_plan`, the coordinator launch and its built-in agent, plans submitted through the tool with errors answered, questions and events routed to it, restart, the plan timeout. The headless planner path removed. End to end with a fake coordinator that submits a plan with an error, then a good one. |
| **C: harness and agents** | The workers' harness and per-task edits (§4), the agy probe and its fix or check (§5). |

## 9. Open questions

1. **Coordinator per swarm, or one per workspace** that outlives swarms? A
   per-swarm coordinator is proposed: it knows one goal.
2. **Coordinator cost:** an idle interactive session costs nothing, but every
   event message that wakes it costs a turn. Should events be batched (at
   most one wake per minute, proposed) or sent only on demand?
3. **`n` inside a swarm:** a new task (proposed), or ask the coordinator to
   add one?
