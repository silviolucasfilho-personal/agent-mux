# Inter-Agent Bus (`agent-mux bus …` · MCP coordination tools)

The **Inter-Agent Bus** (`src/bus/`, schema `v16` in `src/tracing/store/schema.rs`) connects live and newly delegated harness sessions (`claude`, `codex`, `agy`) across a workspace. It combines a shared SQLite coordination store (`agent_messages`, `agent_tasks`, `agent_leases` in `traces.db`) with a live TUI Unix domain control socket (`$AGENT_MUX_CONTROL_SOCK`, default `~/.agent-mux/snapshots/bus.sock`).

It supports two workflows simultaneously:

1. **Peer-to-Peer live coordination**: running sessions discover each other, exchange direct or broadcast messages, wake idle peers, acquire cooperative file leases enforced by the `PreToolUse` hook guard, and split/claim/complete tasks on a shared workspace task board.
2. **Lead-to-Workers dynamic delegation**: any lead session can delegate a task to a newly spawned worker session (`claude`, `codex`, or `agy`, optionally with a persona agent) in either the shared workspace or an isolated git worktree, grouped under `⇄` (`GroupKind::Bus`) in the Agents sidebar.

## Environment variables handed to every traced launch

- `AGENT_MUX_SESSION_ID`: the session's unique `launch_id` in `traces.db`.
- `AGENT_MUX_WORKSPACE`: the canonical workspace path of the session.
- `AGENT_MUX_CONTROL_SOCK`: the Unix domain socket path where the running `agent-mux` TUI listens for `WakeSession` and `SpawnWorker` requests.

## MCP tools (`agent-mux mcp serve`)

Every harness session wired with the `agent-mux` MCP server can call these tools directly:

| Tool | Purpose |
| --- | --- |
| `agent_mux_list_peers` | Lists active and recent peer sessions in the workspace (launch ID, harness, runtime state, active tasks, unread count) and active file leases. |
| `agent_mux_send_message` | Sends a direct (`to = "<launch_id>"`) or broadcast (`to = "*"`) message (`topic`, `urgent`, `wake`). When `wake = true`, asks the TUI to wake an idle recipient session. |
| `agent_mux_recv_messages` | Reads messages addressed to this session or broadcast to the workspace, with optional long-polling (`wait_ms` up to `30000`). |
| `agent_mux_task_board` | Lists tasks on the shared workspace task board (`open`, `claimed`, `running`, `done`, `failed`, `cancelled`) and active file leases. |
| `agent_mux_task_split` | Publishes one or more subtasks (`title`, `instructions`, `files`, `target_harness`, `target_agent`, `isolation`) onto the workspace task board. |
| `agent_mux_task_update` | Performs an atomic task or lease action: `"claim"`, `"running"`, `"complete"` / `"done"`, `"fail"`, `"cancel"`, `"wait"`, `"lease"`, or `"unlease"`. |
| `agent_mux_delegate` | Creates a task on the workspace board and asks the live `agent-mux` TUI over `$AGENT_MUX_CONTROL_SOCK` to spawn a worker session (`isolation = "shared" \| "worktree"`), optionally waiting (`wait_ms`) for completion. |

## CLI (`agent-mux bus …`)

The CLI mirrors the MCP tools 1:1 so shell scripts and hooks can participate:

```
agent-mux bus peers [--all] [--workspace DIR] [--db PATH] [--json]
agent-mux bus send <body...> [--to LAUNCH_ID] [--topic message|question|urgent] [--reply-to MSG_ID] [--wake] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
agent-mux bus recv [--from-peer LAUNCH_ID] [--all] [--no-ack] [--wait-ms MS] [--limit N] [--workspace DIR] [--db PATH] [--json]
agent-mux bus tasks [--status S] [--assigned-to ID] [--parent ID] [--workspace DIR] [--db PATH] [--json]
agent-mux bus split --title TITLE --instructions TEXT [--files p1,p2] [--harness H] [--agent A] [--isolation shared|worktree] [--assign ID] [--parent ID] [--workspace DIR] [--db PATH] [--json]
agent-mux bus claim <task-id> [--from LAUNCH_ID] [--db PATH] [--json]
agent-mux bus done <task-id> [--result JSON_OR_TEXT] [--from LAUNCH_ID] [--wake] [--db PATH] [--json]
agent-mux bus fail <task-id> <error...> [--from LAUNCH_ID] [--wake] [--db PATH] [--json]
agent-mux bus wait <task-id...> [--wait-ms MS] [--db PATH] [--json]
agent-mux bus delegate --title TITLE --instructions TEXT [--task-id ID] [--harness claude|codex|agy] [--agent NAME] [--files p1,p2] [--isolation shared|worktree] [--wait-ms MS] [--workspace DIR] [--db PATH] [--json]
agent-mux bus lease <pattern...> [--task-id ID] [--ttl-s SEC] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
agent-mux bus unlease [pattern...] [--task-id ID] [--from LAUNCH_ID] [--workspace DIR] [--db PATH] [--json]
```

