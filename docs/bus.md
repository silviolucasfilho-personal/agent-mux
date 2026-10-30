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

## Swarm Control Room (`Mode::Swarm`, `agent-mux swarm …`)

The swarm screen (`F1` → "Open swarm control room", `src/app/swarm_view.rs`, `src/ui/swarm.rs`) shows one workspace's Coordinator, its Workers, the task board with its leases and the message feed on one screen, and starts a swarm from a goal: `r` opens the launch dialog, which spawns a Coordinator session (built-in `coordinator` persona, `$AGENT_MUX_SWARM_ROLE=coordinator`, `$AGENT_MUX_SWARM_GOAL`) and, if asked, pre-warmed workers. Design: `docs/superpowers/specs/2026-10-29-agent-swarm-screen-design.md`.

- **The swarm's state** is one row per workspace in `swarm_state` (schema `v17`): the coordinator's launch id, the goal, `max_workers` and the default isolation. `bus::store::set_coordinator` writes it when a swarm starts (goal and parameters) and when the user promotes another session with `c` (the goal and parameters stay); `swarm_state` reads it; `clear_swarm` forgets it (`agent-mux swarm stop`). `bus::swarm::load_snapshot` takes the coordinator from it, then from a live session whose persona is `coordinator`, then from the creator of the board's first task. Nothing of this is posted on the bus, so workers never receive the registry as mail.
- **Every worker holds a board task.** A worker spawned from the screen (`n` on the roster) runs the task the screen just created, same id, so the spawn claims it (`running`, `assigned_to`, leases on its `files`) and the worker's `$AGENT_MUX_TASK_ID` resolves. A pre-warmed worker holds a `Standby worker #n` task whose instructions (`bus::swarm::STANDBY_WORKER_INSTRUCTIONS`) tell it to poll the board and its inbox until the coordinator hands it work.
- **Broadcast wakes.** `ControlRequest::WakeWorkspace { workspace, prompt, exclude }` wakes every live session of the workspace (and of the worktrees its tasks run in) the way `WakeSession` wakes one, except the sender; the response's `detail` counts how many were woken and how many queued until idle. The screen's composer (`b`, or `m` with nothing selected), `agent-mux swarm broadcast --wake`, `agent-mux bus send --wake` without `--to` and `agent_mux_send_message(to = "*", wake = true)` all send it.
- **Reading and writing on the screen.** `Enter` on a feed row opens the message in full (`j`/`k` scroll, `m` replies to its sender with a fitting topic); the feed itself shows one line per message with its time of day. The composer (`m`, `b`) is a form: the body is text (Enter adds a line), `Tab` reaches Topic, Urgent and Wake (`←`/`→`/Space change them), `Ctrl+Enter` or `Ctrl+S` sends. The preview's Task Detail follows the selected agent's latest task, or the board's selection while the board has focus. A confirmation (stop a session, cancel a task, release a lease, relaunch a swarm) takes `y` only; `r` while a coordinator is live asks before launching a second swarm.
- **`agent-mux swarm start`** needs the running TUI: a coordinator is a live session, so without the control socket the command refuses rather than recording a coordinator that does not exist.

```
agent-mux swarm status [--workspace DIR] [--db PATH] [--json]
agent-mux swarm start <goal...> [--coordinator claude|codex|agy] [--workers N] [--isolation shared|worktree] [--workspace DIR]
agent-mux swarm broadcast <message...> [--urgent] [--wake] [--workspace DIR] [--db PATH] [--json]
agent-mux swarm stop [--workspace DIR] [--db PATH] [--json]
```
