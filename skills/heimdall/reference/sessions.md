# Sessions: briefing procedure

## Dossier Section: `sessions`

When `$AGENT_MUX_BRIEFING` is provided (schema version 2), the `sessions` section (`data.sessions`) contains up to 20 recent and active session cards for the workspace captured within the 24-hour window ending at `as_of`.

Each session card holds:
- Identity: `session_key` (`provider:session_id`), `launch_id`, `provider` (`claude`, `codex`, `antigravity`), `cwd`.
- State: `runtime_state` (`working`, `waiting_for_user`, `idle`, `exited`), `task_outcome` (`succeeded`, `failed`, `cancelled`, `unknown`).
- Activity:
  - `initial_goal`: first user prompt with confidence and limitations.
  - `current_activity`: newest open observation or in-flight tool and target.
  - `completed_turns`, `open_turns`.
  - `total_tools`, `tool_counts` (breakdown by tool name).
  - `files_modified` (paths touched).
  - `recent_commands` (shell commands executed).
  - `last_assistant_output` (closing text).
- Resources: `duration_ms`, `last_active_ns`, `total_tokens`, `total_cost_usd`.

## Procedure

1. For the startup briefing: answer directly from `$AGENT_MUX_BRIEFING`'s `sessions` section. Do not query MCP or the CLI.
2. If `status` is `partial`, check `warnings` (e.g. session cards capped or live process clues unavailable) and report the limitation.
3. Only when the user asks for a timeline or drill-down into specific turns: call `agent_mux_get_timeline` (or `trace show <session-key> --json`).
4. Only when the user asks about another workspace or an older time window: call `agent_mux_get_briefing` (`since`, `until`, `provider`).

## Report shape

```
### <provider> · <cwd> · <session key>
Goal: …
Now: <tool> on <path/command> for <n> s   (or: idle since <time>)
Done: <turns> turns · <tools> tool calls (<top three by name>) · <files> files · <commands> commands
Last output: …
Cost: <tokens> tokens · $<usd> · <duration>
Evidence: <trace ids / observation ids>
```

## SQL (Provenance & Developer Reference)

The underlying schema and queries are documented below for provenance and ad-hoc analysis (see `docs/trace-sql-examples.md`).

```sql
SELECT s.key, s.provider, s.cwd, datetime(s.last_seen_ns/1000000000,'unixepoch','localtime') AS last_seen,
       ss.turn_count, ss.open_turns, ss.tool_count, ss.error_count, ss.total_tokens, ss.total_cost_usd
FROM sessions s JOIN session_stats ss ON ss.key = s.key
WHERE s.last_seen_ns > (strftime('%s','now') - 86400) * 1000000000
ORDER BY ss.open_turns DESC, s.last_seen_ns DESC;
```
