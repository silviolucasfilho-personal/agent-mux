# SQL examples for the agent-mux trace store

Developer material for `agent-mux trace sql "<query>" --json` (read-only connection). This file used to ship inside the Heimdall skill; it moved here on 2026-09-15 because the questions below are answered without SQL by the MCP tools and CLI commands, which track the schema through tests, while hand-written SQL does not. Prefer those; use these queries for ad-hoc exploration.

| Question | Tool / command that answers it |
| :--- | :--- |
| Most expensive turns | `agent_mux_list_sessions`, `trace ls --all --json`, then `trace show <session> --json` |
| Tool-storm turns | `trace loops [session] --json` (`tool_calls`, warnings) |
| Error hot spots by tool | `agent_mux_get_timeline`, `trace show <trace-id> --json` (`level = ERROR`) |
| Model mix and cost per model | `trace doctor` (unpriced models), `trace show --json` per turn (`models`) |
| Cache effectiveness | `trace loops --json` (`cache_ratio`) |
| Full-text search | `agent_mux_search_traces`, `trace search "<fts5>" --json` |

## Tables

| Table | Grain | Key columns |
| :--- | :--- | :--- |
| `sessions` | one harness conversation | `key`, `provider`, `session_id`, `cwd`, `title`, `first_seen_ns`, `last_seen_ns` |
| `launches` | one process started by agent-mux | `id`, `run_id`, `profile`, `provider`, `cwd`, `session_key`, `started_ns`, `ended_ns`, `termination`, `exit_code`, `reported_cost_usd`, `metadata` |
| `traces` | one turn | `id`, `session_key`, `launch_id`, `ordinal`, `name`, `status`, `start_ns`, `end_ns`, `input`, `output`, `thinking`, `skills` (JSON array), `metadata` |
| `observations` | one model call, tool call, subagent, event or span | `id`, `trace_id`, `parent_id`, `type`, `name`, `tool_name`, `skill`, `mcp_server`, `path`, `start_ns`, `end_ns`, `level`, `status_message`, `model`, `input`, `output`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `total_tokens`, `total_cost_usd`, `metadata` |
| `hook_events` | raw CLI hook payloads | `launch_id`, `event`, `received_ns` |
| `scores` | human or API verdicts | `target`, `target_id`, `name`, `value` |

`type` is one of `generation`, `tool`, `agent`, `event`, `span`. `level` is `DEBUG`, `DEFAULT`, `WARNING` or `ERROR`. Nanosecond timestamps: `datetime(x/1000000000,'unixepoch','localtime')`.

## Views

| View | Grain | Adds |
| :--- | :--- | :--- |
| `trace_stats` | turn | `latency_ms`, `observation_count`, `generation_count`, `tool_count`, `error_count`, `open_count`, token and cost sums, `unpriced_generations`, `models`, `retries`, `declined` |
| `session_stats` | session | `turn_count`, `open_turns`, `duration_ms`, `tool_count`, `error_count`, token and cost sums, `reported_cost_usd` |
| `loop_stats` | turn | `tool_calls`, `distinct_tools`, `tool_errors`, `declined`, `subagents`, `compacted`, `input_tokens`, `cache_read_tokens` |
| `skill_stats` | skill | `turns_loaded`, `generations`, `tools`, `tokens`, `cost`, `turns_unused`, `first_ns`, `last_ns` |
| `agent_stats` | subagent type | `invocations`, `mean_ms`, `max_ms`, `tokens`, `cost`, `failures` |

## Ready queries

Most expensive turns this week:

```sql
SELECT id, session_key, ordinal, latency_ms, tool_count, error_count, total_tokens, total_cost_usd, models
FROM trace_stats WHERE start_ns > (strftime('%s','now') - 7*86400) * 1000000000
ORDER BY total_cost_usd DESC LIMIT 10;
```

Tool-storm turns (many calls, few distinct tools):

```sql
SELECT trace_id, session_key, tool_calls, distinct_tools, tool_errors
FROM loop_stats WHERE tool_calls >= 25 ORDER BY tool_calls DESC LIMIT 10;
```

Error hot spots by tool across the store:

```sql
SELECT tool_name, COUNT(*) AS errors, substr(status_message,1,100) AS sample
FROM observations WHERE level='ERROR' AND type='tool' GROUP BY tool_name ORDER BY errors DESC LIMIT 10;
```

Model mix and cost per model:

```sql
SELECT model, COUNT(*) AS generations, SUM(total_tokens) AS tokens, ROUND(SUM(total_cost_usd),4) AS usd
FROM observations WHERE type='generation' GROUP BY model ORDER BY usd DESC;
```

Cache effectiveness per session (higher cache_read share is cheaper):

```sql
SELECT key, provider, cache_read_tokens, input_tokens,
       ROUND(1.0*cache_read_tokens/NULLIF(cache_read_tokens+input_tokens,0),2) AS cache_ratio
FROM session_stats WHERE input_tokens IS NOT NULL ORDER BY last_seen_ns DESC LIMIT 20;
```

Search inside prompts and outputs (FTS5 tables mirror `traces` and `observations`):

```sql
SELECT t.id, t.session_key, snippet(traces_fts, 0, '[', ']', '…', 12) AS hit
FROM traces_fts JOIN traces t ON t.rid = traces_fts.rowid
WHERE traces_fts MATCH ?1 ORDER BY t.start_ns DESC LIMIT 20;
```

## Guardrails

- Only `SELECT` / `WITH`. The CLI rejects writes, and you must not try.
- Use `LIMIT`; the store can hold months of turns.
- When a query returns nothing, check the window and `trace doctor` before concluding the data is missing.
