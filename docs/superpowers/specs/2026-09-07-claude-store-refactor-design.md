# Refactoring the Claude capture path and the SQLite store

**Status:** Design (plan: `../plans/2026-09-07-claude-store-refactor.md`).
**Request (2026-09-07):** "a complete refactory for the sqlite option for claude. First check what langfuse do" — against `langfuse/Claude-Observability-Plugin` and the Langfuse monorepo.
**Builds on:** the SQLite trace store (schema v4), the hook channel, the transcript parser, the Langfuse backend, and the trace/skills views.

## Purpose

The store records Claude sessions, and the record is wrong in ways that show up the moment anyone reads it. The tree view is flat because no observation has a parent. Subagent work is absent entirely. Sessions are titled `/model` because the title is taken from the first prompt. The Loop view reports zero subagents on a turn that ran several. None of this is a rendering problem; the rows themselves are missing or unlinked.

Two reference implementations were read before designing the replacement: Langfuse's own Claude Code plugin, which solves exactly this capture problem, and the Langfuse data model, which is the most mature answer to how agent telemetry should be shaped. This design takes what each got right and states plainly where agent-mux should differ.

## What is actually broken, with evidence

Measured on this machine: 22,038 transcript lines across all projects, and the live store.

| Defect | Evidence |
|---|---|
| The observation tree is never built | 8,989 observations, **0** with a parent. `parent_id` is hardcoded `None` in all three row builders (`map.rs` gen/tool/usage rows); the only code that sets it hangs off `SubagentStart`, which Claude never emits |
| Subagent work is not captured | 22 subagent transcripts exist on disk carrying 109.4M cache-read, 7.4M cache-write, 139k output and 47k input tokens. The store has 15 agent rows, **0** with tokens or cost |
| Most of the transcript is discarded | The parser handles 5 record types; Claude writes 20. Dropped: `attachment` (4103 lines), `last-prompt`, `mode`, `ai-title`, `permission-mode`, `bridge-session`, `atis-latch`, `pr-link`, `file-history-snapshot`, `file-history-delta`, `queue-operation`, `agent-name`, and the `system` subtypes including `compact_boundary` |
| Session titles are wrong | Nine sessions are titled `/model`. Claude's own `ai-title` record is present in 42 transcripts and unused |
| Turn identity is positional | Turns are keyed by `ordinal`, so a fork or a truncated backfill renumbers everything and the code has to salt ids (`ordinal_salted`) to avoid collisions |
| Latency can be negative | One turn stores an end before its start; nothing forbids it |
| Two disagreeing error signals | Observations carry both `level` and `is_error` |
| Timestamps render as UTC | The views use `datetime(ts,'unixepoch')` and `fmt_time` applies no offset, so every time shown is three hours off this machine's clock |

## What the references do

### Langfuse's Claude plugin

One file, wired to exactly two hooks: `Stop` and `SessionEnd`. Everything else is read from the transcript. The decisions worth taking:

1. **Turn identity is the user row's `uuid`.** The trace id is `hash(session_id, user_row_uuid)`. Re-emitting converges with no coordination, and a fork cannot renumber anything.
2. **The trailing turn is never closed by `Stop`.** `Stop` fires several times inside one logical turn. Only the next user row, or `SessionEnd`, closes it.
3. **A generation ends at its own assistant row, not at the tool result.** Their regression fixture is a three-hour wait on a question to the user, which the naive rule reports as three hours of model latency.
4. **Subagents come from sidecar files**, `<transcript-stem>/subagents/agent-<id>.jsonl` plus `.meta.json`, joined to the parent by `toolUseId`.
5. **Cache writes are split by lifetime**, 5-minute and 1-hour, because the two are priced differently.
6. **Ambiguous attribution emits nothing** rather than guessing.

Their documented gaps, which agent-mux can beat: compaction is not handled at all, and subagent descent stops at depth 1.

### The Langfuse data model

1. **Usage and cost are stored twice**: `provided_usage_details` and `provided_cost_details` exactly as the client sent them, beside `usage_details` and `cost_details` as finally computed.
2. **Any client-supplied cost suppresses every calculated cost point.** All or nothing, never per-key blending.
3. **An error is `level = ERROR` plus a `status_message`.** There is no error boolean.
4. **A trace has no end time.** Its latency is derived from its children.
5. **Ten observation types** share one physical schema; seven are generations with a semantic label.
6. **Scores carry a data type** (numeric, categorical, boolean, text, correction), a source (api, eval, annotation), and attach to a trace, an observation, a session or a dataset run.
7. **Every entity has an immutable-field list** that updates may never touch.
8. **Nothing is precomputed.** Langfuse added aggregating tables and materialized views, then deleted them; rollups are query-time. This validates agent-mux's use of SQL views rather than rollup tables.

## Verified facts this design depends on

- The subagent join is exact. Of the subagent metadata files with a `toolUseId` and a surviving parent, **21 of 21** resolve to a tool call in the parent transcript. Zero misses.
- Subagent sidecars carry full per-message `usage`, a `model`, an `agentType`, a `description`, a `parentAgentId` and a `spawnDepth`. Six of the 22 ran at depth 2, so recursion is real.
- `compact_boundary` carries `compactMetadata` with `trigger`, `preTokens`, `postTokens` and `cumulativeDroppedTokens`, plus `logicalParentUuid` linking across the compaction.
- `attachment/skill_listing` carries the authoritative skill inventory for a session, with names and a count.
- Langfuse prices Claude on `input`, `output`, `cache_read_input_tokens`, `input_cached_tokens`, `input_cache_read`, `cache_creation_input_tokens`, `input_cache_creation`, `input_cache_creation_5m` and `input_cache_creation_1h`. The 5-minute and 1-hour rates differ, 3.75 against 6.00 per million on Sonnet 3.

