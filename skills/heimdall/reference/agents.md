# Agents (subagents): deep evaluation

An agent here is a subagent a harness spawned inside a turn: Claude Code `Agent`/`Task` calls, Codex sub-sessions, Antigravity `define_subagent` runs. Each is one observation of `type = 'agent'`; its own tool calls and generations are child observations (`parent_id = <agent observation id>`).

Sessions started from the agent-mux sidebar are separate processes and appear as `launches` rows whose `profile` is `<Skill name> (<harness>)`.

## Dossier Section: `agents`

When `$AGENT_MUX_BRIEFING` is provided (schema version 2), the `agents` section (`data.agents`) contains facts for every subagent definition and invocation across the 30-day evaluation window ending at `as_of`.

Each `AgentFact` carries:
- Identity & Definitions: `agent_type`, `definitions` (harness, scope, path, declared tools, model, lint findings).
- Volume & Outcomes: `invocations`, `failures`.
- Latency Percentiles: `mean_ms`, `p50_ms`, `p90_ms`, `max_ms` (nearest-rank percentiles).
- Child Observations: `child_tools`, `child_tool_errors` (direct children only).
- Cost & Share: `tokens`, `cost_usd`, `maximum_session_cost_share` (`session_key`, `agent_cost_usd`, `session_cost_usd`, `share`).
- Recurring Errors: `recurring_failures` (child tool name, normalized error message, count).
- Evidence: `examples` (newest failures or slowest runs with trace ID, session key, duration, task snippet).
- Limitations: `limitations`.

## Thresholds

| Signal | Dossier Fields | Flag when |
| :--- | :--- | :--- |
| Unreliable | `failures`, `invocations` | `failures / invocations` ≥ 0.2 with ≥ 5 invocations |
| Slow | `p90_ms`, `max_ms`, `mean_ms` | `p90_ms` > 60 000 ms, or `max_ms` > 3 × `mean_ms` |
| Expensive | `maximum_session_cost_share` | `maximum_session_cost_share.share` > 40 % |
| Chatty | `child_tools`, `invocations` | `child_tools / invocations` > 30 |
| Idle | `invocations`, `definitions` | `invocations` = 0 while definition is on disk |

## Interpretation guide

- Failures concentrated in one child tool: check `recurring_failures`; the instructions likely point to a missing tool, path, or permission.
- `p90_ms` far above mean with few invocations: one runaway run, not systemic; inspect that invocation's example.
- High cost share with low child tool count: the agent is generating long text, not working; check its model in `definitions`.
- Chatty agents: repeated child tool calls; suggest tighter scope in the task prompt.
- Idle agents: definition on disk has zero invocations in the 30-day window; consider pruning unused definitions.

## Report shape

```
| Agent | Invocations | Fail % | p50 ms | p90 ms | Tokens | $ | Cost share | Verdict |
```
followed by one paragraph per flagged agent: evidence (observation ID, trace ID, recurring failure), cause, and recommended fix.

## SQL (Provenance & Developer Reference)

For provenance, full SQL queries and ad-hoc analysis are documented in `docs/trace-sql-examples.md`.
