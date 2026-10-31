# Swarm Mission Control (`Mode::Swarm`, second layout) — design

Status: P1–P4 implemented, 2026-10-30. Replaces the four-quadrant layout of
`2026-10-29-agent-swarm-screen-design.md` section 3; the data model, the bus
and the CLI of that spec stay.

## 1. Why

The first layout drew the swarm as the database draws it: one quadrant per
table (agents, tasks and leases, messages) plus a preview. Rendered against a
two-worker swarm at 120 and 180 columns it showed five problems:

1. **One unit of work is split across three panes under three names.**
   "W2 wires the bus store" was `W2` in the roster, `[task-946] … → W2 (co` on
   the board and `W2 (codex) → ★ Coord` in the feed.
2. **It does not answer what the operator opens it for.** Is the swarm
   progressing, does anything need me, is anyone stuck? A worker idle on a
   running task with an unanswered question was one truncated feed line, an
   `○ idle` and a `✉ 1` badge, in three places.
3. **Most of the screen is empty or behind a toggle.** The preview said
   "Press v to view its Task Detail"; leases hid behind `l`, feed filters
   behind `f`, preview modes behind `v`; every quadrant was mostly blank.
4. **Names were positions.** `W1`, `W2` were assigned in roster order on every
   refresh (`bus::swarm::load_snapshot`), so an exit renumbered everyone and
   "W2" in an old message could mean another agent.
5. **Ten verbs in the footer, four more in the hint bar,** a header that
   truncated at 120 columns, and columns such as `1t · 1.0k`.

## 2. Model: a mission with lanes

From the operator's chair a swarm is

> **mission** (goal) → **plan** (tasks) → **lanes** (one agent on one task) →
> **conversation** (questions, contracts, handoffs) → **result** (changes to
> review and merge)

and the operator has five jobs: watch progress, unblock, steer, step into one
agent, accept the result. The screen is organised around them, and its unit is
the **lane**.

### 2.1 Lanes (`bus::swarm::Lane`, `SwarmSnapshot::lanes`)

A lane is derived, never stored:

| Lane | Source | Shown |
| --- | --- | --- |
| coordinator | `snapshot.coordinator` | pinned first, `★`, the plan line: tasks and open questions |
| agent | each worker member | its current task (latest non-terminal assigned task, else its latest task), or none |
| queued | each open task with no assignee | `◦ queued`, its title, `r` delegates it |

Order: coordinator, then agents in the order they joined (first task
`created_ns`, then launch id), then queued tasks oldest first. The order does
not change with state, so the cursor does not jump; the selection is kept by
the lane's key (launch id, or task id for a queued lane), not its index.

**Names are stable.** A lane is named `persona·harness` (`implementer·codex`),
the persona being the session's agent, else its task's `target_agent`, else
`worker`. When two lanes share a name, each gets its launch id's first four
characters (`implementer·codex 8f31`); the suffix drops once its twin has
left. A name never passes to another agent, which is what positional `W2`
did. The coordinator is `coordinator·<harness>`; a launch that is no longer a
member shows as its short id.
Messages, the Thread and the Activity use the same names
(`SwarmSnapshot::member_label`).

**State** (`LaneState`), first match wins:

| State | When | Glyph |
| --- | --- | --- |
| `Failed` | its task is `failed` | `✗` |
| `Lost` | its task is running but its session exited | `!` |
| `Waiting` | it asked a question nobody answered (2.2) | `◷` |
| `Working` | its session is working | `●` |
| `Done` | its task is `done` | `✓` |
| `Idle` | its session is idle with a non-terminal task | `○` |
| `Ready` | live, no task | `◦` |
| `Exited` | session exited, no running task | `·` |
| `Queued` | a queued lane | `◦` |

