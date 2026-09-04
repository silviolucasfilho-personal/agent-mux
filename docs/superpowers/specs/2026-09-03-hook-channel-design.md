# agent-mux Hook Channel — CLI lifecycle hooks as a second trace source

**Date:** 2026-09-03
**Status:** Implemented (plan: `../plans/2026-09-03-hook-channel.md`; Tasks 1–6 landed). Deviations from the design as verified at implementation are noted inline.
**Builds on:** local trace store (`2026-09-03-sqlite-trace-store-design.md`), correlation and assembly from the Langfuse-era design (`2026-08-30-langfuse-integration-design.md`), Antigravity usage side channel (`src/tracing/agy_usage.rs`)

## Purpose

Today every trace is reconstructed from the transcript file each CLI writes. That gives content, models, and token usage, but every timestamp is "when the line was written", tool failures are inferred, API failures never appear, subagent work is invisible, and finding the transcript in the first place needs an injected flag (Claude) or a directory watch (Codex, Antigravity). All three CLIs also offer **lifecycle hooks**: commands they run at session start, prompt submit, before and after each tool call, at turn end, and at session end, each receiving a JSON payload that names the session and the transcript. This design adds those hooks as a **second channel** into the same SQLite store. The transcript stays the source of content and usage; hooks add exact timing, failures, subagent structure, and a session id delivered by the CLI itself.

## Decision summary

**What was checked first.** The Langfuse repository does not use hooks for observability: for its own development it ships skills, an MCP server, and a generated Claude settings block, and its published Codex observability plugin reads rollout files, the approach agent-mux already uses. So there is no prior art to copy there; the hook facilities below were verified directly against Claude Code 2.1, Codex CLI 0.153, and agy 1.1.25.

**Decisions.**

1. **Hooks complement, never replace, the transcript pipeline.** None of the three hook payloads carries token usage, thinking, or per-request model names; those keep coming from transcripts and from agy's conversation database. A session traced with hooks alone would have structure but no cost.
2. **The hook command is the agent-mux binary itself** (`agent-mux trace hook <provider>`), located through the running executable's path. No helper script, no interpreter dependency, one code path on Linux, macOS, and Windows.
3. **Hooks write to the store directly**, into a new append-only `hook_events` table, instead of talking to the TUI over a socket. The store already tolerates a second writer (WAL, busy timeout, idempotent keys), it works when the TUI has already exited, and it gives `trace import`-style backfill for free. The running pipeline consumes the table as a side channel, exactly like the Antigravity usage reader.
4. **Zero CLI-config mutation stays the default.** Claude gets hooks per launch through `--settings` with inline JSON; Codex gets `notify` per launch through a `-c` override. Anything that needs a file in the user's home or a trust prompt is an explicit `agent-mux trace hooks install <provider>` the user runs once: tool-level Codex hooks and the Antigravity plugin.
5. **`--dangerously-bypass-hook-trust` is never injected.** Codex discovers hooks passed with `-c`, but treats them as untrusted; silently adding a flag with that name to a user's command line is not acceptable, so Codex tool hooks are opt-in with the normal `/hooks` trust step.
6. **A local OTLP receiver is deferred.** Codex's `[otel]` table and Claude Code's telemetry could deliver the CLIs' own token accounting to a local port. That means agent-mux listening on the network, which the store design avoided; transcript usage has been reliable, so this stays out of scope until it is not.

Alternatives rejected: a Unix-socket IPC from hook to TUI (dies with the TUI, needs a Windows named-pipe story, no backfill); making hooks the primary source and dropping transcript tailing (no usage, no content); one shell script per platform (interpreter drift, quoting, Windows).

## Requirements

