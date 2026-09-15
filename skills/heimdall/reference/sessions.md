# Sessions: briefing procedure

## What the store holds

- `sessions` — one row per harness conversation: `key` (`provider:session_id`), `provider` (`claude`, `codex`, `antigravity`), `cwd`, `title`, `first_seen_ns`, `last_seen_ns`.
- `launches` — one row per process agent-mux started: `id`, `profile` (the sidebar name, e.g. `Heimdall (claude)`), `provider`, `cwd`, `started_ns`, `ended_ns`, `termination`, `exit_code`, `session_key`.
- `traces` — one row per turn: `id`, `session_key`, `launch_id`, `ordinal`, `status` (`open`/`closed`/`aborted`), `start_ns`, `end_ns`, `input` (user prompt), `output` (assistant reply), `skills` (JSON array loaded in that turn).
- `observations` — every model call, tool call, subagent, event: `type` (`generation`/`tool`/`agent`/`event`/`span`), `name`, `tool_name`, `input`, `output`, `start_ns`, `end_ns` (NULL while running), `level` (`ERROR` on failure), `status_message`, `model`, `total_tokens`, `total_cost_usd`, `skill`, `path`.
- Views: `session_stats` (per session totals), `trace_stats` (per turn totals incl. `latency_ms`, `retries`, `declined`, `models`).

Timestamps are Unix nanoseconds: `datetime(x / 1000000000, 'unixepoch', 'localtime')`.

## Procedure

1. Window: default the last 24 h; widen to `--since 7d` when empty.
2. Active first: sessions with an open turn or an open observation.

```sql
SELECT s.key, s.provider, s.cwd, datetime(s.last_seen_ns/1000000000,'unixepoch','localtime') AS last_seen,
       ss.turn_count, ss.open_turns, ss.tool_count, ss.error_count, ss.total_tokens, ss.total_cost_usd
FROM sessions s JOIN session_stats ss ON ss.key = s.key
WHERE s.last_seen_ns > (strftime('%s','now') - 86400) * 1000000000
ORDER BY ss.open_turns DESC, s.last_seen_ns DESC;
```

3. Goal: the first user prompt of the session.

```sql
SELECT substr(input, 1, 300) FROM traces WHERE session_key = ?1 ORDER BY ordinal ASC LIMIT 1;
```

4. Now (in-flight clue): the newest open observation, with its tool and target.

```sql
SELECT o.id, o.type, o.name, o.tool_name, o.path, substr(o.input,1,160) AS input,
       (strftime('%s','now')*1000000000 - o.start_ns)/1000000 AS running_ms
FROM observations o JOIN traces t ON t.id = o.trace_id
WHERE t.session_key = ?1 AND o.end_ns IS NULL
ORDER BY o.start_ns DESC LIMIT 3;
```

5. Done: tool calls by name, files touched, commands run.

```sql
SELECT o.tool_name, COUNT(*) AS calls, SUM(o.level='ERROR') AS errors
FROM observations o JOIN traces t ON t.id = o.trace_id
WHERE t.session_key = ?1 AND o.type = 'tool' GROUP BY o.tool_name ORDER BY calls DESC;

SELECT DISTINCT o.path FROM observations o JOIN traces t ON t.id = o.trace_id
WHERE t.session_key = ?1 AND o.path IS NOT NULL ORDER BY o.path;

SELECT substr(o.input,1,200) AS command, datetime(o.start_ns/1000000000,'unixepoch','localtime') AS at
FROM observations o JOIN traces t ON t.id = o.trace_id
WHERE t.session_key = ?1 AND o.type='tool' AND o.tool_name IN ('Bash','shell','run_command','execute_command')
ORDER BY o.start_ns DESC LIMIT 10;
```

6. Last output: `SELECT substr(output,1,400) FROM traces WHERE session_key = ?1 ORDER BY ordinal DESC LIMIT 1;`

7. Cost and time come from `session_stats` (`total_tokens`, `total_cost_usd`, `duration_ms`, `unpriced_generations`). Say when `unpriced_generations` > 0.

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
