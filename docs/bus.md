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
| `agent_mux_swarm_plan` | The swarm coordinator submits its plan (`plan = {summary, tasks}`). agent-mux checks it (`record::submit_plan`, `plan::check` against `CheckEnv::for_workspace`): with errors nothing changes and the errors are answered, without them the tasks go on the board (`review`, or `running` with gate `off`) and the answer carries the plan as a table (`record::plan_table`) for the coordinator to show the operator. Refused once the plan is approved. CLI: `agent-mux swarm submit-plan --file`. |
| `agent_mux_swarm_start` | The caller's session becomes the coordinator of a new swarm in its workspace (`record::create_swarm` with no owner, `set_coordinator_launch`); the running TUI adopts and supervises it. Refused for a swarm worker or a session that already coordinates one. (`mcp::swarm_tools`) |
| `agent_mux_swarm_launch` | Approves a plan in review on the operator's quoted words (`operator_said`, required for the chat relay: `record::launch`, a `swarm_decisions` row `by = chat`) and asks for the workers of `tasks` (plan keys) or of every ready task (`swarm_dispatch.launched_by = coordinator`); the engine spawns them at its next pass. CLI: `swarm approve`, `swarm launch [<task>…]`. |
| `agent_mux_swarm_wait` | Blocks until the swarm logs an event after `after` (`swarm_events`: a task offered, spawned, done, failed, lost, a decision, the check) or `wait_ms` pass (default 5 min, at most 10: `view::wait_events`). A timeout is an answer, never an error: the coordinator reports to the operator and asks whether to keep waiting. CLI: `swarm wait`. |
| `agent_mux_swarm_board` | The board as the screen shows it (`view::board_view`): each task's shown state, worker, result, branch, question; ready tasks nobody launched; open questions; the last event id. CLI: `swarm status --json`. |
| `agent_mux_get_swarm` | One swarm as one trace (`tracing::swarm_report::report`): the swarm, its launches from `launches.metadata.swarm_id` (each with turns, tools, tokens, cost and `agent` observations), every task's state, the decisions, the log and the messages. Defaults to the caller's swarm; `swarm` names another, ended or not. CLI: `agent-mux trace swarm <id> [--tree\|--timeline\|--log\|--json]`. |
| `agent_mux_swarm_action` | Relays an operator decision: pause, resume, retry, cancel, merge (queued behind the check command when the swarm has one), force-merge (past it), set-aside, accept, drop, close, delete. Merge, force-merge, set-aside, cancel, close and delete need `operator_said`. Through the control socket when a TUI runs (`SwarmAction` with `by = chat`, `said`, `from`), else `record::apply`. |

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

How to use it: [swarm.md](swarm.md). The Swarm screen (`F1` → "Open swarm", `Mode::Swarm`) has the Agents layout. Its state and keys are in `src/app/swarm_board.rs` (`SwarmBoard`), its drawing in `src/ui/swarm_board.rs`, and its model in `src/bus/swarm/`. Design: `docs/superpowers/specs/2026-11-02-swarm-agents-layout-design.md` §2.

- **Layout:** `ui::draw` gives `Mode::Swarm` the sidebar (`swarm_board::draw_tree`, `SIDEBAR_WIDTH`) and the main pane (`draw_pane`).
  - A row with a live session draws it through the Agents screen's own pane, `ui::draw_session_pane`, so workers' PTYs, which are sized to the main pane, draw uncut. `App::sync_swarm_pane_selection` keeps `App::selected` on that session, so selection, search and key forwarding work.
  - Other rows draw the swarm's card (`draw_swarm_card`) or the task's card (`draw_task_card`).
  - The global status bar is the only footer, and actions report there (`App::set_swarm_status`).
