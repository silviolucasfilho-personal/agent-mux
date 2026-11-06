# Swarms: one goal, many agents

A **swarm** splits one goal into tasks that agents work on in parallel in the same workspace. A swarm is a **conversation with a coordinator**: an agent session on the harness you pick, which talks the goal over with you, proposes the plan as a table, starts the workers once you say go, answers their questions and summarises at the end. agent-mux **supervises** it: it tracks every worker, reminds an idle one, notices a lost one, tells the coordinator what happened, and starts a ready task itself if the coordinator has not launched it two minutes after being told. Workers can be Claude Code, Codex, Antigravity or OpenCode sessions, in any mix. You watch and steer the swarm from the **Swarm** screen. This guide shows how to use it. How it works inside (tables, control requests, the dispatcher) is in [bus.md](bus.md#swarm-mission-control-modeswarm-agent-mux-swarm-), and the design is in the [coordinator-first spec](superpowers/specs/2026-11-05-swarm-coordinator-first-design.md), on top of [the orchestrator](superpowers/specs/2026-11-02-swarm-orchestrator-design.md) and [the screen](superpowers/specs/2026-11-02-swarm-agents-layout-design.md).

## In one minute

1. Select a session in the folder you want to work on, press `F1` and choose **Open swarm**, then `n` (or `Enter` on the empty screen).
2. Pick the coordinator's harness and press `Enter`. Its session opens with the keyboard in it: tell it the goal.
3. The coordinator asks what it needs, then shows the plan as a table and asks whether to start. Say **go**, or tell it what to change ("move task 2 to codex", "drop the docs task"). The screen mirrors the plan read-only; it never approves it.
4. Workers start, each on one task. Select a task to see its worker's terminal, and press `Enter` to type into it. `!` jumps to whatever needs you, and `>` writes to workers. The coordinator reports as tasks finish; when nothing has happened for five minutes it checks in and asks whether to keep waiting.
5. When everything is settled the coordinator summarises and asks which branches to merge. Tell it, or press `y` on a task row yourself.
6. When you say to close, it closes the swarm (or `y` on the swarm row).

Anything that needs you also shows outside the Swarm screen, as `⚑ swarm needs you (n)` in the Agents sidebar and `⚑ swarm n` in the status bar.

## Before you start

- **The workspace** is the folder of the session selected when you open the screen (otherwise the folder agent-mux started in).
- **The workers need the bus tools.** agent-mux attaches its MCP server to every session it launches, so this works out of the box. If you set `[agents] mcp = "off"` in `profiles.toml`, workers can't report that they're done.
- **Worktree isolation needs a git repository.** With **shared** isolation, workers edit the same files, protected by file leases. With **worktree** isolation, each task gets its own branch under `.loop-worktrees/`.
- **Several swarms per workspace.** A new swarm starts beside a running or stuck one. Verbs, messages and closing act on the swarm the cursor is in; on the CLI, name one with `--swarm ID` when the folder runs several.

## Starting a swarm

Press `n` on a swarm row (or `Enter` on the empty screen). The dialog asks one thing, **which harness coordinates** (`←`/`→`; Claude when installed, else the selected session's), and `Enter` starts it. The coordinator's session opens with the keyboard in it: say the goal there, as you would to any agent. Everything else — the workers' pool, how many run at once, isolation, a check command — is talked over in the chat and shows in the plan you confirm.

**Advanced** (`Tab` to the fold, `Space`) unfolds what you may want to fix in advance; anything set there is a limit the coordinator cannot exceed:

| Field | Meaning | Default |
| --- | --- | --- |
| Goal | Optional here. With a goal the swarm is **headless**: the coordinator plans it without asking you anything first (what `agent-mux swarm start "<goal>"` does). | — |
| Workers may run on | The workers' pool: tick each harness workers may use (`←`/`→` move, `Space` ticks). The coordinator gives each task one of them, and a plan naming another is refused. Tasks that name none run on the coordinator's harness if it is ticked, else on the first ticked. Until you tick one, the pool follows the coordinator. | the coordinator's |
| Max parallel workers | How many tasks run at once. A finished worker no longer counts. | 3 (up to 8) |
| Default isolation | `shared` (leases on files) or `worktree` (one branch per task). | `shared` |
| Plan approval | `in the coordinator's chat` (you say go there) or `off` (a plan that passes its checks runs at once; the coordinator still shows you the table). The screen never approves a plan. | in the chat |
| Check when done | An optional command, such as `cargo test`. agent-mux runs it in the workspace once every task is settled and every branch reviewed, before the swarm reads `finished`. | — |

`Tab` moves between fields, `←`/`→` change an option, `Space` ticks a harness, `Alt+Enter` adds a line to the goal, and `Enter` (or `Ctrl+S`) starts. If the swarm can't start, the dialog stays open with the reason in red at its top.

**Any session can become a coordinator.** In a running session, invoke the `swarm-coordinator` skill (`/swarm-coordinator` on Claude Code and Antigravity, `$swarm-coordinator` on Codex, `load skill swarm-coordinator` on OpenCode) and ask for a swarm: the session records itself as the coordinator of a new swarm in its folder and the running agent-mux supervises it.

For example, Claude as coordinator with agy and Codex in the pool gives a plan whose tasks run on agy or Codex, as the coordinator judges fit. Tell it to move a task before you say go; move a task yourself with `r` when it fails or gets stuck; a harness chosen there joins the pool.

## The screen

The Swarm screen shows **one swarm**: its coordinator, the agents it started and their sub-agents, each a real terminal you can watch and type into. It opens on the swarm of the session you have selected (its coordinator or one of its workers), else on the swarm that needs you, else on the folder's newest live one; `n` starts another. Every swarm is also a group in the Agents sidebar (`◆ <goal>`, with `★ coordinator` and the workers under it), where `Enter` opens this screen on it. The layout is the Agents screen's: a tree on the left, the selected row in the main pane on the right.

```
┌ Swarms • Keyboard focus ───┐┌ ◆ add OAuth login ─────────────────────────────────────────────┐
│>▾ ◆ add OAuth login    ⚑1  ││  1 Board  2 Live (3)  3 Timeline  4 Log (41)  5 Activity (12)  6 Changes │
│     ★ coordinator·claude ○ ││                                                                 │
│     ✓ 1 oauth-schema       ││ running · 1/5 done · 2/3 workers · 18m · 41k tok · $1.24        │
│     ● 2 oauth-routes codex ││                                                                 │
│         ↳ write tests   1m ││ ── Needs you (1) ──                                             │
│     ◷ 3 oauth-ui         ! ││ question  oauth-ui asks: which provider first?                  │
│     ◦ 4 docs               ││ ── Plan ──                                                      │
│ + new swarm  n             ││ confirmed in the chat 12:31 “go”                                │
│                            ││ ✓ done      1 oauth-schema   Add the tables  claude             │
│                            ││ ● working   2 oauth-routes   Routes  codex ← oauth-schema       │
└────────────────────────────┘└─────────────────────────────────────────────────────────────────┘
[F1 Commands] Swarm • Keyboard focus [n] New [r] Retry [x] Stop
```

- **The tree:** the swarm's row (`⚑n` when it needs you, `done/total` otherwise, `✓` once closed), then:
  - **`★` coordinator:** its session state. `Enter` types into it: this is where the plan is talked over and confirmed.
  - **One row per agent the swarm started**, in plan order: its state glyph, number and key, and on the right its harness, elapsed time and cost (`!` when it needs you, `✗` when the plan has an error on it). `Enter` types into its terminal.
  - **`↳` sub-agents** under a worker (a Claude `Task` run, a Codex sub-agent), live ones always, ended ones when you unfold the worker with `→`. They have no terminal of their own: `Enter` goes to their worker's.
- **The main pane** shows the selected row:
  - **The swarm row** has six tabs (`1`–`6`): **Board** (the card: state, needs, the plan as a table with each task's live word, how the plan was confirmed and by whose words, the check), **Live** (every live terminal of the swarm at once, a tile each, the coordinator first; `Tab` into it, `↑↓←→` choose a tile, `Enter` connects to it), **Timeline** (one lane per agent — the swarm, `★` the coordinator, each worker, `↳` its sub-agents, the check last — time left to right from the swarm's start: `█` while the agent worked, `░` while its session lived, `?` while it waited on an answer, and marks for what happened — `▲` launch, `✓` done, `✗` failed, `◆` merge, `│` a message, `▤` a plan, `■` closed; `Tab` into it, `←`/`→` pan, `-`/`+` zoom, `0` the whole swarm), **Log** (the swarm's log, newest last: time, kind, who, what — every decision with the operator's words, every task offered, started, done or failed, every question and message, the check; `Tab` into it, `t` cycles the kinds — all, decisions, tasks, messages — `/` filters by text, `Enter` opens the row the line is about), **Activity** (every message of the swarm) and **Changes** (each worktree task's branch, then the workspace's diff).
  - **The coordinator** shows its terminal.
  - **A worker** shows its terminal while it has one; its tabs are `1 Task` (instructions, done-when, files, dependencies, result or error, branch), `2 Live`, `3 Thread` (its messages), `4 Trace` (its turns from the store, each with its tool calls, generations and sub-agents, duration, tokens and cost) and `5 Changes` (its diff). The pane opens on the tab that matters.
  - **A sub-agent** (`↳`) opens on the Trace tab with its own rows only; `Enter` goes to its worker's terminal.
  - **A worker whose session ended** keeps its row: `Enter` **reconnects** — the same harness resumes the same conversation in a new session on the task (Claude Code, Codex and OpenCode; agy cannot, `r` retries instead).
- **`Enter` on a terminal** gives it the keyboard: the pane's title says `Keyboard goes to …`. `⌘L` (`Ctrl+Shift+L`) brings the keyboard back to the tree.
- **Cost** reads `—` when a harness's cost isn't priced (agy isn't), and `≥$x` when only part of it is.

### What the task words mean

The Board shows seven words beside the glyphs; the glyph tells which finer state it is.

| Word | Glyphs | Meaning |
| --- | --- | --- |
| `waiting` | `·` draft · `⋯` blocked · `◦` ready | not started: the plan isn't confirmed, a dependency isn't done, or it waits for the coordinator to start it (agent-mux starts it itself two minutes after telling the coordinator) |
| `working` | `●` (`◌` starting) | its worker works |
| `asking` | `◷` | it asked a question nobody answered yet |
| `needs you` | `⚑` prompt · `○` stalled · `!` lost · `✗` failed | its worker waits on a prompt, stopped without reporting, exited mid-task, or failed |
| `done` | `✓` | done |
| `review` | `⇡` | done on a branch that waits for your merge |
| `off` | `–` | cancelled or set aside |

A task that depends on a worktree task starts only once that task's branch is merged, so it builds on the merged work.

## The plan

The coordinator reads the goal and the workspace and submits tasks, each with a key, instructions, a done-when line, files and dependencies, with the `agent_mux_swarm_plan` tool. agent-mux checks the plan, and answers the coordinator with any errors so it fixes them itself; only a plan without errors goes on the board:

- **Errors block approval:** a key used twice, a dependency that isn't in the plan or forms a cycle, a harness that isn't installed, an unknown agent, a worktree outside a git repository, or an **agy task with an agent that edits or runs commands**. agy runs a custom agent with read-only tools, so such a task can't do its work; drop the agent or pick another harness. The same pair is refused everywhere else too: the task form won't save it, `r` on such a task opens its retry form already moved to Claude (with a note saying why; clear the agent to keep agy), and agent-mux won't start such a worker, so the task fails with the reason instead of stalling.
- **Warnings don't:** two tasks that may run at the same time and touch the same files in a shared workspace, or more than 12 tasks.

The coordinator shows you the same plan as a table in its chat and asks whether to start. **You confirm there**: say go, or say what to change and it submits the plan again (tasks that already ran are kept). `y` on the screen never approves a plan: it puts the keyboard in the coordinator's chat. From a shell, `agent-mux swarm approve` does the same for scripts. In **review** the screen also lets you:

| Key | Does |
| --- | --- |
| `e` (or `Enter`) | edit the selected task: title, instructions, done-when, files, dependencies |
| `n` | add a task |
| `d` | delete the selected task |
| `p` | re-plan: write what should change and press `Enter`. The coordinator gets your notes and submits a new plan. Tasks that already ran are kept. |

Nothing starts before you confirm, and workers can't claim tasks. Once you have, the coordinator starts each task when its dependencies are done (`agent_mux_swarm_launch`); a task it has not started two minutes after agent-mux told it is started by agent-mux itself, and the log says so. Every decision — who confirmed, with which words — is recorded (`agent-mux swarm events`).

## What needs you

Each task that needs you carries `!` on its row, and its swarm carries `⚑n`. The swarm's card lists them all, most urgent first. `!` jumps to the next row that needs you, and that row's keys act on it.

| Item | When | On its row |
| --- | --- | --- |
| `question` | a worker or the coordinator asked you something, or a worker's question to the coordinator went unanswered for 5 minutes | `m` answers it |
| `prompt` | a worker waits on a prompt in its terminal (usually a permission) | `Enter` types into its terminal |
| `plan` | the plan waits for your approval | on the swarm: `y` approve · `p` re-plan |
| `coordinator` | the coordinator has not submitted a plan after 15 minutes | on the swarm: `p` nudges it with notes · on `★`: `x` stops it, `r` starts a new one |
| `failed` | a task failed | `r` retry |
| `lost` | a worker's session exited mid-task | `r` retry |
| `stalled` | a worker stopped without reporting, even after a reminder | `Enter` types into its terminal · `r` stops it, then `r` retries |
| `review` | a worktree task finished | `y` merge · `x` set aside |
| `proposed` | a worker proposed a new task | `y` accept · `x` drop |
| `check` | the final check failed | the swarm's card shows its output |
| `finished` | everything is settled | on the swarm: `y` close |

A worker that goes idle on an unfinished task gets one reminder after 90 seconds. Only if it stays idle does it show up as `stalled`.

**Retry** (`r`) opens the task's form on its harness, agent and isolation, so a task that got stuck on one harness can move to another. Saving it stops its worker if it still has one and puts the task back on the board, and agent-mux starts a new worker. A worktree task keeps its worktree, so the new worker continues the work already there.

## Writing to workers

Press `>` to open the command line. `m` opens it addressed to the selected task's worker (or, on a message, as a reply to its sender); `@all` addresses everyone.

```
[!] [@who] [#topic] message
```

| You write | It does |
| --- | --- |
| `@f1-search match the section title too` | to the worker of task `f1-search`. Any unique prefix works (`@f1`), and separators don't matter. |
| `@all stop editing src/bus/ for now` | to every worker |
| `looks good` | no address: to the selected task's worker, or to the asker when you're replying |
| `!@f1 stop, you're editing the wrong file` | **urgent**: the worker gets it at its very next tool call, even mid-task |
| `@lf-cost #api-contract v2 is final` | with an explicit topic |

- **Before you send:** the line above the input says what `Enter` will do (`→ f1-search · directive · wakes it if idle`), or why it can't (`@store could be store-queries or store-schema`).
- **Editing:** `Tab` completes `@` names (press again to cycle), and `Esc` cancels.
- **Delivery:** every message wakes an idle worker. Without `!`, a busy worker reads it when it next checks its messages.

## Keys

| Key | Does |
| --- | --- |
| `↑` `↓` `g` `G` | choose a row |
| `←` `→` | fold or unfold: a worker's ended sub-agents (`→` on it), `←` on a child climbs to its parent |
| `Enter` | type into the row's terminal · reconnect a worker whose session ended · connect to a Live tile · start a swarm when there is none |
| `Tab` | tree ↔ pane (in Activity or Thread, `↑`/`↓` choose a message, `Enter` reads it in full, `m` answers it; in Live, `↑↓←→` choose a tile; in Timeline, `←`/`→` pan, `-`/`+` zoom, `0` all; in Log, `↑`/`↓` choose a line, `t` the kinds, `/` a text filter, `Enter` opens the row it is about) |
| `1`–`6` | the swarm row's tab (Board, Live, Timeline, Log, Activity, Changes) or a worker's (Task, Live, Thread, Trace, Changes) |
| `y` | confirm the one thing on the row: merge a branch in `review` · accept a proposal · close a finished swarm. Never the plan: on a plan in review, `y` moves the keyboard to the coordinator's chat. |
| `x` | stop a worker · cancel a task · set a branch aside · drop a proposal (confirms with `y`) |
| `r` | retry a failed, lost, cancelled or stalled task (the retry form: harness, agent, isolation) · restart an exited coordinator |
| `space` | pause or resume: while paused, running workers continue but nothing new starts |
| `n` | a new swarm |
| `>` `m` | write to workers (`m`: the selected task's worker, or an answer to its question) |
| `!` | the next row that needs you |
| `Ctrl+R` | reload now (the screen refreshes on its own when anything changes) |
| `⌘L` | from a terminal, back to the tree |
| `Esc` | back to the tree, then close the screen |

Changing the plan (a task's card, adding or dropping one, re-planning) is said to the coordinator: `Enter` on `★`. Confirmations take `y` only. `Enter` never confirms. What an action did shows in the status bar.

## Reviewing and finishing

- **Worktree tasks** end on a branch (`loop/bus-…`). Each appears in Needs you as `review`, and the swarm reads `integrating` once nothing else is left.
  - `y` **merges** the branch into the branch your workspace has checked out, with a merge commit, after asking first. Git refuses if your uncommitted changes would be overwritten. A conflict aborts the merge and leaves your workspace as it was: resolve it by hand, then merge again.
  - `x` **sets the branch aside** without merging it. The branch is kept. Tasks that depend on it stay blocked.
- **Finished workers stay open.** You can attach to one or message it with follow-ups until the swarm closes.
- **The check:** when every task is settled and every branch reviewed, agent-mux runs the swarm's check command, if it has one, in the workspace (`sh -c`, at most 30 minutes). The top line reads `check running`, then `check ✓` or `check ✗`. A failed check lands in Needs you with the end of its output. The swarm still finishes, because whether to fix it is your call.
- **Finished:** once every task is settled, every branch reviewed and the check run, the swarm is `finished` and Needs you offers to close it. `y` closes the swarm: its sessions stop, open tasks are cancelled, and leases are released.

## The trace

Every session of a swarm is recorded with the swarm's id (`launches.metadata.swarm_id`, its role, its task and its parent), so the whole swarm reads as one trace:

- `agent-mux trace swarm <id>` prints the swarm as a tree (the coordinator, each worker with its task's state, turns, tools, tokens, cost and `↳` sub-agents, the decisions, the check); `--timeline` draws the Timeline tab's lanes, `--log` prints the log, `--json` gives it all. Without an id, the workspace's live swarm.
- `agent_mux_get_swarm` is the same for a session or Heimdall (whose startup briefing lists the workspace's swarms of the day).
- **Langfuse:** with Langfuse configured, every turn of the coordinator and the workers carries `session.id = <swarm id>` and the tags `swarm`, `swarm:coordinator` or `swarm:worker`, so the swarm is one Langfuse session (`agent-mux langfuse web -s <swarm id>`). When the swarm closes, agent-mux sends one **summary trace** named `swarm <goal>`: a span for the swarm, one per task (timing, state, worker, tokens, cost, result or error, and the worker's trace ids) and an event per decision. `agent-mux langfuse export-swarm <id>` sends a swarm again, or for the first time when it ran with Langfuse off.

## From the command line

```sh
agent-mux swarm start "Add OAuth login" --planner claude --worker-harness agy,codex --workers 4 --isolation worktree
#   --planner: the coordinator's harness · --worker-harness: the workers' pool, the first for tasks that name none
agent-mux swarm status                  # the swarm and its board, each task's state (--json for scripts)
agent-mux swarm plan                    # the board with the plan's errors and warnings
agent-mux swarm approve                 # from a shell: every ready task starts
agent-mux swarm launch [<task>...]      # start the named ready tasks now (or every ready one)
agent-mux swarm wait [--timeout MS]     # block until the swarm logs something; a timeout is an answer
agent-mux swarm events [-n 50]          # the swarm's log: who decided, offered, started, finished what
agent-mux swarm replan "split the API task in two"
agent-mux swarm pause | resume
agent-mux swarm retry <task>            # a failed, lost or cancelled task, again
agent-mux swarm cancel <task>           # a task that has not finished; its worker is stopped
agent-mux swarm merge <task>            # or: set-aside <task>
agent-mux swarm accept <task>           # or: drop <task>, for a task a worker proposed
agent-mux swarm close                   # stops its sessions, cancels open tasks, releases leases
agent-mux swarm restart                 # a new coordinator in place of one that exited
agent-mux swarm submit-plan --file plan.json   # what agent_mux_swarm_plan does, from a shell
agent-mux swarm broadcast "Freeze src/api/" --urgent --wake
```

- **`<task>`** is a plan key (`bus-fix`), a task id, or a unique prefix of either.
- **Without the TUI:** `start`, `replan`, `restart` and `close` need the agent-mux TUI running. `approve`, `pause`, `resume`, `retry`, `merge`, `set-aside`, `accept` and `drop` also work without it: they update the records, and the workers start when a TUI runs.
- **`stop`** is like `close`. Without a TUI, it only updates the records and says so.
- **Options:** every command takes `--workspace DIR` (default: the current folder) and `--json`. `--no-plan-gate` starts the workers as soon as the plan passes its checks.

## The coordinator

The coordinator is an interactive session on the coordinator harness, in the swarm's workspace. Its opening prompt is its role and the planner context (`$AGENT_MUX_SWARM_PLAN`).

- **It plans** with `agent_mux_swarm_plan`. With errors, the tool answers them and nothing changes; the coordinator fixes them and submits again. A sound plan goes on the board for you to review.
- **It answers** the workers' questions, or forwards them to you with its recommendation.
- **It is told** when you approve the plan or send it back, and, batched at most once a minute while it is idle, which tasks finished, failed or were proposed. It may propose follow-up tasks (`agent_mux_task_split`), which land on the board as proposals.
- **It cannot** approve its own plan, start workers, merge or close the swarm. agent-mux does those, at your word.
- **If it stops,** the swarm keeps running. Its `★` row shows it, and `r` there starts a new coordinator with the board as it stands (`x` stops a running one first).

## What the workers use

Each worker starts with its whole task as its opening message: the instructions, the done-when line, its files, what the tasks before it reported, and how to finish. It uses the bus:

- **`agent_mux_task_update`** with `done` (and a result) or `fail` (and the reason) reports the task. Until it does, the task isn't over.
- **`agent_mux_send_message`** to `coordinator` with topic `question` asks the coordinator first. It answers, or asks you with its recommendation. A question the coordinator leaves unanswered for 5 minutes lands in your Needs you.
- **`agent_mux_task_split`** (or `agent_mux_delegate`) from a worker doesn't start anything. It puts the task on the board as a **proposal**: with the plan gate on, it waits for you (`y` accepts, `x` drops); with the gate off, it is scheduled like the others. The worker is told so and carries on with its own task.
- **File leases** stop two workers in a shared workspace from editing the same file. A blocked edit names the worker that holds the lease.

The full list and the matching `agent-mux bus …` commands are in [bus.md](bus.md).

## Tips

- **Write the goal like a ticket.** Say what done looks like, the constraints, and what not to touch. The coordinator splits better with clear edges.
- **Read the plan's files and dependencies before approving.** Tasks that run at the same time on the same files are the most common cause of a swarm stepping on itself; the plan warns about them.
- **Use worktree isolation for broad changes** and shared isolation for small, well-separated files. Worktrees cost a merge per task; shared costs lease contention.
- **Merge as tasks finish.** A task that depends on a worktree task waits for that merge.
- **Answer questions quickly.** An asking worker holds its slot until you reply.
- **Use `!` sparingly.** An urgent message interrupts the worker mid-task: use it for "stop, that's wrong", not for questions.

## When something goes wrong

| Symptom | Cause and fix |
| --- | --- |
| `y` says "fix the plan first: …" | The plan has an error. Select the task marked `!`, read its card, and `e` edit or `d` delete it, or `p` re-plan. |
| "the coordinator has not submitted a plan" | Select `★` to see what it is doing, and `Enter` to talk to it. `p` sends it notes; `x` then `r` on `★` starts a new one. |
| A task stays `⋯` blocked | A task it depends on isn't done, or its branch isn't merged. The card says which. A failed dependency needs `r`. |
| A task shows `!` lost | Its session exited mid-task. `r` retries it with a new worker. |
| A task shows `○` stalled | The worker stopped without reporting, even after a reminder. Attach (`Enter`) to see why, message it, or `r` to stop it and `r` again to retry. |
| "a swarm runs inside the running agent-mux TUI" from `swarm start` | The TUI isn't running. Start `agent-mux` first. |
| Merge says "CONFLICT … (merge aborted, nothing changed)" | Merge the branch by hand (`git merge loop/bus-…`), resolve the conflict and commit, then `x` to mark it handled. |
| An edit is refused with a lease message | Another worker holds that file. Message it, or wait for its task to finish. |