## Decisions

1. **Identity is derived from content, never from position.** A turn id is `hash(provider, session_id, user_row_uuid)`. An observation id is `hash(turn_id, message_id | tool_use_id | agent_id)`. `ordinal` survives as a display-only column; `ordinal_salted` and the salting logic are deleted. A fork, a compaction or a re-import lands on the same ids.

2. **The tree is built from the transcript's own DAG.** Every line carries `uuid` and `parentUuid`, and every tool result carries `sourceToolAssistantUUID`. A tool observation is parented to the generation whose message issued it. A subagent span is parented to its launching tool call. The subagent's own generations and tools are parented to that span, recursively via `parentAgentId`. Hook events stop being a source of structure and go back to being what they are good at: timing pins and lifecycle facts.

3. **Subagents are ingested from their sidecar transcripts.** Each `agent-<id>.jsonl` is parsed with the same code as a parent transcript, into observations under an `agent` span. Depth comes from `spawnDepth`, parentage from `parentAgentId`, and the link to the parent turn from `toolUseId`. Where a sidecar is absent the span still exists, carrying what the tool result reported, and its tokens read as unknown rather than zero.

4. **Usage and cost are stored as provided and as computed.** Two JSON maps each, with the scalar columns kept as indexed projections of the computed map. Claude reports usage per assistant message and a session total in `cost-state`; both become `provided_*`. agent-mux's own pricing fills `usage_details`/`cost_details` only when nothing was provided, following Langfuse's all-or-nothing rule.

5. **The usage key vocabulary matches what Langfuse prices.** `input`, `output`, `total`, `cache_read_input_tokens`, `input_cache_creation_5m`, `input_cache_creation_1h`, `output_reasoning_tokens`. The current export sends `input_cache_write` and `input_cache_write_1h`, which no price row matches. This is latent rather than live, because agent-mux also sends `cost_details` and a provided cost suppresses server pricing entirely, but it breaks the moment a model is unpriced locally.

6. **The records Claude already writes are ingested rather than dropped.** `ai-title` becomes the session title, with the first prompt as fallback. `compact_boundary` becomes an event observation and puts `trigger`, `preTokens` and `postTokens` on the turn, which is what the Loop view's context panel should have been reading all along. `pr-link` and the file-history records become turn metadata. `attachment/skill_listing` becomes the skill inventory, replacing the disk scan that reports plugin skills as "not on disk". `remote_session_change` records the web session URL, so a claude.ai link resolves to a local session.

7. **An error is a level, not a boolean.** `is_error` is dropped. Existing rows migrate to `level = 'ERROR'`. A declined tool call becomes `level = 'WARNING'` with a status message, which is what it is.

8. **A turn's latency is derived, never stored wrong.** `end_ns` is kept but the views compute latency as `max(end, last child end) - start` clamped at zero.

9. **Scores gain a data type, an observation target, and a source.** Numeric, categorical, boolean and text, attaching to a turn, an observation, a session or a launch, recorded as annotation or evaluation.

10. **Times render in local time.** One formatter, offset applied once, used by both the CLI and the browser.

## What this is not

Not a rewrite of the store's engine: SQLite, WAL, the writer thread and the deterministic-id scheme all stay. Not a change to Codex or Antigravity capture, which keep working through the same assembler with their own parsers. Not an adoption of Langfuse's wide-event table, whose payoff is columnar scan performance that SQLite cannot use. Not a second pipeline: subagent ingestion runs in the existing tailer.

## Risks

1. **Sidecar timing.** A subagent transcript is written while the parent turn is still open. The tailer must watch the directory, not read it once. Mitigated by treating each sidecar as its own tailer with its own offset, flushed when the parent turn closes.
2. **Re-import volume.** Ingesting subagents multiplies observation counts by roughly the depth of use. On this machine that is 1,419 additional generation rows against 4,400 existing. Acceptable, and `content_max_bytes` still bounds the text.
3. **Migration of existing rows.** Ids change, because identity changes. The migration cannot recompute a content-derived id for a row whose source line is gone, so v5 keeps old rows as they are and applies the new scheme to newly ingested sessions. `trace import` re-ingests a transcript under the new scheme, and the old rows for that session are replaced.
4. **`ai-title` arrives late.** Claude writes it after the first exchange, so a session shows a prompt-derived title briefly and then changes. Preferred over a permanently wrong title.
5. **Depth-2 recursion is real but rare.** Six cases on this machine. The recursive path is exercised by fixture, not only by live data.

## Files

`src/transcript.rs` (record coverage, DAG fields), `src/tracing/map.rs` (identity, parentage, usage split), `src/tracing/subagents.rs` (new: sidecar discovery and tailing), `src/tracing/store/schema.rs` (v5), `src/tracing/store/query.rs` and the views, `src/tracing/langfuse/map.rs` (usage keys), `src/tracing/cli.rs` and `src/ui.rs` (local time, titles, skills source).
