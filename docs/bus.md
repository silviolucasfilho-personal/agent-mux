# Inter-Agent Bus (`agent-mux bus …` · MCP coordination tools)

The **Inter-Agent Bus** (`src/bus/`, schema `v16` in `src/tracing/store/schema.rs`) connects live and newly delegated harness sessions (`claude`, `codex`, `agy`) across a workspace. It combines a shared SQLite coordination store (`agent_messages`, `agent_tasks`, `agent_leases` in `traces.db`) with a live TUI Unix domain control socket (`$AGENT_MUX_CONTROL_SOCK`, default `~/.agent-mux/snapshots/bus.sock`).

It supports two workflows simultaneously:

1. **Peer-to-Peer live coordination**: running sessions discover each other, exchange direct or broadcast messages, wake idle peers, acquire cooperative file leases enforced by the `PreToolUse` hook guard, and split/claim/complete tasks on a shared workspace task board.
2. **Lead-to-Workers dynamic delegation**: any lead session can delegate a task to a newly spawned worker session (`claude`, `codex`, or `agy`, optionally with a persona agent) in either the shared workspace or an isolated git worktree, grouped under `⇄` (`GroupKind::Bus`) in the Agents sidebar.

## Environment variables handed to every traced launch

- `AGENT_MUX_SESSION_ID`: the session's unique `launch_id` in `traces.db`.
- `AGENT_MUX_WORKSPACE`: the canonical workspace path of the session.
- `AGENT_MUX_CONTROL_SOCK`: the Unix domain socket path where the running `agent-mux` TUI listens for `WakeSession` and `SpawnWorker` requests.
- **How a wake is delivered** (`WakeSession`): to an idle session, the prompt (one line) is pasted into its input box, bracketed when the child enabled bracketed paste, and Enter follows as a separate write about 150 ms later (`app::WAKE_SUBMIT_DELAY`). Claude Code and Codex take a single burst over about 100 bytes as a paste and would leave `prompt\r` unsubmitted (probed 2026-11-02 on Claude Code 2.1.316, Codex 0.166.0, OpenCode 2.0.32 and agy 1.4.12). A session that is working, or waiting on a prompt in its terminal, gets the wake queued (`App::pending_bus_wakes`, one per session) until it turns idle; a wake not delivered within 10 minutes (`app::WAKE_TTL`) is dropped, and the message stays unread on the bus.

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

