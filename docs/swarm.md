# Swarms: one goal, many agents

A **swarm** splits one goal into tasks that agents work on in parallel in the same workspace. agent-mux runs it: a planner writes the plan, you review and approve it, and agent-mux starts one worker per task, in order, up to the number of workers you allow. Workers can be Claude Code, Codex, Antigravity or OpenCode sessions, in any mix. You watch and steer the swarm from the **Swarm** screen. This guide shows how to use it. How it works inside (tables, control requests, the dispatcher) is in [bus.md](bus.md#swarm-mission-control-modeswarm-agent-mux-swarm-), and the design is in [the spec](superpowers/specs/2026-11-02-swarm-orchestrator-design.md).

## In one minute

1. Select a session in the folder you want to work on, press `F1` and choose **Open swarm**.
2. Press `r` (or `Enter`), write the goal, and press `Ctrl+S`.
3. The planner writes the plan. When **⚑ NEEDS YOU** says the plan waits for you, read the board, edit what you want, and press `y` then `y` to approve it.
4. Workers start, each on one task. Answer what lands in **Needs you**, and write to workers with `>`.
5. When a worker finishes in its own git worktree, `y` merges its branch.
6. When the swarm is finished, `y` closes it.

## Before you start

- **The workspace** is the folder of the session selected when you open the screen (otherwise the folder agent-mux started in).
- **The workers need the bus tools.** agent-mux attaches its MCP server to every session it launches, so this works out of the box. If you set `[agents] mcp = "off"` in `profiles.toml`, workers can't report that they're done.
- **Worktree isolation needs a git repository.** With **shared** isolation, workers edit the same files, protected by file leases. With **worktree** isolation, each task gets its own branch under `.loop-worktrees/`.
- **One swarm per workspace** at a time. Close one before starting the next.

## Starting a swarm

Press `r` (or `Enter` on the empty screen). The form asks for:

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

```
┌ ◆ Ship three follow-ups · running · 1/3 done ███░░░░░░░ · 2/3 workers · 18m · 41k tok $1.24 ───────┐
│ ⚑ NEEDS YOU (1)   ! to act                                                                         │
│   question  f1-search asks: match section titles case-insensitively?   Enter answer                │
│┌ Plan (3) · planner: claude · "one task per subsystem" ─┐┌ f1-search · asking · F1 search ──────────┐│
││  ✓  1 bus-fix    Bus: all_workspaces lists …    codex ││ 1 Task   2 Live   3 Thread (2)   4 Changes ││
││▸ ◷  2 f1-search  F1: search section titles      codex ││ …its task card, terminal, messages or diff ││
││  ⋯  3 lf-cost    Langfuse: trace cost ← bus-fix claude││                                            ││
│ ↑↓ task  Enter attach  1-4 view  > message  space pause  r retry  x stop  ! needs  a activity  Esc   │
```

- **Top line:** the goal, the swarm's state, tasks done out of the total, busy workers against your limit, elapsed time, and tokens and cost.
  - The states run `planning` → `review` → `running` (or `paused`) → `integrating` → `finished`.
- **Needs you:** only what *you* have to resolve, most urgent first. When nothing does, it collapses to "Nothing needs you."
- **Board:** one row per task, in plan order. Each row shows:
  - its state symbol and number
  - its key and title
  - `← key` for each task it depends on
  - the harness, and once a worker runs it, its cost
  - `!` for a task with a plan error, `?` for a warning
- **Panel:** shows the selected task, on one of four tabs:
  - `1 Task` is its card: instructions, done-when, files, dependencies, and its result or error.
  - `2 Live` is its worker's terminal.
  - `3 Thread` is its messages.
  - `4 Changes` is its diff: its branch's commits and diff for a worktree task, or the workspace's uncommitted changes for a shared one.
  - The panel opens on the tab that matters: Thread when the worker asked something, Live while it works, Changes when its branch waits for review, Task otherwise. A tab you choose sticks to that task.
  - `a` swaps the panel for **Activity**, every message of the swarm.

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

## Acting on "Needs you"

Press `!` to focus the strip, `↑`/`↓` to choose an item, `Enter` to act, `d` to dismiss it for as long as the screen is open, and `Esc` to go back to the board.

| Item | When | Enter | Also (on the board) |
| --- | --- | --- | --- |
| `question` | a worker asked you something | answer it on the command line | |
| `prompt` | a worker waits on a prompt in its terminal (usually a permission) | attach to it | |
| `plan` | the plan waits for your approval | show the task's card | `y` approve · `p` re-plan |
| `planner` | the planner gave no usable plan | re-plan with notes | `n` add tasks by hand |
| `failed` | a task failed | show its card | `r` retry |
| `lost` | a worker's session exited mid-task | show its card | `r` retry |
| `stalled` | a worker stopped without reporting, even after a reminder | attach to it | `r` stops it, then `r` retries |
| `review` | a worktree task finished | show its Changes | `y` merge · `x` set aside |
| `proposed` | a worker proposed a new task | show its card | `y` accept · `x` drop |
| `check` | the final check failed | read its output | `r` retry a task |
| `finished` | everything is settled | | `y` close the swarm |

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

## Board keys

| Key | Does |
| --- | --- |
| `↑` `↓` `g` `G` | choose a task |
| `Tab` | move to the panel and back (in Thread or Activity, `↑`/`↓` choose a message, `Enter` reads it in full, `m` answers it) |
| `1`–`4` | panel tab |
| `Enter` | attach to the task's worker, or edit a task that hasn't started |
| `y` | approve the plan · merge a branch · accept a proposal · close a finished swarm |
| `space` | pause or resume: while paused, running workers continue but nothing new starts |
| `n` / `e` / `d` | add, edit or delete a task (delete only a task that never started) |
| `r` | retry a failed, lost, cancelled or stalled task · start a swarm when none runs |
| `x` | stop the task's worker · cancel a task · set a branch aside · drop a proposal |
| `p` | re-plan (in review) |
| `>` `m` `b` | write to workers |
| `!` | focus Needs you |
| `a` | Activity on or off |
| `Ctrl+R` | reload now (the screen refreshes on its own when anything changes) |
| `Esc` | back to the board, then close |

Confirmations take `y` only. `Enter` never confirms.

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