1. **No new blocking.** A hook invocation never delays the agent by more than a hard cap of 200 ms and never returns a blocking exit code or decision, whatever the state of the store.
2. **Zero CLI-config mutation** for the per-launch channels; installers are explicit, reversible, and idempotent.
3. **Existing user hooks keep running.** Per-launch injection must not shadow hooks the user configured in their own settings.
4. **Same content policy.** Hook payloads pass through the same masking, `redact_literals`, truncation, and `content_mode` rules as transcript content; in metadata mode only names, ids, and timings are stored.
5. **Same fail-open invariant.** A missing, locked, or newer-schema database, or a hook binary that cannot load its config, degrades to "no hook events", never to a broken session.
6. **Idempotent.** Re-delivered or duplicated hook invocations converge on one row.
7. **Correlation improves, never regresses.** A hook-announced session id wins over the watch heuristics; the old mechanisms stay as fallbacks for launches where hooks are off or unavailable.

## Architecture

```
CLI process ──(hook event)──► agent-mux trace hook <provider> ──INSERT──► hook_events (SQLite, WAL)
                                                                              │
per-session pipeline task (existing) ─ tick ─► HookFeed.poll(launch_id | provider+session_id)
   correlate ◄── Announced session id / transcript path
   assembler ◄── attach_hook_event: exact times, failures, subagents, turn cross-checks
```

### The hook command

`agent-mux trace hook <claude|codex|codex-notify|agy> [--event <name>] --home <dir> [--launch <id>] [--content-mode <mode>] [--db <path>]`

