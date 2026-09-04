# agent-mux Hook Channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the CLIs' lifecycle hooks (Claude Code, Codex, Antigravity) as a second trace source into the local SQLite store: exact tool and turn timing, tool and API failures, subagent structure, and CLI-announced session ids. Transcripts remain the source of content and usage.

**Architecture:** The CLIs invoke `agent-mux trace hook <provider>`, which normalizes the payload, applies the content policy, and inserts one row into a new `hook_events` table. Each pipeline polls that table as a side channel (like `agy_usage`), adopts announced transcripts ahead of the watch heuristics, and pins or creates observations by `tool_use_id`, `turn_key`, `agent_id`, or `stepIdx`. Claude gets hooks per launch via `--settings` inline JSON; Codex gets `notify` per launch via `-c`; Codex tool hooks and the Antigravity plugin are explicit `trace hooks install` opt-ins.

**Tech Stack:** existing deps only.

**Spec:** `docs/superpowers/specs/2026-09-03-hook-channel-design.md` (read it first; it is authoritative on every behavior below)

---

### Task 1: Schema v2 + hook command (`src/tracing/store/schema.rs`, `src/tracing/hooks/mod.rs`, `src/tracing/cli.rs`)

**Interfaces:** `hook_events` table + indexes (migration v2); `HookEvent { provider, session_id, launch_id, event, ts_ns, cwd, transcript_path, turn_key, tool_use_id, tool_name, agent_id, agent_type, step_index, model, is_error, payload }`; `parse_claude / parse_codex / parse_codex_notify / parse_agy(payload) -> Option<HookEvent>`; content policy (`mask_secrets`, `redact_literals`, `truncate_content`, metadata-mode whitelist); dedupe `key = UUIDv5(...)`; `Store::insert_hook_event`; `trace hook <source> [--event E] [--home <dir>] [--launch <id>] [--content-mode …] [--db <path>]` reading stdin or the last argv (`--event` names the event for agy, whose payload does not), exit 0 always, 150 ms busy cap, agy stdout contract.

- [x] **Step 1:** Fixture payloads per provider/event; tests: normalization, masking/metadata mode, dedupe, locked-store exit 0 within budget, agy responses
- [x] **Step 2:** Implement; migration test from a v1 fixture database

### Task 2: Feed + attachment + announced correlation (`src/tracing/hooks/feed.rs`, `map.rs`, `correlate.rs`, `mod.rs`)

**Interfaces:** `HookFeed::{new(launch_id), set_session(provider, id), poll(conn) -> Vec<HookEvent>}`; `TurnAssembler::attach_hook_event` (Pre/Post pinning either order, failure level, Stop cross-check, StopFailure abort, PostCompact, PostModelSwitch, SessionEnd reason); `CorrelationSpec::Announced` checked first in `poll`; `correlation = "announced"`, `correlation_plan = "announced+…"`.

- [x] **Step 1:** Tests: pinning both orders, failure flags, Stop/turn_number, StopFailure, parked rows for primed history, announced adoption beats the watch, claim registry
- [x] **Step 2:** Implement; feed polled in `tick` and `finalize`. Announced adoption lives in the pipeline (`try_announced`, checked before `correlate::poll`) rather than as a `CorrelationSpec` variant; turn-level hooks pick their turn by timestamp so a `Stop` polled after the transcript opened the next turn still lands on the right one; `launches.metadata` (schema v2) carries the session start source, end reason, and hook counts.

### Task 3: Claude per-launch `--settings` (`src/tracing/mod.rs`, `src/config.rs`)

**Interfaces:** inline JSON registration (exec form, `async: true`, SessionEnd sync 1 s, `--home`, `--content-mode`); merge of user `hooks` from `~/.claude/settings.json`, `.claude/settings.json`, `.claude/settings.local.json` (drop once native array merge is verified); `[tracing] hooks = auto|off|installed`, per-profile `hooks = "off"`; two-strike disable on fast failure shared with injection.

- [x] **Step 1:** Tests: JSON shape, user-hook merge, profile off, `-p`/`-r`/`--continue` launches now correlate
- [x] **Step 2:** Implement; e2e `tests/trace_hooks.rs` (fake claude invoking the hook command around its transcript lines). Registration lives in `src/tracing/hooks/register.rs`; `[tracing] hooks` and the per-profile `hooks = "off"` are in `config.rs`; the launch env also carries `AGENT_MUX_EXE`; `correlation_plan` reads `announced+<plan>`; the two-strike fast-failure disable now covers both injected flags. A `UserPromptSubmit` that lands up to 2 s after the transcript's user line still pins the turn (coarse clocks).

### Task 4: Codex `notify` per launch (`src/tracing/mod.rs`, `src/tracing/hooks/mod.rs`)

- [x] **Step 1:** Tests: `-c notify=[...]` value is valid TOML; existing user `notify` is chained; `thread-id` announces and the rollout glob `rollout-*-<thread-id>.jsonl` adopts; `TurnComplete` pins turn end
- [x] **Step 2:** Implement (`register::codex_notify_override`; the hook command's `--chain <json argv>` re-invokes the user's notify program with the payload)

### Task 5: Subagent nesting (`map.rs`, `ui.rs`, `cli.rs`)

- [x] **Step 1:** Tests: SubagentStart/Stop create the agent row; child tool events nest under it via `parent_id`; merge onto the transcript's Task row by `structured.agentId`; browser and `trace show` render children indented
- [x] **Step 2:** Implement. The hook-created agent row keeps its own id (`…|agent|<agent_id>`) and is re-parented under the transcript's Task row when that row's result names the agent; child tool rows are `…|agent|<agent_id>|tool|<tool_use_id>`. `query::list_observations` returns tree order with a `depth` field, which `trace show` and the browser indent.

### Task 6: Installers + doctor (`src/tracing/hooks/install.rs`, `cli.rs`)

**Interfaces:** `trace hooks install|uninstall|status codex` (merge a named `agent-mux` entry into `~/.codex/hooks.json`, `commandWindows`, trust guidance, trust state read from `hooks.state.<key>`); `trace hooks install|uninstall|status agy` (plugin dir under `~/.agent-mux/hooks/agy-plugin/`, `plugins.json` registration, `agy plugin install` fallback, PostToolUse/PreInvocation/PostInvocation/Stop only, PreToolUse gated on verification); doctor section (mode, readiness, rows per provider, last event age, stale binary path).

- [x] **Step 1:** Tests: idempotent merge preserving other entries; uninstall restores; agy registration; status against fixture homes (stale binary path)
- [x] **Step 2:** Implement; `profiles.example.toml` and spec status updated. The agy plugin is written straight into the global root `~/.gemini/config/plugins/agent-mux/` (no `plugins.json` edit; see the spec). Codex trust state is reported as present/absent only, since the config keys are undocumented.