- **Tree:** `SwarmBoard::rows` holds `Row::Swarm`, `Row::Coordinator`, `Row::Task` and `Row::Agent` (a worker's sub-agent: an `observations` row of type `agent` under its launch, `SwarmData.agents`, `sub_agents`).
  - **One swarm per screen** (coordinator-first §9.1): `SwarmBoard.scope` names it — the selected session's swarm by its launch row (`record::swarm_of_launch`, `App::open_swarm_view`), the swarm that needs the operator (`App::swarm_needs_first`), a group in the Agents sidebar (`GroupKind::Swarm`, `RowTag::SwarmGroup`, `Action::OpenSwarmGroup`, `App::open_swarm_view_on`), else the folder's newest live swarm; `o` opens the swarm picker (`Modal::Picker`, `SwarmBoard::open_picker`). Every swarm session carries a `GroupKind::Swarm` group (the goal as its title); the Agents sidebar lists those sessions only with `[agents] show_swarm = true` (`App::session_listed`), and then other swarms live there too.
  - **Cursor and folding:** the cursor is kept by row key across refreshes, `←`/`→` fold (a worker's ended sub-agents, `agent_unfolded`), and `sync_current` points the selected-swarm fields at the cursor's swarm.
  - **The swarm row's tabs** (`SwarmTab`: Board, Live, Timeline, Log, Activity, Changes; `1`–`6`): Live is the mosaic of every live terminal (`live_tiles`, `draw_live_mosaic`: a tile shows the bottom of a session's screen as text and never resizes a PTY; `in_mosaic` arrows, `Enter` connects), Timeline the lanes of `tracing::swarm_report::timeline` (`in_timeline`: `←→` pan, `-+` zoom, `tl_zoom`/`tl_pan`), Log the `swarm_events` rows (`in_log`: `t` cycles `LogKind`, `/` edits `log_filter`, `Enter` opens the row the event names), Changes the combined diff (`load_diff` without a task). A task row has Task, Live, Thread, Trace (`load_trace`, `swarm_report::launch_trace_lines`; a `Row::Agent` opens on its own rows) and Changes.
  - **Reconnect:** `Enter` on a worker whose session ended is `Outcome::Reconnect` → `App::reconnect_swarm_worker`: the launch row's provider and session id, `harness::Resume::Id`, a new session in the swarm's group with the worker's `SwarmLaunch`, the task tracked again under the new launch when it was not settled, event `task.reconnected`; agy is refused.
  - **Keys gone** (§9.4): `e`, `p`, `b`, `a` say to tell the coordinator; `n` is always a new swarm; the task form remains as the retry form only. `d` deletes the swarm (closing a live one first); traces and branches stay, worktree checkouts are removed.
  - **Plan checks:** computed for the selected swarm, against its own workspace (`current_workspace`).
  - **Live sessions:** the screen gets every one (`App::live_sessions_in(None)`), so workers in other workspaces are not taken for lost.
- **Pane focus:** `Enter` on a terminal sets `pane_focused`. Dispatch then forwards keys as `Action::ForwardBytes`, paste goes to the session, and `text_entry_active` holds back the shared verbs. The Focus chord (`ActionId::FocusAgents`) hands the keyboard back to the tree.
- **Needs you:** `swarm_board::needs_for` computes them per swarm. They show as `!` on task rows and `⚑n` on swarm rows, listed in full on the swarm's card, and `!` jumps to the next one. A question reads its first line that is not a header (`question_line`). The dispatcher totals them for the swarms this TUI runs (`App::swarm_needs`), which the Agents sidebar and the status bar show.
- **Cost:** a live session carries `priced` (`LiveSwarmSession`), so an unpriced harness shows `—` and a partly priced total `≥$x` (`ui::swarm_board::spend`).
- **Writing:** `>` opens the command line, `m` addresses it to the selected task's worker (or answers its open question), and `@all` addresses everyone (`swarm_board::parse_command`).
- **Refresh:** only when `trace_changes` advances or a live session changes (`SwarmBoard::refresh_due`), at most every 500 ms.
- **The coordinator agent** (schema v22, `swarm_coordinators`):
  - **Launch:** `App::start_swarm_coordinator` installs the `swarm-coordinator` skill and starts an interactive session on the swarm's coordinator harness (`swarms.planner_harness`) in the workspace, in the swarm's `GroupKind::Swarm` group with label `coordinator`. Its opening prompt is `bus::swarm::prompt::coordinator_prompt` (three lines from `prompts.toml` `[swarm] coordinator`: the role, the swarm id, the context file), with `$AGENT_MUX_SWARM_CONTEXT` (`PlannerContext`: goal, inventory, `limits`, `tools`, `screen`; also `$AGENT_MUX_SWARM_PLAN` for older skills), `$AGENT_MUX_SWARM_ID` and `$AGENT_MUX_WORKSPACE` set. `record::set_coordinator_launch` records it, also as `swarms.planner_launch`, so the `★` row draws its terminal. Any session that invokes the skill and calls `agent_mux_swarm_start` becomes a coordinator the same way.
  - **Re-plan:** the operator says it in the chat; `agent-mux swarm replan <notes>` (`SwarmVerb::Replan`) messages the coordinator the notes (topic `plan-changes`) and wakes it. If it is gone, a new one starts with the board and the notes in its context.
  - **Approve:** the coordinator relays the operator's yes with `agent_mux_swarm_launch(operator_said)`; an approval from the CLI tells the coordinator (`plan-approved`).
  - **Restart:** `SwarmVerb::Restart` (`r` on `★`, `agent-mux swarm restart`) starts a new one once the old one exited.
  - **Questions:** a worker's `to = "coordinator"` (MCP `agent_mux_send_message`, CLI `bus send --to coordinator`) resolves to its swarm's coordinator (`record::coordinator_address`). Such a question becomes the operator's need only after `STUCK_AFTER_NS` (5 min) unanswered.
  - **Events:** `App::tell_coordinator_events` batches task ends and proposals since `events_seen_ns` into one `swarm-events` message and one wake, at most every `EVENTS_EVERY_NS` (1 min), while the coordinator is idle and the swarm is running, integrating or finished.
  - **Slow plan:** a swarm still `planning` after `PLAN_TIMEOUT_NS` (15 min) lists a `coordinator` need.
- **Harnesses:**
  - **Workers' pool:** `swarm_coordinators.worker_harness`, comma separated, set from the launch form's "Workers may run on" or `swarm start --worker-harness agy,codex`. `record::worker_pool` reads it (the coordinator's harness alone when empty) and is the planner context's `harnesses`; its first, `record::worker_harness`, runs tasks that name none and is the context's `default_harness`. `plan::check` refuses a task on a harness outside the pool (`CheckEnv.pool`, filled by `record::submit_plan` and approval); a harness the operator gives a task (`record::update_plan_task`) joins the pool.
  - **Coordinator default:** Claude when installed (`App::default_swarm_harness`).
  - **Per-task changes:** a task's harness, agent and isolation change through `record::update_plan_task`, from the task form before the task starts, or from `r`. The retry form saves them, stops a still-open worker and reopens the task (`Outcome::RetryTask`).
  - **agy refusal:** `plan::check` refuses agy with an agent that edits or runs commands (`plan::writing_agents`, `agents::tools_write`). agy 1.4.12 gives a Markdown custom agent read-only tools: view_file and MCP, no write_file, edit_file or run_command, as seen in its own transcripts. The pair is also refused when a task is saved (the task and retry forms, `SwarmBoard.writing_agents`; a retry starts on Claude, `SwarmBoard::agy_trap`) and when a worker is spawned (`SpawnWorker`), so a retry or a delegation cannot start one.
- **Broadcast wakes.** `ControlRequest::WakeWorkspace { workspace, prompt, exclude }` wakes every live session of the workspace (and of the worktrees its tasks run in) the way `WakeSession` wakes one, except the sender; the response's `detail` counts how many were woken and how many queued until idle. The screen's `@all` (`b`), `agent-mux swarm broadcast --wake`, `agent-mux bus send --wake` without `--to` and `agent_mux_send_message(to = "*", wake = true)` all send it.
- **Recovery and the merge gate (schema v26).** A failed task (within `FAILED_FRESH_NS`) or a lost one (worker gone past `LOST_GRACE_NS`) is retried once by the engine (`recovery::retry`: a failure on another pool harness, a lost worker on its own), unless the operator stopped its worker (`recovery::hold`). With a check command, `merge` queues the branch, and the gate (`gate::test_branch`, `gate::land`) merges it once the check passes on the branch brought up to date with the workspace. One gate runs per swarm at a time. Events: `task.retried`, `task.lost`, `merge.queued`, `merge.started`, `merge.failed`, `merge.requeued`, `review.merged`. Guide: `docs/swarm.md`.
- **Coordinator-first (schema v24).** Design: `docs/superpowers/specs/2026-11-05-swarm-coordinator-first-design.md`. The coordinator launches and agent-mux supervises: a ready task with a free slot is first **offered** (`engine::Action::Offer`, `swarm_dispatch.offered_ns`, event `task.offered`, told to the coordinator in its `swarm-events` batch); a task someone asked for (`swarm_dispatch.launched_by`: the coordinator through `agent_mux_swarm_launch`, the operator through `swarm approve|launch` or the screen) spawns at the next pass; one offered longer than `DISPATCH_GRACE_NS` (2 min, `App::swarm_dispatch_grace_ns`) is started by the dispatcher itself. Side tables: `swarm_settings` (`gate_mode` `chat`|`off`, `headless`), `swarm_dispatch`, `swarm_decisions` (every operator-level decision: `verb`, `by` chat|tui|cli, `said`, the relaying `launch`; `record::decide`, `Decider`), `swarm_events` (the log: `record::event`, `events_after`, `recent_events`; kinds in `SwarmEvent`). Every launch of a swarm carries `launches.metadata.swarm_id`, `swarm_role`, `swarm_task`, `swarm_task_key` and `parent_launch` (`bus::swarm::SwarmLaunch`, `LaunchPlan.swarm`, `SpawnWorker.swarm`), so one query reads the swarm as a tree. New verbs `SwarmVerb::Launch` and `SwarmVerb::Cancel`; a coordinator relaying `launch`, `merge`, `set-aside`, `cancel`, `close` or `delete` must quote the operator (`SwarmVerb::needs_operator_words`).
- **The orchestrated swarm (schema v21).** Design: `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`.
  - **Tables:** `swarms` holds one swarm's run (`planning` → `review` → `running`/`paused` → `integrating` → `finished`; several may be live per workspace since schema v23, which dropped the unique `swarms_live` index; `owner_run` is the TUI that dispatches it). `swarm_tasks` extends `agent_tasks` with the plan's key, position, `depends_on`, acceptance, `proposed`, attempts and `nudged_ns`. The migration ends every coordinator-era swarm (`swarm_state`, `swarm_plans`).
  - **Code:** `bus::swarm::plan` checks a plan (`Plan`, `check`, `CheckEnv`; the fenced `swarm-plan` block remains the headless answer's shape), `bus::swarm::record` reads and writes the rows and applies the operator's `SwarmVerb`s, and `bus::swarm::engine::next_actions` is the dispatcher (`Offer`, `Spawn`, `Nudge`, `Advance`, `RunCheck`), a pure function. `bus::swarm::prompt` holds the coordinator's and the worker's opening prompts and the nudge; `bus::swarm::view` the board and the wait as the tools answer them. `app::swarm_engine` starts the coordinator, runs the dispatcher every second from `on_tick` and tells the coordinator the events. Spawns go through `SpawnWorker` with `opening_prompt` and `swarm`.
  - **Gate:** `plan_blocks(task_id)` refuses claims and spawns of a task until its own swarm is past `review`; a task on no swarm's board never waits.
  - **Several swarms (v23):** a swarm is found by its id: the screen's cursor (`SwarmBoard.swarm`, `App::swarm_action_on`), a task's `swarm_tasks` row (`record::swarm_of_task`), the coordinator's launch or `$AGENT_MUX_SWARM_ID` (`agent_mux_swarm_plan`, `swarm submit-plan`), or `--swarm ID` (`ControlRequest::SwarmAction.swarm`). Without one, `record::resolve_swarm` takes the workspace's only live swarm and lists them when there are several. Closing one (`App::stop_swarm_run`, `record::end_swarm`) stops its coordinator and its tasks' workers and cancels only its tasks; `StopSwarm`/`swarm stop` ends every swarm of the workspace.
  - **Proposals:** a swarm worker's `agent_mux_task_split`, `agent_mux_delegate`, `bus split` or `bus delegate` (`record::swarm_of_worker` finds its swarm by the caller's held task) spawns nothing. It adds `proposed` tasks with a key from the title (`record::propose`); with the gate off they are accepted at once. `accept` and `drop` decide them.
  - **The check:** with `check_cmd` set, once everything is settled the dispatcher answers `RunCheck`. `app::swarm_engine` writes a `running` marker into `check_result`, then runs `bus::swarm::check::run` on a thread (`sh -c` in the workspace, `CHECK_TIMEOUT` of 30 min, the last 40 lines kept) and stores the outcome. Only then does the swarm advance to `finished`. A marker older than the timeout is run again, and a retry clears the result.
  - **Control requests:** `StartSwarm { workspace, goal, planner, max_workers, isolation, gate, check, workers }` (`goal` empty for a conversation, set for a headless swarm) and `SwarmAction { workspace, swarm, verb, task, notes, by, said, from }`.
  - **`agent-mux swarm start`** needs the running TUI. Without the control socket it refuses rather than recording a swarm nobody runs.

```
agent-mux swarm start [<goal...>] [--coordinator claude|codex|agy|opencode] [--gate chat|off] [--worker-harness H[,H...]] [--workers N] [--isolation shared|worktree] [--check CMD] [--workspace DIR]
agent-mux swarm status|plan [--workspace DIR] [--db PATH] [--json]
agent-mux swarm approve|pause|resume|close|delete [--workspace DIR] [--json] [--said "<words>"]
agent-mux swarm launch [<task>...] [--said "<words>"]
agent-mux swarm wait [--timeout MS] [--after ID] [--json]
agent-mux swarm events [-n N] [--json]
agent-mux swarm replan <notes...>
agent-mux swarm retry|cancel|merge|force-merge|set-aside|accept|drop <task> [--workspace DIR] [--json] [--said "<words>"]
agent-mux swarm <verb> ... --as-coordinator --said "<words>"   # a coordinator without MCP, recorded by = chat
agent-mux swarm broadcast <message...> [--urgent] [--wake] [--workspace DIR] [--db PATH] [--json]
agent-mux swarm stop [--workspace DIR] [--db PATH] [--json]
agent-mux trace swarm <id> [--tree | --timeline | --log] [--json] [--workspace DIR]
agent-mux langfuse export-swarm <id> [--dry-run]
```

- **The trace (§7):** `swarm_events` is the log (`record::event`; `bus::store::update_task` writes `task.<status>` for a swarm's task and `send_message` writes `message` / `question.asked` / `question.answered` for a swarm's participants). `tracing::swarm_report` reads a swarm as one tree (launches by `launches.metadata.swarm_id`, turns from `trace_stats`, sub-agents from `observations.type = 'agent'`), draws the Timeline (`timeline`, `lane_cells`) and the Log (`log_lines`), and lists recent swarms for Heimdall's dossier (`swarms_since`). With Langfuse, `map::MapCtx::swarms` (learned from `StoreOp::Launch` rows, seeded from the store for a replay) puts a swarm's turns in `session.id = <swarm id>` with the `swarm` tags; `map::swarm_summary` is the summary trace, sent by `App::export_swarm_trace` when a swarm closes and by `langfuse::export_swarm` (`langfuse export-swarm`).