How to use it: [swarm.md](swarm.md). The swarm screen (`F1` → "Open swarm", `Mode::Swarm`) has its state and keys in `src/app/swarm_board.rs` (`SwarmBoard`), its drawing in `src/ui/swarm_board.rs`, and its model in `src/bus/swarm/` (`record`, `engine`, `plan`). It shows one workspace's orchestrated swarm as a **board** of its plan's tasks beside a panel on the selected task. Design: `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §8; the earlier layouts were `2026-10-30-swarm-mission-control-ux-design.md` (lanes) and `2026-10-29-agent-swarm-screen-design.md` (quadrants).

- **Mission strip** (the top border): the goal, the swarm's state (`swarms.status`), tasks done with a bar, busy workers against `max_workers` (`engine::busy_workers`: a finished worker stays open but holds no slot), time since the swarm started, and tokens and cost of its live workers.
- **Board:** `record::board` in plan order.
  - **Row:** a task's shown state (`engine::shown`: draft, proposed, blocked, ready, working, asking, stalled, prompt, done, review, failed, lost, cancelled, each with its glyph), its key and title, `← key` for each dependency, its harness and its worker's cost.
  - **Marks:** `!` and `?` flag the plan's errors and warnings (`plan::check` over `record::board_plan`, computed while the swarm is `planning` or `review`).
  - **Empty board:** while planning, the planner's terminal fills the panel. A planner that answered nothing usable leaves the board empty, with its error.
- **Panel**, on the selected task:
  - `1 Task` is the card: instructions, done-when, files, dependencies, result or error, branch and review, and the plan's problems.
  - `2 Live` is its worker's terminal.
  - `3 Thread` shows messages from or to its worker, plus broadcasts.
  - `4 Changes` is a worktree task's commits and diff since it branched from the workspace's branch, or the workspace's uncommitted diff for a shared task.
  - The default tab follows the state (`Tab::for_state`), and a chosen tab sticks per task.
  - `a` shows **Activity**, every message of the workspace.
- **Needs you** (`swarm_board::compute_needs`), most urgent first:
  - an open question from a worker or to `user` (`bus::swarm::open_questions`)
  - a worker on a terminal prompt
  - the plan in review, or a planner that gave no plan
  - a failed task, a lost task (its session exited mid-task), a stalled task (idle after its one nudge)
  - a branch waiting for review, a proposed task
  - a finished swarm

  `!` focuses it, `Enter` acts, and `d` dismisses an item for the screen's lifetime (keyed by the task's `updated_ns`, so a new failure comes back).
- **Verbs** go through `App::swarm_action` (`SwarmVerb`), the same path as `agent-mux swarm …`:
  - **Plan:** `y` approves the plan (refused while it has errors). `p` re-plans with notes, which keeps tasks that ran.
  - **Branches and proposals:** `y` merges a review branch (`merge_branch`, recorded in `swarm_reviews`) or accepts a proposal. `x` sets a branch aside or drops a proposal.
  - **Running:** `space` pauses or resumes. `r` retries (`reopen_task`; a stalled task's worker is stopped first).
  - **Closing:** `y` closes a finished swarm (`App::stop_swarm`, `record::end_swarm`).
  - **Editing the board:** `n`, `e` and `d` add, edit (`record::update_plan_task`) and delete (only a task that never started) tasks.
  - **Stopping:** `x` stops a task's worker or cancels an open task.

  A confirmation takes `y` only.
- **Writing:** `>` opens the command line, `m` opens it to the selected task's worker (or as a reply), and `b` opens it to everyone.
  - **Syntax:** a line reads `[!] [@who] [#topic] message` (`swarm_board::parse_command`).
  - **Addresses:** `@all`, or a task key whose worker is live (a separator-blind prefix or a launch id). Without one, the line goes to the selected task's worker, or to the asker when replying. `Tab` completes keys.
  - **Topic:** a leading `!` sends topic `urgent`, which the `PreToolUse` hook delivers mid-turn. Otherwise it's `answer` for a reply to a question and `directive` else, unless `#topic` names one.
  - **Delivery:** every message wakes an idle recipient.
- **Refresh:** the screen refreshes when the `trace_changes` feed advances (triggers on the bus tables, `swarms` and `swarm_tasks`) or a live session changes (`SwarmBoard::refresh_due`, at most one look per 500 ms), never on a timer. `Ctrl+R` and every action refresh at once.
- **Broadcast wakes.** `ControlRequest::WakeWorkspace { workspace, prompt, exclude }` wakes every live session of the workspace (and of the worktrees its tasks run in) the way `WakeSession` wakes one, except the sender; the response's `detail` counts how many were woken and how many queued until idle. The screen's `@all` (`b`), `agent-mux swarm broadcast --wake`, `agent-mux bus send --wake` without `--to` and `agent_mux_send_message(to = "*", wake = true)` all send it.
- **`max_workers` is enforced.** `SpawnWorker` (the screen's `n`/`r`, `agent_mux_delegate`, `agent-mux bus delegate`) refuses when the swarm's `max_workers` delegated workers are already live in the workspace (sessions in a bus group; the coordinator is not counted), with `worker cap reached: N/N …`; the task stays on the board for `r` later, and the title shows `N/cap Workers`. A swarm without a recorded cap has none.
- **The screen refreshes on change, not on a timer.** `SwarmView` keeps one store connection and, at most every 500 ms, asks whether anything moved: the `trace_changes` feed (schema `v18` adds triggers for `agent_messages`, `agent_tasks`, `agent_leases` and `swarm_state`), a live session's state or telemetry (a fingerprint, no store read), or a lease it shows expiring (`SwarmView::refresh_due`). `Ctrl+R` and every action refresh at once. `load_snapshot` is a read: expired leases are left out, pruning stays with the writers (`acquire_leases`, `split_tasks`). Timestamps handed to it are nanoseconds, like the rest of the store.
- **The orchestrated swarm (schema v21).** Design: `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`.
  - **Tables:** `swarms` holds one swarm's run (`planning` → `review` → `running`/`paused` → `integrating` → `finished`; one live per workspace; `owner_run` is the TUI that dispatches it). `swarm_tasks` extends `agent_tasks` with the plan's key, position, `depends_on`, acceptance, `proposed`, attempts and `nudged_ns`. The migration ends every coordinator-era swarm (`swarm_state`, `swarm_plans`).
  - **Code:** `bus::swarm::plan` parses and checks the planner's fenced `swarm-plan` answer, `bus::swarm::record` reads and writes the rows and applies the operator's `SwarmVerb`s, and `bus::swarm::engine::next_actions` is the dispatcher (`Spawn`, `Nudge`, `Advance`), a pure function. `bus::swarm::prompt` holds the worker's opening prompt and the nudge. `app::swarm_engine` launches the planner (a `LivePlan` with `swarm` set), turns its answer into the board and runs the dispatcher every second from `on_tick`. Spawns go through `SpawnWorker` with `opening_prompt`.
  - **Gate:** `plan_blocks` refuses claims until the swarm is past `review`.
  - **Control requests:** `StartSwarm { workspace, goal, planner, max_workers, isolation, gate, check }` and `SwarmAction { workspace, verb, task, notes }`.
  - **`agent-mux swarm start`** needs the running TUI. Without the control socket it refuses rather than recording a swarm nobody runs.

```
agent-mux swarm start <goal...> [--planner claude|codex|agy|opencode] [--workers N] [--isolation shared|worktree] [--no-plan-gate] [--check CMD] [--workspace DIR]
agent-mux swarm status|plan [--workspace DIR] [--db PATH] [--json]
agent-mux swarm approve|pause|resume|close [--workspace DIR] [--json]
agent-mux swarm replan <notes...>
agent-mux swarm retry|merge|set-aside|accept|drop <task> [--workspace DIR] [--json]
agent-mux swarm broadcast <message...> [--urgent] [--wake] [--workspace DIR] [--db PATH] [--json]
agent-mux swarm stop [--workspace DIR] [--db PATH] [--json]
```