## Cooperative file leases & urgent message delivery

- **Lease enforcement**: when a session runs in a shared workspace and another peer holds an active lease in `agent_leases` matching a file targeted by `Write`, `Edit`, `MultiEdit`, or `NotebookEdit`, `tracing::hooks::guard::bus_lease_verdict` blocks the tool call in `PreToolUse` and names the owning peer and task.
- **Automatic lease lifecycle**: claiming a task (`claim`) or spawning a shared-workspace worker automatically acquires leases on the task's `files` (`DEFAULT_LEASE_TTL_NS` = 30 minutes); marking a task `done`, `failed`, or `cancelled` automatically releases its leases and posts a `"result"` message back to the task creator.
- **Urgent message injection**: unread messages sent with `topic = "urgent"` (or `urgent = true`) are drained by `bus::store::drain_urgent_for_hook` on the recipient's next `PreToolUse` hook invocation and injected via `hookSpecificOutput.additionalContext`.

## Swarm Mission Control (`Mode::Swarm`, `agent-mux swarm …`)

The swarm screen (`F1` → "Open swarm control room", `src/app/swarm_view.rs`, `src/ui/swarm.rs`, model `src/bus/swarm.rs`) shows one workspace's swarm as **lanes**, each an agent and the task it holds, beside a panel on the selected lane, and starts a swarm from a goal: `r` opens the launch dialog, which spawns a Coordinator session (built-in `coordinator` persona, `$AGENT_MUX_SWARM_ROLE=coordinator`, `$AGENT_MUX_SWARM_GOAL`) and, if asked, pre-warmed workers. Design: `docs/superpowers/specs/2026-10-30-swarm-mission-control-ux-design.md` (the first layout: `2026-10-29-agent-swarm-screen-design.md`).

