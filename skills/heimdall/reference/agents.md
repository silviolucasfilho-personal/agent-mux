# Agents (subagents): deep evaluation

An agent here is a subagent a harness spawned inside a turn: Claude Code `Agent`/`Task` calls, Codex sub-sessions, Antigravity `define_subagent` runs. Each is one observation of `type = 'agent'`; its own tool calls and generations are child observations (`parent_id = <agent observation id>`).

Sessions started from the agent-mux sidebar are separate processes and appear as `launches` rows whose `profile` is `<Skill name> (<harness>)`.

## Where the numbers come from

- `trace agents --json` rows: `agent_type`, `invocations`, `mean_ms`, `p90_ms`, `max_ms`, `tokens`, `cost_usd`, `failures`. Source: the `agent_stats` view (`failed` is true when the agent or any child observation has `level = 'ERROR'`).
- `trace show <trace-id> --json --tree`: the agent observation with its children nested.
- `trace skills lint <name>`: for Claude agents on disk (`.claude/agents/*.md`), the description, tools and model checks.

## Thresholds

| Signal | Flag when |
| :--- | :--- |
| Unreliable | `failures / invocations` ≥ 0.2 with ≥ 5 invocations |
| Slow | p90 > 60 000 ms, or `max_ms` > 3 × `mean_ms` |
| Expensive | agent cost > 40 % of the session's `total_cost_usd` |
| Chatty | > 30 child tool calls per invocation on average |
| Idle | invocations in the window = 0 while the definition is on disk |

## SQL

Every invocation of one agent type, newest first:

```sql
SELECT a.id, t.session_key, t.id AS trace_id,
       datetime(a.start_ns/1000000000,'unixepoch','localtime') AS at,
       (COALESCE(a.end_ns,a.start_ns)-a.start_ns)/1000000 AS ms,
       a.level, substr(a.input,1,160) AS task,
       (SELECT COUNT(*) FROM observations c WHERE c.parent_id = a.id AND c.type='tool') AS child_tools,
       (SELECT SUM(c.level='ERROR') FROM observations c WHERE c.parent_id = a.id) AS child_errors,
       COALESCE(a.total_tokens,0) + COALESCE((SELECT SUM(c.total_tokens) FROM observations c WHERE c.parent_id=a.id),0) AS tokens
FROM observations a JOIN traces t ON t.id = a.trace_id
WHERE a.type = 'agent' AND COALESCE(json_extract(a.metadata,'$.agent_type'), a.name) = ?1
ORDER BY a.start_ns DESC LIMIT 20;
```

Latency percentiles per agent type:

```sql
WITH d AS (
  SELECT COALESCE(json_extract(a.metadata,'$.agent_type'), a.name) AS agent_type,
         (COALESCE(a.end_ns,a.start_ns)-a.start_ns)/1000000 AS ms
  FROM observations a WHERE a.type='agent'
), r AS (
  SELECT agent_type, ms,
         ROW_NUMBER() OVER (PARTITION BY agent_type ORDER BY ms) AS rn,
         COUNT(*) OVER (PARTITION BY agent_type) AS n
  FROM d
)
SELECT agent_type, n AS invocations,
       MAX(CASE WHEN rn = MAX(1, -CAST(-0.5*n AS INT)) THEN ms END) AS p50_ms,
       MAX(CASE WHEN rn = MAX(1, -CAST(-0.9*n AS INT)) THEN ms END) AS p90_ms
FROM r GROUP BY agent_type ORDER BY n DESC;
```

Where the failures come from (child tool and message):

```sql
SELECT c.tool_name, substr(c.status_message,1,120) AS error, COUNT(*) AS n
FROM observations a JOIN observations c ON c.parent_id = a.id
WHERE a.type='agent' AND COALESCE(json_extract(a.metadata,'$.agent_type'), a.name) = ?1 AND c.level='ERROR'
GROUP BY c.tool_name, error ORDER BY n DESC LIMIT 10;
```

Cost share of agents inside a session:

```sql
SELECT COALESCE(json_extract(a.metadata,'$.agent_type'), a.name) AS agent_type,
       SUM(COALESCE(a.total_cost_usd,0) + COALESCE((SELECT SUM(c.total_cost_usd) FROM observations c WHERE c.parent_id=a.id),0)) AS agent_cost,
       (SELECT total_cost_usd FROM session_stats WHERE key = ?1) AS session_cost
FROM observations a JOIN traces t ON t.id=a.trace_id
WHERE t.session_key = ?1 AND a.type='agent' GROUP BY agent_type ORDER BY agent_cost DESC;
```

Sessions launched from the agent-mux sidebar for a skill (e.g. Heimdall itself):

```sql
SELECT l.id, l.profile, l.provider, l.cwd, datetime(l.started_ns/1000000000,'unixepoch','localtime') AS started,
       l.termination, l.exit_code, ss.turn_count, ss.tool_count, ss.error_count, ss.total_cost_usd
FROM launches l LEFT JOIN session_stats ss ON ss.key = l.session_key
WHERE l.profile LIKE ?1 || ' (%' ORDER BY l.started_ns DESC LIMIT 20;
```

## Interpretation guide

- Failures concentrated in one child tool: the agent's instructions point at a missing tool, path or permission; quote the error and the definition line.
- p90 far above mean with few invocations: one runaway run, not a systemic problem; show that invocation's timeline.
- High cost share with low child tool count: the agent is generating, not working; check its model in the definition (`skills lint`).
- Chatty agents: repeated tool calls with the same input show up as `retries` in `trace_stats`; suggest tighter scope in the task prompt.

## Report shape

```
| Agent | Invocations | Fail % | p50 ms | p90 ms | Tokens | $ | Cost share | Verdict |
```
then one paragraph per flagged agent: evidence (observation IDs), cause, fix.