**Activity line**: what the lane is doing now, from data already collected:
`running <tool>` (the session's `running_tool`), `waiting 4m on <name>`,
`done · <first line of the result>`, `failed · <first line of the error>`,
`session exited`, `idle`.

### 2.2 Open questions (`SwarmSnapshot::open_questions`)

A message with topic `question` from A is answered when a later message names
it in `reply_to`, or when a later message goes to A from the question's
addressee (anyone, for a broadcast question). Replies sent from the screen set
`reply_to`. An unanswered question makes its sender's lane `Waiting`, counts on
the coordinator's plan line, and (P2) lands in **Needs you** after
`STUCK_AFTER`.

## 3. Layout

```
┌ ◆ Ship auth + UI in parallel ─── executing · 3/6 done ███████░░░░ · 3/4 workers · 18m · 41k tok $1.24 ┐
│ ⚑ NEEDS YOU (2)  ▸ implementer·codex asks "Should cancelled tasks release worktrees?"  4m  [a]nswer  │  P2
├─ LANES ────────────────────────────────────────┬─ implementer·codex · Wire bus store queries ───────┤
│ ★ coordinator·claude   ● running task_board     │ 1 Live   2 Thread (3)   3 Changes   4 Task         │
│   plan: 6 tasks · 1 open question               │ $ cargo test --lib bus::store                       │
│ ● architect·claude     ● running Edit           │ running 14 tests                                    │
│   Add swarm state      shared · 2 leases        │ …                                                   │
│ ◷ implementer·codex    waiting 4m on coordinator │                                                     │
│   Wire bus queries     wt bus-946               │                                                     │
│ ✗ tester·agy           failed · exit 101        │                                                     │
│ ◦ queued               Write migration          │                                                     │
├─────────────────────────────────────────────────┴─────────────────────────────────────────────────────┤
│ ↑↓ lane  Tab panel  1-4 view  Enter attach  m message  b broadcast  n new worker  r run  x stop  a activity │
└──────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Mission strip** (top border): goal, phase, a progress bar of tasks
  done/total, workers against `max_workers`, elapsed since `swarm_state`'s
  `started_ns`, tokens and cost. The workspace path is the window's, not the
  strip's. **Phase**: `not started` (no coordinator, no tasks), `planning`
  (a live coordinator, no tasks), `executing` (any task not terminal),
  `wrapping up` (all terminal, coordinator live), `done` (all terminal,
  coordinator gone).
- **Lanes** (left, 44% width, at least 36 columns): two lines per lane —
  glyph, name, activity; then task title and placement (`shared · 2 leases`,
  `wt <branch>`).
- **Focus panel** (right), on the selected lane, tabs `1 Live`, `2 Thread (n)`,
  `3 Changes`, `4 Task`:
  - *Live*: the session's terminal; for an agent in another process, the
    Thread instead (there is no terminal to show).
  - *Thread*: only this lane's messages (from it, to it, broadcasts), as a
    conversation: time, sender → recipient, topic, the whole body wrapped.
    `↑↓` choose a message, `Enter` opens it, `m` replies to it.
  - *Changes*: `git diff` of the lane's worktree (or the workspace for a
    shared lane), with its `--shortstat` on the tab.
  - *Task*: the task card (instructions, files and leases, result or error).
  The tab opens on what the lane's state calls for: Thread when `Waiting`,
  Changes when `Done` in a worktree, Task when `Failed`, `Queued` or `Lost`,
  Live otherwise; choosing a tab sticks until the lane changes.
- **Activity** (`a`): the right panel shows the whole swarm's messages
  instead of one lane's (P2 adds events: claimed, lease taken, done, failed,
  spawned, refused by the cap).
- **Empty state**: with no lanes, the right panel is the launch card, not a
  hint.

## 4. Keys (`docs/keyboard.md`)

| Key | Lanes focused | Panel focused |
| --- | --- | --- |
| `↑↓` `j/k` `g/G` | choose a lane | scroll, or choose a message in Thread/Activity |
| `Tab` / `Shift+Tab` | to the panel | to the lanes |
| `1`–`4` | panel tab | panel tab |
| `Enter` | attach to the lane's session | Thread/Activity: open the message |
| `m` | message the lane | reply to the chosen message |
| `b` | message everyone | — |
| `n` | new worker (a task, spawned at once; past the cap it stays queued) | — |
| `e` | edit the lane's task | — |
| `r` | queued lane: delegate; else start a swarm (asks when one is live) | — |
| `x` | stop the lane's session; queued lane: cancel the task (`y` confirms) | — |
| `d` | release the lane's leases (`y` confirms) | — |
| `a` | Activity on/off | Activity on/off |
| `Ctrl+R` | reload | reload |
| `Esc` `q` | close | back to the lanes |

`c` (make the lane the coordinator) and `w` (wake it) keep working but leave
the footer: they are rare and a message already wakes. `v`, `l` and `f` are
gone: the panel's tabs, the lanes' leases and the Thread replace them.

## 5. Phases

| Phase | Scope |
| --- | --- |
| **P1** | Lanes with stable names and states, the mission strip, the focus panel with its four tabs, Activity, the keys above, replies carry `reply_to`. |
| **P2** | **Needs you**: a session waiting on a prompt, questions to the operator (at once) or unanswered for `STUCK_AFTER`, failed tasks (`r` retries: re-open and delegate, reusing the task's worktree), lost tasks (session exited mid-task, also retryable), stuck lanes (a non-terminal task whose session has printed nothing for `STUCK_AFTER` with no tool running), the cap reached while tasks are queued, finished worktree lanes waiting for review. `!` focuses the strip, `Enter` acts, `d` dismisses for the screen's lifetime. Activity gains task and lease events. Spawns refused by the cap are not events: the refusal is not recorded anywhere but the response, and the cap need covers it. |
| **P3** | A `>` command line replaces the composer form: `@coord`, `@all`, `@<lane>` (separator-blind prefix or launch id; default: the selected lane, or the asker for a reply), `Tab` completes `@`, a leading `!` marks it urgent and sends it with topic `urgent` (the form's Urgent toggle only changed the wake prompt, so the hook never delivered it mid-turn), wake is always on, the topic is inferred (a reply to a question is `answer`, else `directive`) unless `#topic` names one, and a preview line says what Enter will do or why it cannot. |
| **P4** | Phases become behaviour: the **plan gate** (`swarm_plans`, schema v19: the coordinator puts its tasks on the board and sends a `plan` message; `SpawnWorker` and `claim_task` refuse until the operator approves with `y`, or sends it back with `e` and notes; standby workers spawn on approval), an *integrating* phase for finished worktree branches (`y` merges with `git merge --no-ff`, aborted on conflict; `x` sets aside; `swarm_reviews`), and a finished state with a summary on the coordinator's Plan tab (`y` closes the swarm: sessions stopped, open tasks cancelled, leases released; `d` keeps it). `agent-mux swarm stop` now stops the sessions through the TUI (`ControlRequest::StopSwarm`). |

### Decisions

- `STUCK_AFTER` = **5 minutes** (operator, 2026-10-30). One constant in
  `bus::swarm`, used by both stuck lanes and unanswered questions.
- The **plan gate is on by default** (operator, 2026-10-30): the launch dialog
  has "Approve plan before workers start" checked; unchecking it restores
  today's behaviour. The flag is stored on `swarm_state` (P4 migration) so the
  coordinator's prompt, `agent_mux_delegate` and `SpawnWorker` all see it:
  until the plan is approved, `SpawnWorker` refuses with "plan awaiting
  approval" exactly as it refuses past the cap.
