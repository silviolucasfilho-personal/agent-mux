# Swarms: one goal, many agents

A **swarm** splits one goal into tasks that agents work on in parallel in the same workspace. agent-mux runs it: a planner writes the plan, you review and approve it, and agent-mux starts one worker per task, in order, up to the number of workers you allow. Workers can be Claude Code, Codex, Antigravity or OpenCode sessions, in any mix. You watch and steer the swarm from the **Swarm** screen. This guide shows how to use it. How it works inside (tables, control requests, the dispatcher) is in [bus.md](bus.md#swarm-mission-control-modeswarm-agent-mux-swarm-), and the design is in the specs for [the orchestrator](superpowers/specs/2026-11-02-swarm-orchestrator-design.md) and [the screen](superpowers/specs/2026-11-02-swarm-agents-layout-design.md).

## In one minute

1. Select a session in the folder you want to work on, press `F1` and choose **Open swarm**.
2. Press `n` (or `Enter` on the empty screen), write the goal, and press `Ctrl+S`.
3. The planner writes the plan: its `★` row shows its terminal. When the swarm's card says the plan waits for you, look over the tasks, edit what you want, and press `y` then `y` to approve it.
4. Workers start, each on one task. Select a task to see its worker's terminal, and press `Enter` to type into it. `!` jumps to whatever needs you, and `>` writes to workers.
5. When a worker finishes in its own git worktree, `y` on its row merges its branch.
6. When the swarm is finished, `y` on its row closes it.

Anything that needs you also shows outside the Swarm screen, as `⚑ swarm needs you (n)` in the Agents sidebar and `⚑ swarm n` in the status bar.

## Before you start

- **The workspace** is the folder of the session selected when you open the screen (otherwise the folder agent-mux started in).
- **The workers need the bus tools.** agent-mux attaches its MCP server to every session it launches, so this works out of the box. If you set `[agents] mcp = "off"` in `profiles.toml`, workers can't report that they're done.
- **Worktree isolation needs a git repository.** With **shared** isolation, workers edit the same files, protected by file leases. With **worktree** isolation, each task gets its own branch under `.loop-worktrees/`.
- **One swarm per workspace** at a time. Close one before starting the next.

## Starting a swarm

Press `n` on a swarm row (or `Enter` on the empty screen). The form asks for:

| Field | Meaning | Default |
| --- | --- | --- |
| Goal | What the swarm should achieve. Be concrete: what done looks like, and what's out of scope. | — |
| Planner | Which harness writes the plan. Tasks that don't name a harness also run on it. | the selected session's |
| Max parallel workers | How many tasks run at once. A finished worker no longer counts. | 3 (up to 8) |
| Default isolation | `shared` (leases on files) or `worktree` (one branch per task). | `shared` |
| Approve the plan before workers start | The **plan gate**. With it off, workers start as soon as the plan passes its checks. | yes |
| Check when done | An optional command, such as `cargo test`. agent-mux runs it in the workspace once every task is settled and every branch reviewed, before the swarm reads `finished`. | — |

`Tab` moves between fields, `←`/`→` change an option, `Alt+Enter` adds a line to the goal, and `Ctrl+S` starts. The planner runs in the background, and its output shows in the panel while it works (`Enter` attaches to it).

## The screen

The Swarm screen has the Agents screen's layout: a tree on the left, and the selected row in the main pane on the right.

```
┌ Swarms • Keyboard focus ───┐┌ grok-spec · asking · Design specification ─────────────────────┐
│>▾ ◆ add grok to agent… ⚑2  ││  1 Task   2 Live   3 Thread (1)   4 Changes                      │
│     ★ planner·claude    ○  ││                                                                 │
│     ◷ 1 grok-spec       !  ││  12:35 grok-spec → you #question                                │
│     ⇡ 2 harness-core    !  ││    Should the spec cover ~/.grok sessions?                      │
│     ⋯ 3 workflows-and-… sh ││                                                                 │
│ ▸ ◆ fix the bus          ✓ ││                                                                 │
│ + new swarm  n             ││                                                                 │
└────────────────────────────┘└─────────────────────────────────────────────────────────────────┘
[F1 Commands] Swarm • Keyboard focus [n] New [e] Edit [d] Delete [r] Run/resume
```

- **The tree** lists every swarm: live ones first (the one in your folder at the top), then those that ended in the last week, folded. Under each `◆` swarm:
  - **`★` planner:** the coordinator's row. It shows the planner's terminal while it plans.
  - **Tasks:** one row per task, in plan order, with its state symbol, number and key. On the right is its harness, `!` when it needs you, or `✗` when the plan has an error on it.

  A swarm shows `⚑n` when it needs you, `done/total` otherwise, and `✓` once it's closed.
- **The main pane** shows the selected row:
  - **A swarm** shows its card: state, done/total, workers against your limit, elapsed time, tokens and cost, then everything that needs you, the plan, the check and the keys that apply now.
  - **The planner** shows its terminal.
  - **A task** shows its worker's terminal while it has one. This is the same pane as the Agents screen, at full size. Its tabs are `1 Task` (the card: instructions, done-when, files, dependencies, result or error, branch, and the plan's problems), `2 Live` (the terminal), `3 Thread` (its messages) and `4 Changes` (its diff). The pane opens on the tab that matters: Thread when the worker asked something, Live while it works, Changes when its branch waits for review, Task otherwise.