- **Mission strip** (the top border): the goal, the phase (`not started`, `planning`, `executing`, `wrapping up`, `done`: `SwarmSnapshot::phase`), tasks done with a bar, workers against `max_workers`, time since the swarm started, and tokens and cost when there is room.
- **Lanes** (`SwarmSnapshot::lanes`): the coordinator first (`★`, its plan: tasks done and open questions), then each worker in the order it joined, then each task nobody holds yet (`◦ queued`). A lane says what it is doing now (`running Edit`, `waiting 4m on coordinator·claude`, `done · …`, `failed · …`) and where its task works (`shared · 2 leases`, `wt <branch>`). Its state, first match wins: `✗` failed, `!` lost (the session exited mid-task), `⚑` needs you in its terminal, `◷` waiting on an answer, `●` working, `✓` done, `○` idle, `◦` ready or queued, `·` exited.
- **Names are stable**: `persona·harness` (`implementer·codex`), with the launch id's first four characters when two lanes share a name; the coordinator is `coordinator·<harness>`. A name never passes to another agent. Lanes, threads and confirmations use the same names.
- **Open questions** (`SwarmSnapshot::open_questions`): a `question` is answered by a later message naming it in `reply_to`, or by a later message to its asker from its addressee (from anyone, for a broadcast question). Replies written on the screen set `reply_to`.
- **Needs you** (`SwarmSnapshot::needs`), the strip under the mission line: only what the operator has to resolve, most urgent first: a session waiting on a prompt in its terminal, a question to you (at once) or one nobody answered for `STUCK_AFTER_NS` (5 minutes), a failed task, a task whose session exited (lost), a lane quiet that long on an unfinished task with no tool running (stuck: `Session::quiet_for`), the worker cap reached while tasks wait, a task finished in a worktree waiting for review. One line ("Nothing needs you.") when empty. `!` focuses it: `↑↓` a need, `Enter` acts (answers the question, attaches to the waiting or stuck session, or shows the lane on its Task or Changes tab), `r` retries a failed or lost task (`bus::store::reopen_task`: open, nobody's, no error, leases released, worktree and branch kept; then delegated again, the new worker continuing in the same worktree), `d` dismisses it for this screen (a later failure of the same task comes back). `r` on a failed or lost lane retries too.
- **The panel**, on the selected lane: `1 Live` (its terminal), `2 Thread` (its messages as a conversation, whole bodies, open questions marked), `3 Changes` (`git diff` of its worktree, or the workspace), `4 Task` (the task card and the leases it holds). A lane opens on what its state calls for: Thread when waiting, Task when failed, lost or queued, Changes when done in a worktree, Live otherwise; a tab you choose sticks to that lane. `a` swaps the panel for the whole swarm's **Activity** (`SwarmSnapshot::timeline`): its messages with what happened to tasks and leases (planned or added, finished, failed, cancelled, a lease taken); `Enter` on an event shows its lane.
- **Writing**: `>` opens the command line at the bottom; `m` opens it addressed to the selected lane (or, on a message, as a reply to its sender), `b` addressed to everyone. A line reads `[!] [@who] [#topic] message`: `@coord`, `@all` or a lane (`@tester`, `@impl` for `implementer·codex`: separators are ignored and a unique prefix or launch id is enough; a disambiguated name is one word, `@implementer·codex·8f31`), the selected lane when none is named, the asker when replying. `Tab` completes and cycles `@` names. A leading `!` sends it as `urgent`, which the `PreToolUse` hook hands to the agent at its next tool call (`drain_urgent_for_hook`); the topic is otherwise inferred (`answer` for a reply to a question, else `directive`) unless `#topic` names one. Every message wakes an idle recipient. The line above the input says what Enter would do (`→ tester·agy · directive · wakes it if idle`) or why it cannot (`@impl could be …`); `Esc` drops it; `Alt+Enter` or `Ctrl+J` adds a line; paste lands in it (`App::handle_paste`). A reply keeps `reply_to` only while it goes back to the asker.
- **Keys**: `!` the Needs-you strip, `↑↓` a lane, `Tab` the panel and back (there `↑↓` choose a message or scroll, `Enter` reads it in full, `m` answers it), `1`–`4` the panel's tab, `Enter` attaches to the lane's session, `>` the command line, `m` writes to the lane, `b` to everyone, `n` a new worker (a task spawned at once; past the cap it stays queued), `e` edits the lane's task, `r` delegates a queued lane, retries a failed or lost one, or starts a swarm (asking when one is live), `x` stops the lane's session or cancels a queued task, `d` releases the lane's leases, `a` Activity, `Ctrl+R` reload, `Esc` back to the lanes, then close. `c` (make the lane the coordinator) and `w` (wake it) still work but leave the footer. A confirmation takes `y` only.
- **The swarm's state** is one row per workspace in `swarm_state` (schema `v17`): the coordinator's launch id, the goal, `max_workers` and the default isolation. `bus::store::set_coordinator` writes it when a swarm starts (goal and parameters) and when the user promotes another session with `c` (the goal and parameters stay); `swarm_state` reads it; `clear_swarm` forgets it (`agent-mux swarm stop`). `bus::swarm::load_snapshot` takes the coordinator from it, then from a live session whose persona is `coordinator`, then from the creator of the board's first task. Nothing of this is posted on the bus, so workers never receive the registry as mail.
- **Every worker holds a board task.** A worker spawned from the screen (`n`) runs the task the screen just created, same id, so the spawn claims it (`running`, `assigned_to`, leases on its `files`) and the worker's `$AGENT_MUX_TASK_ID` resolves. A pre-warmed worker holds a `Standby worker #n` task whose instructions (`bus::swarm::STANDBY_WORKER_INSTRUCTIONS`) tell it to poll the board and its inbox until the coordinator hands it work.
- **Broadcast wakes.** `ControlRequest::WakeWorkspace { workspace, prompt, exclude }` wakes every live session of the workspace (and of the worktrees its tasks run in) the way `WakeSession` wakes one, except the sender; the response's `detail` counts how many were woken and how many queued until idle. The screen's `@all` (`b`), `agent-mux swarm broadcast --wake`, `agent-mux bus send --wake` without `--to` and `agent_mux_send_message(to = "*", wake = true)` all send it.
- **`max_workers` is enforced.** `SpawnWorker` (the screen's `n`/`r`, `agent_mux_delegate`, `agent-mux bus delegate`) refuses when the swarm's `max_workers` delegated workers are already live in the workspace (sessions in a bus group; the coordinator is not counted), with `worker cap reached: N/N …`; the task stays on the board for `r` later, and the title shows `N/cap Workers`. A swarm without a recorded cap has none.
- **The screen refreshes on change, not on a timer.** `SwarmView` keeps one store connection and, at most every 500 ms, asks whether anything moved: the `trace_changes` feed (schema `v18` adds triggers for `agent_messages`, `agent_tasks`, `agent_leases` and `swarm_state`), a live session's state or telemetry (a fingerprint, no store read), or a lease it shows expiring (`SwarmView::refresh_due`). `Ctrl+R` and every action refresh at once. `load_snapshot` is a read: expired leases are left out, pruning stays with the writers (`acquire_leases`, `split_tasks`). Timestamps handed to it are nanoseconds, like the rest of the store.
- **`agent-mux swarm start`** needs the running TUI: a coordinator is a live session, so without the control socket the command refuses rather than recording a coordinator that does not exist.

```
agent-mux swarm status [--workspace DIR] [--db PATH] [--json]
agent-mux swarm start <goal...> [--coordinator claude|codex|agy] [--workers N] [--isolation shared|worktree] [--workspace DIR]
agent-mux swarm broadcast <message...> [--urgent] [--wake] [--workspace DIR] [--db PATH] [--json]
agent-mux swarm stop [--workspace DIR] [--db PATH] [--json]
```
