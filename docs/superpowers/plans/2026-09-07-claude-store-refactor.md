# Claude capture and store refactor: implementation plan

Design: `../specs/2026-09-07-claude-store-refactor-design.md`.

Each phase lands on its own, with the store still readable between phases. Schema changes are append-only migrations from v4.

## Phase 1 — Identity and the tree

### Task 1: Content-derived ids (`src/tracing/ids.rs`, `src/tracing/map.rs`)

- [x] Turn id becomes `hash(provider, session_id, user_row_uuid)`; the ordinal stays as a display column.
- [x] Observation id becomes `hash(turn_id, message_id | tool_use_id | agent_id | index)`.
- [x] Delete `ordinal_salted` and the salting path; a truncated backfill no longer needs it.
- [x] Tests: the same transcript replayed twice yields identical ids; a fork's copied history yields the parent's ids for shared rows.

### Task 2: Parent links from the transcript DAG (`src/transcript.rs`, `src/tracing/map.rs`)

- [x] Surface `uuid`, `parentUuid` and `sourceToolAssistantUUID` on the parsed events.
- [x] Parent a tool observation to the generation whose message issued its `tool_use`.
- [x] Remove the `parent_id: None` hardcodes; keep the hook path as a fallback only.
- [x] Tests: a turn with two generations and three tool calls produces the right tree; the existing flat expectations are updated.

## Phase 2 — Subagents

### Task 3: Sidecar discovery and tailing (`src/tracing/subagents.rs`)

- [x] Watch `<transcript-stem>/subagents/` for `agent-*.meta.json` and `agent-*.jsonl`.
- [x] One tailer per sidecar, with its own offset, polled with the parent.
- [x] Read `agentType`, `description`, `toolUseId`, `parentAgentId`, `spawnDepth`, `model`.

### Task 4: Nested ingestion (`src/tracing/map.rs`)

- [x] Parse a sidecar with the existing Claude parser into observations under an `agent` span.
- [x] Parent the span to the launching tool call via `toolUseId`; recurse via `parentAgentId`.
- [x] A missing sidecar leaves the span with unknown tokens, never zero.
- [x] Tests: depth-1 and depth-2 fixtures; per-subagent tokens and cost roll up to the turn.

## Phase 3 — Record coverage

### Task 5: The dropped record types (`src/transcript.rs`, `src/tracing/map.rs`)

- [x] `ai-title` becomes the session title, first prompt as fallback.
- [x] `system/compact_boundary` becomes an event observation and puts `trigger`, `preTokens`, `postTokens` on the turn.
- [x] `pr-link` and `file-history-*` become turn metadata.
- [x] `attachment/skill_listing` becomes the session's skill inventory.
- [x] `attachment/remote_session_change` records the web session URL.
- [x] Tests: one fixture per record type, taken from real transcript lines.

## Phase 4 — Usage, cost and cleanups

### Task 6: Provided versus computed usage (`src/tracing/store/schema.rs` v5, `src/tracing/map.rs`)

- [x] Add `provided_usage`, `provided_cost` JSON columns; keep the scalars as projections of the computed values.
- [x] Apply the all-or-nothing rule: a provided cost suppresses computed pricing.

### Task 7: Export key vocabulary (`src/tracing/langfuse/map.rs`)

- [x] Rename to `cache_read_input_tokens`, `input_cache_creation_5m`, `input_cache_creation_1h`, `output_reasoning_tokens`.
- [x] Tests: the emitted map matches the names Langfuse prices for Claude.

### Task 8: Errors, latency and time (`schema.rs`, views, `src/tracing/cli.rs`, `src/ui.rs`)

- [x] Drop `is_error`; migrate to `level = 'ERROR'`; a decline becomes `WARNING`.
- [x] Clamp derived latency at zero in the views.
- [x] One local-time formatter used by the CLI and the browser.

### Task 9: Scores (`schema.rs` v5, `src/tracing/scores.rs`)

- [x] Add `data_type`, `source`, `string_value`, and an `observation` target.