- **`Enter` on a terminal** gives it the keyboard, as on the Agents screen: the pane's title says `Keyboard goes to …`. `⌘L` (`Ctrl+Shift+L`) brings the keyboard back to the tree.
- **`a`** shows **Activity**, every message of the swarm.
- **Cost** reads `—` when a harness's cost isn't priced (agy isn't), and `≥$x` when only part of it is.

### What the task symbols mean

| Symbol | State | | Symbol | State |
| --- | --- | --- | --- | --- |
| `·` | draft (the plan isn't approved yet) | | `✓` | done |
| `⋯` | blocked: waits for a task it depends on | | `⇡` | done, its branch waits for review |
| `◦` | ready: starts when a worker is free | | `✗` | failed |
| `●` | working | | `!` | lost: its session exited mid-task |
| `◷` | asking: waits for your answer | | `–` | cancelled |
| `○` | stalled: stopped without finishing | | `+` | proposed by a worker |
| `⚑` | waiting on a prompt in its terminal | | | |

A task that depends on a worktree task starts only once that task's branch is merged, so it builds on the merged work.

## The plan

The planner reads the goal and the workspace and answers with tasks, each with a key, instructions, a done-when line, files and dependencies. agent-mux checks the plan:

- **Errors block approval:** a key used twice, a dependency that isn't in the plan or forms a cycle, a harness that isn't installed, an unknown agent, or a worktree outside a git repository.
- **Warnings don't:** two tasks that may run at the same time and touch the same files in a shared workspace, or more than 12 tasks.

In **review** you can change the plan before approving it:

| Key | Does |
| --- | --- |
| `e` (or `Enter`) | edit the selected task: title, instructions, done-when, files, dependencies |
| `n` | add a task |
| `d` | delete the selected task |
| `p` | re-plan: write what should change, `Enter`, and the planner runs again. Tasks that already ran are kept. |
| `y` | approve. Refused while the plan has errors: the status line names the first one. |

With the gate on, nothing starts before you approve, and workers can't claim tasks.

## What needs you

Each task that needs you carries `!` on its row, and its swarm carries `⚑n`. The swarm's card lists them all, most urgent first. `!` jumps to the next row that needs you, and that row's keys act on it.

| Item | When | On its row |
| --- | --- | --- |
| `question` | a worker asked you something | `m` answers it |
| `prompt` | a worker waits on a prompt in its terminal (usually a permission) | `Enter` types into its terminal |
| `plan` | the plan waits for your approval | on the swarm: `y` approve · `p` re-plan |
| `planner` | the planner gave no usable plan | on the swarm: `p` re-plan · `n` add tasks by hand |
| `failed` | a task failed | `r` retry |
| `lost` | a worker's session exited mid-task | `r` retry |
| `stalled` | a worker stopped without reporting, even after a reminder | `Enter` types into its terminal · `r` stops it, then `r` retries |
| `review` | a worktree task finished | `y` merge · `x` set aside |
| `proposed` | a worker proposed a new task | `y` accept · `x` drop |
| `check` | the final check failed | the swarm's card shows its output |
| `finished` | everything is settled | on the swarm: `y` close |

A worker that goes idle on an unfinished task gets one reminder after 90 seconds. Only if it stays idle does it show up as `stalled`.

**Retry** (`r`) puts the task back on the board, and agent-mux starts a new worker for it. A worktree task keeps its worktree, so the new worker continues the work already there.

## Writing to workers

Press `>` to open the command line. `m` opens it addressed to the selected task's worker (or, on a message, as a reply to its sender), and `b` opens it addressed to everyone.

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
| `←` `→` | fold or unfold a swarm (`←` on a task climbs to its swarm) |
| `Enter` | type into the row's terminal · edit a task that hasn't started · fold or unfold a swarm · start a swarm when there is none |
| `Tab` | move between the tree and the card (in Thread or Activity, `↑`/`↓` choose a message, `Enter` reads it in full, `m` answers it) |
| `1`–`4` | a task's tab |
| `y` | approve the plan · merge a branch · accept a proposal · close a finished swarm |
| `space` | pause or resume: while paused, running workers continue but nothing new starts |
| `n` | a new swarm (on a swarm row) · a new task (on a task or the planner) |
| `e` / `d` | edit or delete a task (delete only a task that never started) |
| `r` | retry a failed, lost, cancelled or stalled task |
| `x` | stop the task's worker · cancel a task · set a branch aside · drop a proposal |
| `p` | re-plan (in review) |
| `>` `m` `b` | write to workers |
| `!` | the next row that needs you |
| `a` | Activity on or off |
| `Ctrl+R` | reload now (the screen refreshes on its own when anything changes) |
| `⌘L` | from a terminal, back to the tree |
| `Esc` | back to the tree, then close the screen |

Confirmations take `y` only. `Enter` never confirms. What an action did shows in the status bar.

## Reviewing and finishing

- **Worktree tasks** end on a branch (`loop/bus-…`). Each appears in Needs you as `review`, and the swarm reads `integrating` once nothing else is left.
  - `y` **merges** the branch into the branch your workspace has checked out, with a merge commit, after asking first. Git refuses if your uncommitted changes would be overwritten. A conflict aborts the merge and leaves your workspace as it was: resolve it by hand, then merge again.
  - `x` **sets the branch aside** without merging it. The branch is kept. Tasks that depend on it stay blocked.
- **Finished workers stay open.** You can attach to one or message it with follow-ups until the swarm closes.
- **The check:** when every task is settled and every branch reviewed, agent-mux runs the swarm's check command, if it has one, in the workspace (`sh -c`, at most 30 minutes). The top line reads `check running`, then `check ✓` or `check ✗`. A failed check lands in Needs you with the end of its output. The swarm still finishes, because whether to fix it is your call.
- **Finished:** once every task is settled, every branch reviewed and the check run, the swarm is `finished` and Needs you offers to close it. `y` closes the swarm: its sessions stop, open tasks are cancelled, and leases are released.

## From the command line

```sh
agent-mux swarm start "Add OAuth login" --planner claude --workers 4 --isolation worktree
agent-mux swarm status                  # the swarm and its board, each task's state (--json for scripts)
agent-mux swarm plan                    # the board with the plan's errors and warnings
agent-mux swarm approve                 # workers start
agent-mux swarm replan "split the API task in two"
agent-mux swarm pause | resume
agent-mux swarm retry <task>            # a failed, lost or cancelled task, again
agent-mux swarm merge <task>            # or: set-aside <task>
agent-mux swarm accept <task>           # or: drop <task>, for a task a worker proposed
agent-mux swarm close                   # stops its sessions, cancels open tasks, releases leases
agent-mux swarm broadcast "Freeze src/api/" --urgent --wake
```

- **`<task>`** is a plan key (`bus-fix`), a task id, or a unique prefix of either.
- **Without the TUI:** `start`, `replan` and `close` need the agent-mux TUI running. `approve`, `pause`, `resume`, `retry`, `merge`, `set-aside`, `accept` and `drop` also work without it: they update the records, and the workers start when a TUI runs.
- **`stop`** is like `close`. Without a TUI, it only updates the records and says so.
- **Options:** every command takes `--workspace DIR` (default: the current folder) and `--json`. `--no-plan-gate` starts the workers as soon as the plan passes its checks.

## What the workers use

Each worker starts with its whole task as its opening message: the instructions, the done-when line, its files, what the tasks before it reported, and how to finish. It uses the bus:

- **`agent_mux_task_update`** with `done` (and a result) or `fail` (and the reason) reports the task. Until it does, the task isn't over.
- **`agent_mux_send_message`** to `user` with topic `question` asks you something. It lands in Needs you.
- **`agent_mux_task_split`** (or `agent_mux_delegate`) from a worker doesn't start anything. It puts the task on the board as a **proposal**: with the plan gate on, it waits for you (`y` accepts, `x` drops); with the gate off, it is scheduled like the others. The worker is told so and carries on with its own task.
- **File leases** stop two workers in a shared workspace from editing the same file. A blocked edit names the worker that holds the lease.

The full list and the matching `agent-mux bus …` commands are in [bus.md](bus.md).

## Tips

- **Write the goal like a ticket.** Say what done looks like, the constraints, and what not to touch. The planner splits better with clear edges.
- **Read the plan's files and dependencies before approving.** Tasks that run at the same time on the same files are the most common cause of a swarm stepping on itself; the plan warns about them.
- **Use worktree isolation for broad changes** and shared isolation for small, well-separated files. Worktrees cost a merge per task; shared costs lease contention.
- **Merge as tasks finish.** A task that depends on a worktree task waits for that merge.
- **Answer questions quickly.** An asking worker holds its slot until you reply.
- **Use `!` sparingly.** An urgent message interrupts the worker mid-task: use it for "stop, that's wrong", not for questions.

## When something goes wrong

| Symptom | Cause and fix |
| --- | --- |
| `y` says "fix the plan first: …" | The plan has an error. Select the task marked `!`, read its card, and `e` edit or `d` delete it, or `p` re-plan. |
| "the planner gave no plan" | Its answer had no usable `swarm-plan` block. `p` re-plans with notes, or `n` adds tasks by hand. |
| A task stays `⋯` blocked | A task it depends on isn't done, or its branch isn't merged. The card says which. A failed dependency needs `r`. |
| A task shows `!` lost | Its session exited mid-task. `r` retries it with a new worker. |
| A task shows `○` stalled | The worker stopped without reporting, even after a reminder. Attach (`Enter`) to see why, message it, or `r` to stop it and `r` again to retry. |
| "a swarm runs inside the running agent-mux TUI" from `swarm start` | The TUI isn't running. Start `agent-mux` first. |
| Merge says "CONFLICT … (merge aborted, nothing changed)" | Merge the branch by hand (`git merge loop/bus-…`), resolve the conflict and commit, then `x` to mark it handled. |
| An edit is refused with a lease message | Another worker holds that file. Message it, or wait for its task to finish. |
