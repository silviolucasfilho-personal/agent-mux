# Swarms: one goal, many agents

A **swarm** is a coordinator agent that splits one goal into tasks and hands each to a worker agent, all working in parallel in the same workspace. They can be Claude Code, Codex, Antigravity or OpenCode sessions, in any mix. You watch and steer the swarm from one screen, **Swarm Mission Control**. This guide shows how to use it. How it works inside (tables, control requests, schema) is in [bus.md](bus.md#swarm-mission-control-modeswarm-agent-mux-swarm-), and the design is in [the spec](superpowers/specs/2026-10-30-swarm-mission-control-ux-design.md).

## In one minute

1. Select a session in the folder you want to work on, press `F1` and choose **Open swarm**.
2. Press `r`, write the goal, and press `Ctrl+S`.
3. The coordinator plans. When **⚑ NEEDS YOU** shows its plan, press `!` then `y` to approve it.
4. Workers start. Answer what lands in **Needs you**, and write to agents with `>`.
5. When a worker finishes in its own git worktree, `y` merges its branch.
6. When the swarm says it's finished, `y` closes it.

## Before you start

- **The workspace** is the folder of the session selected when you open the screen (otherwise the folder agent-mux started in). It's shown in the top line until you start a swarm.
- **The agents need the bus tools.** agent-mux attaches its MCP server to every session it launches, so this works out of the box. If you set `[agents] mcp = "off"` in `profiles.toml`, swarms can't coordinate.
- **Worktree isolation needs a git repository.** With **shared** isolation, agents edit the same files, protected by file leases. With **worktree** isolation, each task gets its own branch under `.loop-worktrees/`.

## Starting a swarm

Press `r` (or `Enter` on the empty screen). The launch form asks for:

| Field | Meaning | Default |
| --- | --- | --- |
| Mission Goal | What the swarm should achieve. Be concrete: what done looks like, and what's out of scope. | — |
| Coordinator Harness | Which agent leads: `claude`, `codex`, `agy` or `opencode`. | `claude` |
| Max Parallel Workers | The cap. Spawns beyond it are refused, and those tasks wait. | 3 (up to 8) |
| Default Task Isolation | `shared` (leases on files) or `worktree` (one branch per task). | `shared` |
| Approve the plan before workers start | The **plan gate**. Leave it on unless the goal is trivial. | yes |
| Standby Workers | Idle workers waiting for tasks. With the gate on, they start after you approve. | 0 |

`Tab` moves between fields, `←`/`→` change an option, and `Ctrl+S` launches. To start a swarm from a script instead, see [From the command line](#from-the-command-line).

## The screen

```
┌ ◆ Ship auth + UI · executing · 3/6 done ███████░░░ · 3/4 workers · 18m · 41k tok $1.24 ──────────┐
│ ⚑ NEEDS YOU (2)   ! to act                                                                         │
│   question  implementer·codex asks you: "Should cancelled tasks release worktrees?"  4m            │
│   failed    tester·agy failed "UI tests": cargo test exit 101  2m                                   │
│┌ Lanes (5) ─────────────────────────────┐┌ implementer·codex · Wire bus store queries ───────────┐│
││  ● ★ coordinator·claude  running Bash  ││  1 Live   2 Thread (3)   3 Changes   4 Task            ││
││    plan: 3/6 tasks done                ││  …the agent's terminal, its messages, diff or task…    ││
││▸ ◷ implementer·codex  waiting 4m on you││                                                        ││
││    Wire bus queries · wt loop/bus-946  ││                                                        ││
││  ✗ tester·agy  failed · exit 101       ││                                                        ││
││  ◦ queued  waiting for a worker        ││                                                        ││
│ ↑↓ lane  Tab panel  1-4 view  Enter attach  m message  > command  n worker  x stop  a activity     │
```

- **Top line:** the goal, the phase (`planning` → `executing` → `integrating` → `wrapping up` → `done`), tasks done, workers against the cap, elapsed time and spend.
- **Needs you:** only what *you* have to resolve, most urgent first. When nothing does, it collapses to "Nothing needs you."
- **Lanes:** one row per agent and the task it holds. The coordinator (`★`) comes first, then workers in the order they joined, then tasks nobody holds yet (`queued`). Names are `persona·harness`, like `implementer·codex`. When two agents would share a name, the first four characters of their id are added. A name never moves to a different agent.
- **Panel:** shows the selected lane. `1` Live is its terminal, `2` Thread its messages, `3` Changes its `git diff`, and `4` Task its task card. On the coordinator, tab 4 is **Plan**. The panel opens on the tab that matters: Thread when the agent is waiting, Task when it failed, Plan while the plan waits for you. `a` swaps the panel for **Activity**, the whole swarm's messages and task events.

### What the lane symbols mean

| Symbol | State | | Symbol | State |
| --- | --- | --- | --- | --- |
| `●` | working | | `✓` | done |
| `◷` | waiting on an answer | | `✗` | failed |
| `⚑` | waiting on a prompt in its terminal | | `!` | lost: its session exited mid-task |
| `○` | idle with an unfinished task | | `◦` | ready (no task) or queued |
| `·` | exited | | | |

## The plan gate

With the gate on (the default), no worker can start until you approve the plan. This is enforced, not just suggested: the coordinator's attempts to delegate, and any agent's attempt to claim a task, are refused until approval.

1. The coordinator puts its tasks on the board and sends you a `plan` message.
2. **Needs you** shows `plan  coordinator·claude proposes a plan of N tasks`, and the coordinator's **Plan** tab shows the proposal and the board.
3. Choose:
   - `y` **approves**. The coordinator is told and woken, standby workers start, and the phase moves to `executing`.
   - `e` **sends the plan back**. The command line opens as `@coord #plan-changes `: write what should change, then `Enter`. The coordinator revises the plan and proposes again.

You can also approve with `y` on the coordinator's lane, even before it proposes, which approves the board as it stands.

## Acting on "Needs you"

Press `!` to focus the strip, `↑`/`↓` to choose an item, and `Esc` to go back to the lanes.

| Item | When | Enter | Also |
| --- | --- | --- | --- |
| `needs you` | an agent waits on a prompt in its terminal | attach to it | |
| `plan` | the coordinator proposed a plan | open the Plan tab | `y` approve · `e` send back |
| `question` | a question to you, or one nobody answered for 5 minutes | answer it on the command line | |
| `failed` | a task failed | open its Task tab | `r` retry |
| `lost` | an agent's session exited mid-task | open its Task tab | `r` retry |
| `stuck` | an agent printed nothing for 5 minutes with no tool running on an unfinished task | attach to it | |
| `cap` | every worker is busy and tasks wait | show the first queued task | |
| `review` | a worktree task finished | open its Changes tab | `y` merge · `x` set aside |
| `finished` | everything is settled | open the summary | `y` close the swarm |

`d` dismisses an item until you close the screen. If the same task fails again later, it comes back.

**Retry** (`r`) puts the task back on the board and hands it to a new worker. A worktree task keeps its worktree, so the new worker continues the work already there.

## Writing to agents

Press `>` to open the command line. `m` opens it addressed to the selected lane (or, on a message, as a reply to its sender), and `b` opens it addressed to everyone.

```
[!] [@who] [#topic] message
```

| You write | It does |
| --- | --- |
| `@coord split the auth task in two` | to the coordinator |
| `@all stop editing src/bus/ for now` | to every agent |
| `@tester run the UI tests again` | to a lane by name. Any unique prefix works, separators don't matter (`@impl`, `@tester.agy`), and so does a launch-id prefix. |
| `looks good` | no address: to the selected lane, or to the asker when you're replying |
| `!@impl stop, you're editing the wrong file` | **urgent**: the agent gets it at its very next tool call, even mid-task |
| `@coord #api-contract v2 is final` | with an explicit topic |

- **Before you send:** the line above the input says exactly what `Enter` will do (`→ tester·agy · directive · wakes it if idle`), or why it can't (`@impl could be implementer·codex·aaaa or implementer·codex·bbbb`).
- **Editing:** `Tab` completes `@` names (press again to cycle), `Alt+Enter` or `Ctrl+J` adds a line, paste works, and `Esc` cancels.
- **Delivery:** every message wakes an idle agent. Without `!`, a busy agent reads it when it next checks its messages.

## Lane keys

| Key | Does |
| --- | --- |
| `↑` `↓` | choose a lane |
| `Tab` | move to the panel and back (in Thread or Activity, `↑`/`↓` choose a message, `Enter` reads it in full, `m` answers it) |
| `1`–`4` | panel tab, remembered per lane |
| `Enter` | attach to the lane's terminal (return through `F1`) |
| `n` | new worker: a task with title, instructions, files, harness, persona and isolation, spawned at once (past the cap it stays queued) |
| `e` | edit the lane's task |
| `r` | queued lane: delegate it · failed or lost lane: retry it · otherwise: start a new swarm (asks first if one is running) |
| `x` | stop the lane's session, or cancel a queued task |
| `d` | release the lane's file leases |
| `a` | Activity on/off |
| `Ctrl+R` | reload now (the screen refreshes on its own when anything changes) |
| `Esc` | back to the lanes, then close |

Confirmations take `y` only. `Enter` never confirms a destructive action.

## Reviewing and finishing

- **Worktree tasks** end on a branch (`loop/bus-…`). Each one appears in Needs you as `review`, and the phase reads `integrating`.
  - `y` **merges** the branch into the branch your workspace has checked out, with a merge commit, after asking first. Git refuses if your uncommitted changes would be overwritten. A conflict aborts the merge and leaves your workspace as it was: resolve it by hand, then merge again.
  - `x` **sets the branch aside** without merging. The branch is kept.
- **Finished:** when every task is settled and every branch reviewed, Needs you shows `finished`, and the coordinator's Plan tab shows the summary: done, failed and cancelled tasks, branches merged (with commit) or set aside, time and spend.
  - `y` **closes the swarm**: its sessions stop, open tasks are cancelled, leases released, and the swarm is forgotten.
  - `d` keeps it running.

## From the command line

```sh
agent-mux swarm start "Add OAuth login" --coordinator claude --workers 4 --isolation worktree
agent-mux swarm status                  # coordinator, workers, tasks, leases, spend (--json for scripts)
agent-mux swarm broadcast "Freeze src/api/" --urgent --wake
agent-mux swarm stop                    # stops sessions, cancels open tasks, releases leases
```

- **`start`** needs the agent-mux TUI running, because the coordinator is a live session. It refuses otherwise. `--no-plan-gate` skips plan approval.
- **`stop`** goes through the running TUI to stop sessions. Without a TUI, it only updates the records and says so.
- **All four** take `--workspace DIR` (default: the current folder).

## What the agents use

The coordinator and the workers coordinate through the bus. They don't need to be told how; the coordinator's opening prompt covers it. The tools they use:

- `agent_mux_task_split` puts tasks on the board, and `agent_mux_delegate` spawns a worker for a task.
- `agent_mux_task_update` claims a task, finishes it, or fails it.
- `agent_mux_send_message` and `agent_mux_recv_messages` carry questions, answers, API contracts and handoffs.
- **File leases** stop two agents in a shared workspace from editing the same file. A blocked edit names the agent that holds the lease.

The full list and the matching `agent-mux bus …` commands are in [bus.md](bus.md).

## Tips

- **Write the goal like a ticket.** Say what done looks like, the constraints, and what not to touch. The coordinator splits better with clear edges.
- **Use worktree isolation for broad changes** and shared isolation for small, well-separated files. Worktrees cost a merge at the end; shared costs lease contention.
- **Keep the gate on** and read the plan's file lists. Overlapping files between tasks is the most common cause of a swarm stepping on itself.
- **Answer questions quickly.** A waiting worker holds its task (`◷`), and anything waiting more than 5 minutes is listed in Needs you.
- **Prefer `@coord` over writing to workers directly** when you change the plan, so the coordinator's picture stays true.
- **Use `!` sparingly.** An urgent message interrupts the agent mid-task: use it for "stop, that's wrong", not for questions.

## When something goes wrong

| Symptom | Cause and fix |
| --- | --- |
| "worker cap reached: N/N" | Every worker slot is busy. The task stays queued: wait for one to finish, or `r` on it later. |
| "plan awaits the operator's approval" | The gate is on. Approve the plan (`!`, `y`). |
| "a coordinator is a live session…" from `swarm start` | The TUI isn't running. Start `agent-mux` first. |
| A lane shows `!` lost | Its session exited mid-task. `r` retries the task with a new worker. |
| A lane shows `○` idle and appears as `stuck` | The agent stopped without finishing. Attach (`Enter`) to see why, or message it. |
| Merge says "CONFLICT … (merge aborted, nothing changed)" | Merge the branch by hand (`git merge loop/bus-…`), resolve the conflict, commit, then `x` to mark it handled. |
| An edit is refused with a lease message | Another agent holds that file. Coordinate through `@coord`, or `d` on the holder's lane if the lease is stale. |