- Reads one JSON object from stdin (Claude, Codex hooks, agy) or from the last argument (Codex `notify`, which passes the payload as argv and closes stdin). agy payloads do not name their event, so the agy registration passes `--event <name>` per handler.
- `--home` is baked into the registration so the hook resolves `~/.agent-mux/profiles.toml` and the database path even when the CLI clears the environment; Codex runs hooks with a replayed environment snapshot, and agy runs them from the `hooks.json` directory.
- Reads `AGENT_MUX_SESSION_ID` from its environment when present (Claude hooks inherit the session's environment, which agent-mux sets) and stores it as `launch_id`; `--launch` overrides it for registrations that can bake the id in.
- Normalizes the payload into a `HookEvent` (below), applies content policy, and inserts one row with `busy_timeout = 150 ms`. On any failure it exits 0 silently; with `AGENT_MUX_HOOK_DEBUG=1` it logs to stderr.
- Prints the platform's neutral response where one is required (agy expects a JSON object on stdout; Claude and Codex accept empty output) and always exits 0.

Budget: process start plus config load plus one insert is a few milliseconds; the 200 ms cap is the busy-wait ceiling.

### `hook_events`

```sql
CREATE TABLE hook_events (
  id              INTEGER PRIMARY KEY,
  key             TEXT NOT NULL UNIQUE,   -- UUIDv5(AMX_NS, "amx1|hook|{provider}|{session_id}|{event}|{tool_use_id|turn_key|step}|{ts_ns}")
  provider        TEXT NOT NULL,
  session_id      TEXT NOT NULL,          -- Claude session_id, Codex session_id / thread-id, agy conversationId
  launch_id       TEXT,                   -- from AGENT_MUX_SESSION_ID when the hook inherited it
  event           TEXT NOT NULL,          -- SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PostToolUseFailure,
                                          -- SubagentStart, SubagentStop, Stop, StopFailure, PostCompact, PostModelSwitch,
                                          -- SessionEnd, Interrupt, TurnComplete (Codex notify), PreInvocation, PostInvocation
  ts_ns           INTEGER NOT NULL,       -- hook process wall clock
  cwd             TEXT,
  transcript_path TEXT,
  turn_key        TEXT,                   -- Claude prompt_id, Codex turn_id, agy invocationNum
  tool_use_id     TEXT,
  tool_name       TEXT,
  agent_id        TEXT,
  agent_type      TEXT,
  step_index      INTEGER,                -- agy stepIdx
  model           TEXT,                   -- Codex `model`, agy `modelName`, PostModelSwitch new_model
  is_error        INTEGER NOT NULL DEFAULT 0,
  payload         TEXT NOT NULL DEFAULT '{}' -- masked, bounded, per-event whitelist (below); '{}' in metadata mode
);
CREATE INDEX hook_events_launch  ON hook_events (launch_id, id);
CREATE INDEX hook_events_session ON hook_events (provider, session_id, id);
CREATE INDEX hook_events_ts      ON hook_events (ts_ns DESC);
```

Schema version 2. Retention and `trace prune` delete hook rows by `ts_ns` alongside everything else; `trace export` includes the table.

`payload` keeps only what the pipeline uses, never the raw hook JSON: `tool_input`, `tool_result` / `tool_response`, `tool_error`, `prompt`, `last_assistant_message`, `result`, `error_type`, `reason`, `source`, `compaction_trigger`, `old_model`, `input-messages`, `terminationReason`. Full mode masks and truncates each string at `content_max_bytes`; metadata mode stores only `error_type`, `reason`, `source`, `compaction_trigger`, and model names.

### The pipeline side channel: `HookFeed`

Each pipeline owns a `HookFeed` created at `start_session`, polled on every tick after the tail poll, and once more in `finalize`. It reads `hook_events WHERE launch_id = ?1 AND id > ?2`; once a session id is known it also reads `WHERE provider = ?1 AND session_id = ?2 AND id > ?3`, which covers hooks that could not inherit the launch id (agy plugin, Codex when the snapshot lacks the variable). Rows are handed to the assembler, which attaches them:

| Hook event | Attachment |
|---|---|
| `SessionStart` | Announces `session_id`, `transcript_path`, `cwd` → correlation (below). `source` (`startup`, `resume`, `clear`, `compact`, `fork`) → launch metadata. |
| `UserPromptSubmit` | Remembered by `turn_key`; the next turn opened by a User transcript event takes this `ts_ns` as `start_ns` when it is earlier, and records `turn_key` in trace metadata. |
| `PreToolUse` | Tool observation for `tool_use_id` gets exact `start_ns`; held until the ToolUse transcript event if it arrives first. Codex: `tool_name` `apply_patch` maps to the transcript's tool name. |
| `PostToolUse` / `PostToolUseFailure` | Exact `end_ns`; `PostToolUseFailure` sets `is_error`, `level = ERROR`, `status_message = tool error`, and `metadata.tool_error` (full mode). |
| `Stop` | Turn `end_ns` for the open turn; `turn_number` cross-checked against the ordinal and any mismatch recorded as `metadata.turn_number_hook`; `last_assistant_message` fills `traces.output` only when the transcript gave none. |
| `StopFailure` | Open turn closes as `aborted` with `metadata.api_error = error_type`. |
| `SubagentStart` / `SubagentStop` | Creates or updates an `agent` observation keyed by `agent_id` (id = `span_id_hex("{trace_key}|agent|{agent_id}")`), exact start and end, `agent_type`, `result` in full mode. When the transcript's own `Task` tool row for that subagent closes with `structured.agentId`, the two are merged onto the transcript row's id and the hook-created row's `parent_id` points to it. |
| Tool events carrying `agent_id` | Observations under the agent row: `parent_id` = the agent observation, id = `span_id_hex("{trace_key}|agent|{agent_id}|tool|{tool_use_id}")`, name = `tool_name`, times from Pre/Post. This is the only source of subagent tool activity, since subagent transcripts are separate files the pipeline does not tail. |
| `PostCompact` | `traces.metadata.compacted = true` on the open turn; `PreCompact` is not registered. |
| `PostModelSwitch` | `sessions.extra.model` updated; the next generations without a transcript model inherit it. |
| `Interrupt` (Codex) | Open turn closes as `aborted` with `metadata.interrupted = true`. |
| `SessionEnd` | `launches.metadata.session_end_reason`; when the launch's own termination is still unknown at that moment it becomes `exit`. |
| `TurnComplete` (Codex notify) | Turn `end_ns` when the transcript's `task_complete` is late or missing; `thread-id` announces the session. |
| `PreInvocation` / `PostInvocation` (agy) | Generation start and end for the step that follows, paired with the `gen_metadata` record by step index. |

Rows for steps the pipeline never emits (primed history on resume) stay parked, the same rule as usage records.

### Correlation upgrade

Announcement is checked before the existing spec on every tick (`Pipeline::try_announced`, implemented in the pipeline rather than as a `CorrelationSpec` variant so the existing specs stay untouched): it reads `hook_events` for a `SessionStart` (or any event) row with this launch id, adopts its `transcript_path` when the file exists, and only then falls back to the existing logic. Outcomes: `correlation = "announced"` for hook-adopted sessions; the launch row's `correlation_plan` records `announced+deterministic`, `announced+watched`, or the old values when hooks are off.

Consequences per provider:

- Claude no longer needs `--session-id` injection to know the transcript. Injection stays on by default for one release as belt and braces (it also makes the id predictable for the browser before the first hook fires), and `inject_session_id = false` becomes safe to recommend. Bare `-r`, `--continue`, and `-p` launches, which were launch-only before, now correlate.
- Codex `notify` delivers `thread-id`; the rollout filename ends in that id, so the transcript is found by glob `sessions/**/rollout-*-<thread-id>.jsonl` without the cwd heuristic. The cwd watch stays as the fallback for launches without `notify`.
- Antigravity hooks deliver `conversationId` and `transcriptPath`, replacing the brain-directory heuristic when the plugin is installed.

## Per-CLI integration

### Claude Code: per-launch `--settings`

`plan_launch` appends `--settings <inline JSON>`. `--settings` accepts a file path or inline JSON, applies for that session only, and merges over the user's files without writing anything. Registered events and one handler shape:

```json
{"hooks":{
  "SessionStart":       [{"hooks":[H]}],
  "UserPromptSubmit":   [{"hooks":[H]}],
  "PreToolUse":         [{"matcher":"","hooks":[H]}],
  "PostToolUse":        [{"matcher":"","hooks":[H]}],
  "PostToolUseFailure": [{"matcher":"","hooks":[H]}],
  "SubagentStart":      [{"hooks":[H]}],
  "SubagentStop":       [{"hooks":[H]}],
  "Stop":               [{"hooks":[H]}],
  "StopFailure":        [{"hooks":[H]}],
  "PostCompact":        [{"hooks":[H]}],
  "PostModelSwitch":    [{"hooks":[H]}],
  "SessionEnd":         [{"hooks":[H_SYNC]}]
}}
H      = {"type":"command","command":"/abs/agent-mux","args":["trace","hook","claude","--home","/home/me"],"async":true,"timeout":5}
H_SYNC = same without "async" and with "timeout":1   (SessionEnd hooks share a 1.5 s budget)
```

Exec form with `args` avoids shell quoting on every platform. `async: true` means the CLI never waits; SessionEnd is synchronous because the process is exiting.

**User hooks.** `--settings` overrides a key it sets. Whether Claude Code merges the `hooks` arrays from the inline JSON with those in the user's files, or replaces the key wholesale, is not stated precisely enough to rely on. Until verified at implementation, `plan_launch` reads `~/.claude/settings.json`, `.claude/settings.json`, and `.claude/settings.local.json`, and merges any `hooks` they define into the inline JSON so nothing is shadowed. If verification shows the arrays merge natively, the manual merge is dropped.

The payload's common fields (`session_id`, `transcript_path`, `cwd`, `prompt_id`, `agent_id`, `agent_type`) map onto `hook_events` directly; `tool_use_id` is present on the tool events; `Stop` carries `last_assistant_message` and `turn_number`; `StopFailure` carries `error_type`.

### Codex: per-launch `notify`, opt-in hooks

**Per launch.** `plan_launch` adds `-c 'notify=["/abs/agent-mux","trace","hook","codex-notify","--home","/home/me","--launch","<launch_id>"]'`. `notify` is honored from the runtime layer, fires `agent-turn-complete` with `thread-id`, `turn-id`, `cwd`, `input-messages`, and `last-assistant-message` as one JSON argument, and is fire-and-forget. If the user's `~/.codex/config.toml` already sets `notify`, the plan reads it and the hook re-invokes that program with the same payload after recording its row, so the user's notification keeps working.

**Opt-in hooks.** `agent-mux trace hooks install codex` merges our handlers into `~/.codex/hooks.json` (Codex's verified format: `{"hooks": {"<Event>": [{"matcher"?, "hooks": [handler]}]}}`, a handler being `{"type": "command", "command", "commandWindows", "timeout", "async"?, "statusMessage"}`), registering SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, SubagentStart, SubagentStop, Stop, Interrupt, PostCompact, and SessionEnd with `async = true` where allowed (SessionEnd and Interrupt run synchronously inside their one-second default budget). Handlers are recognized by the `trace hook codex` marker in their command line, so reinstalling replaces only ours and uninstalling leaves every other hook (and unknown top-level keys) intact. Codex then requires the user to review and trust the entry in `/hooks`; the installer prints exactly that. Codex records trust against the hook's hash under keys its documentation does not specify, so `trace hooks status` and `trace doctor` only report whether any `hooks.state` entries exist in `config.toml` and otherwise defer to `/hooks`. The payload's `session_id`, `transcript_path`, `turn_id`, `tool_use_id`, `tool_name`, `tool_input`, `tool_response`, `model`, and for subagents `agent_id`, `agent_type`, and `agent_transcript_path`, map onto the table.

### Antigravity: opt-in plugin

agy loads `hooks.json` only from a customization root: `.agents/` in the workspace, `~/.gemini/config/`, or a registered plugin. There is no per-launch mechanism, so this channel is `agent-mux trace hooks install agy`, which:

1. Writes the plugin directly into the global customization root, `~/.gemini/config/plugins/agent-mux/`, as `plugin.json` (`{"name": "agent-mux"}`) plus `hooks.json`. agy's bundled plugin guide states that a plugin is any subdirectory of a `plugins/` folder in a customization root and that `~/.gemini/config/` is the global root, so no `plugins.json` edit is needed and the user's own config files stay untouched. (The design originally placed the directory under `~/.agent-mux/hooks/` with a `plugins.json` registration; the standard root is simpler and avoids interpreting `entries.path`.)
2. `agy plugin list` shows it; `config.json`'s `plugins` map (`agent-mux: {enabled}`) is the user's switch and is never written by the installer. Uninstall removes the directory.

`hooks.json` uses agy's verified format: the top-level key is the hook name, tool events (`PostToolUse`) are matcher groups (`{"matcher": "*", "hooks": [handler]}`), and `PreInvocation`, `PostInvocation`, and `Stop` are flat handler lists. A handler is `{"type": "command", "command": "'/abs/agent-mux' trace hook agy --event <E> --home '/home/me'", "timeout": 5}`; agy runs it through `sh -c` with the `hooks.json` directory as cwd, sends the payload on stdin, and reads a JSON object from stdout.

`hooks.json` registers **PostToolUse**, **PreInvocation**, **PostInvocation**, and **Stop**. PreToolUse is deliberately not registered in v1: agy requires a `decision` in its response, and every documented value changes permission behavior; it is registered only if implementation testing shows an empty response leaves the default flow untouched. agy hooks run synchronously with a 30 s default timeout, so the hook's few milliseconds matter and the `--home` argument keeps it from needing the environment. The hook prints `{}` for PostToolUse, PreInvocation, and PostInvocation, and `{"decision": "proceed"}` for Stop, which by contract allows the agent to stop. Payload fields: `conversationId`, `transcriptPath`, `workspacePaths`, `modelName`, `stepIdx`, `error`, `invocationNum`, `terminationReason`.

## Content policy

The hook binary loads the same resolved configuration as the TUI. Full mode masks `tool_input`, `tool_result`, `tool_error`, `prompt`, `last_assistant_message`, `result`, and `input-messages` with `mask_secrets`, `redact_literals`, and `truncate_content`; metadata mode drops them. Tool names, ids, timings, models, error types, and reasons flow in both modes, matching the transcript rules. A per-profile `content_mode` override is respected because the launch registration passes `--content-mode` alongside `--home`.

## Config

```toml
[tracing]
hooks = "auto"     # "auto" (default): per-launch channels that touch no user files
                   # "off": never register hooks, never read hook_events
                   # "installed": auto + honor installer-managed hooks (informational; installers work either way)
```

Per-profile `[profiles.tracing] hooks = "off"` disables injection for one profile, for wrapper commands that reject unknown flags. `provider = "none"` already disables everything.

## Failure posture

| Failure | Behavior |
|---|---|
| Store locked when the hook runs | Wait up to 150 ms, then drop the row and exit 0. The transcript still carries the event. |
| Store missing, unwritable, or newer schema | Exit 0, nothing written; `trace doctor` shows the cause. |
| Hook binary path changed after install (upgrade, move) | Codex and agy installers record the absolute path; `trace doctor` flags a stale path; `trace hooks install` rewrites it. Per-launch registrations always use the current executable. |
| Claude ignores or rejects `--settings` (old binary) | The launch still runs; the transcript pipeline is unchanged. A `--settings`-rejecting binary fails fast like the injected-flag case and follows the same two-strike disable per profile. |
| Codex `-c notify` overrides a user notify | Chained as described; if the user's program is missing, the row is still recorded. |
| Hook rows arrive for a launch id no pipeline owns (TUI gone, session traced by installed hooks alone) | Rows accumulate and are visible through `trace sql`; `trace import` on the transcript later attaches them. A full hook-only ingestion of sessions started outside agent-mux is out of scope. |
| Duplicate delivery | The `key` uniqueness collapses it. |
| Hook timing budget exceeded by the platform | The CLI kills the hook; nothing blocks. |

## CLI additions

| Command | Purpose |
|---|---|
| `trace hook <source> [--event E] --home <dir> [--launch <id>] [--content-mode …] [--db <path>]` | The hook entry point; not for interactive use. |
| `trace hooks install <codex\|agy>` / `uninstall` / `status` | The opt-in installers, their removal, and a per-provider report: registered path, trust state (Codex), plugin registration (agy), events seen in the last 24 h. |
| `trace doctor` | New section: hook channel mode, per-provider readiness, rows in `hook_events` per provider, last event age for live launches, stale binary path. |
| `trace show <trace>` | Marks observations whose times came from hooks (`⏱` in the browser, `hook` column in the CLI) and lists subagent children indented under their agent row. |

## Tests

- **hook command**: payload fixtures per provider and event (Claude tool/turn/subagent/session events, Codex hook and notify payloads, agy PostToolUse/PreInvocation/Stop) → normalized rows; masking in full mode and absence in metadata mode; dedupe key stability; exit 0 with the store locked by another connection, within the budget; agy stdout contract.
- **feed / assembler**: Pre before and after the ToolUse event pin the same row; PostToolUseFailure flips level; Stop cross-check; StopFailure aborts; subagent creation, nesting, and merge onto the transcript's Task row; PostModelSwitch model inheritance; parked rows for primed history.
- **correlation**: an announced session adopts before the watch does; announcement for a transcript that does not exist yet waits for the file; claim registry still prevents double adoption.
- **plan_launch**: Claude `--settings` JSON shape, exec form, async flags, SessionEnd sync, merge of user hooks; Codex `-c notify=` value is valid TOML and chains an existing notify; profile `hooks = "off"` removes both.
- **installers**: Codex `hooks.json` merge is idempotent and preserves other entries; agy plugin directory under the global root; uninstall restores the previous state (and removes a file that held only our entries); status flags a stale binary path.
- **e2e** (`tests/trace_hooks.rs`, unix): the fake `claude` script invokes `agent-mux trace hook claude` with sample payloads around the transcript lines it writes; assert `correlation = "announced"`, tool rows with exact hook times, one subagent row with a nested child, and `session_end_reason` on the launch.

## Files

| File | Change |
|---|---|
| `src/tracing/hooks/mod.rs` (new) | `HookEvent` model, per-provider payload parsers, content policy, dedupe key. |
| `src/tracing/hooks/feed.rs` (new) | `HookFeed` reader; announcement lookup. |
| `src/tracing/hooks/install.rs` (new) | Codex and agy installers, status, uninstall. |
| `src/tracing/store/schema.rs` | migration v2: `hook_events` and indexes. |
| `src/tracing/store/mod.rs` | `insert_hook_event`; prune and export coverage. |
| `src/tracing/correlate.rs` | `CorrelationSpec::Announced`, `poll` checks it first. |
| `src/tracing/map.rs` | `attach_hook_event`, subagent rows and nesting, hook-time pinning, `turn_key` bookkeeping. |
| `src/tracing/mod.rs` | Claude `--settings` and Codex `notify` extras in `plan_launch`; feed polling in `tick` and `finalize`; `correlation_plan` values. |
| `src/tracing/cli.rs` | `hook`, `hooks install/uninstall/status`, doctor section, `show` markers. |
| `src/config.rs` | `hooks` setting, per-profile override. |
| `src/ui.rs` | browser: hook-timed marker, nested subagent rows. |
| `profiles.example.toml`, this spec's plan | docs. |

## Order of work

1. **Schema v2, hook command, Claude per-launch injection, feed with tool and turn pinning, announced correlation.** The largest and most valuable step; independently shippable.
2. **Codex `notify`** per launch with user-notify chaining.
3. **Subagent nesting** for Claude (rows, `parent_id`, merge with the Task row) and the browser's indented view.
4. **Codex hooks installer** with trust-state reporting.
5. **Antigravity plugin installer**, PreInvocation/PostInvocation pairing with usage records, and the PreToolUse verification.
6. **Doctor, docs, example config.**

Rough scale: step 1 about the size of the Antigravity usage change plus the table and installer scaffolding; steps 2 to 6 smaller.

## Out of scope

- A local OTLP receiver for the CLIs' own telemetry.
- Hook-only ingestion of sessions started outside agent-mux.
- Using hooks to block, modify, or gate anything: agent-mux hooks only observe.
- Codex `PermissionRequest`, `PreCompact`; Claude `MessageDisplay`, `Notification`, `PostToolBatch`, permission events: no trace value in v1.

## Risks

1. **Hook contracts drift.** All three are versioned by their vendors and the Codex schema page warns that main-branch fields may not be released. Parsers are lenient, unknown fields are ignored, and a payload that does not parse yields no row rather than an error.
2. **`--settings` shadowing user hooks** if the merge assumption is wrong. Mitigated by the manual merge until verified.
3. **Two writers.** The hook binary and the TUI's writer thread contend for the WAL lock under bursty tool use; busy timeouts on both sides and the hook's drop-on-timeout keep the agent unaffected, at the cost of an occasional missing hook row.
4. **Antigravity synchronous hooks** add a few milliseconds per tool call and per model invocation; the plugin is opt-in for that reason.
5. **Process spawn per event** on Claude: one hook process per registered event, roughly a dozen per turn; measured cost is milliseconds, and `async` keeps it off the critical path.

## Decisions to confirm

- **D1** Register Claude hooks for `-p` launches too. They produce no transcript worth tailing beyond the `-p` result, but hooks give the session shape; default yes.
- **D2** Keep `--session-id` injection on for one release alongside announcements, then flip the default to off.
- **D3** The `hooks = "auto"` default: on. Alternative: off until the user opts in, mirroring the old opt-in posture.
