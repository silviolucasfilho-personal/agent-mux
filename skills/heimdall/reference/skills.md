# Skills: deep evaluation

A skill is a `SKILL.md` a harness loads into the context when its description matches. Two costs matter: the tokens of loading it, and the behaviour it drives afterwards. The store attributes both.

## Dossier Section: `skills`

When `$AGENT_MUX_BRIEFING` is provided (schema version 2), the `skills` section (`data.skills`) contains facts for every discovered and observed skill across the 30-day evaluation window ending at `as_of`.

Each `SkillFact` carries:
- Identity & Definitions: `key`, `harness`, `scope`, `path`, `triggers`, `definition_bytes`.
- Usage & Loading: `turns_loaded`, `turns_unused`, `missed_triggers`.
- Activity & Quality: `generations`, `tools`, `attributed_calls`, `errors`, `schema_errors`.
- Latency & Cost: `p50_ms`, `p95_ms`, `max_ms`, `tokens`, `cost_usd`.
- Timeline: `first_seen`, `last_seen`.
- Diagnostics: `lint` (frontmatter and structure findings), `examples` (unused loads, missed triggers, recurring errors), `limitations`.

## Thresholds (state them when you use them)

| Signal | Dossier Fields | Flag when |
| :--- | :--- | :--- |
| Wasted loads | `turns_unused`, `turns_loaded` | `turns_unused / turns_loaded` ≥ 0.5 and `turns_loaded` ≥ 5 |
| Missed triggers | `missed_triggers` | `missed_triggers` ≥ 3 |
| Expensive | `cost_usd`, `attributed_calls` | `cost_usd / attributed_calls` in top quartile across skills |
| Slow | `p95_ms` | `p95_ms` > 4000 ms |
| Failing | `errors`, `attributed_calls` | `errors / attributed_calls` ≥ 10 % with ≥ 5 calls |
| Stale | `last_seen` | `last_seen` older than 30 days while still on disk |

## Interpretation guide

- High `turns_unused` with a broad description: narrow the description to the quoted trigger phrases; every wasted load costs the skill's full text in input tokens per turn.
- `missed_triggers` > 0 with a narrow description: add the phrases users actually type (quote examples from `examples`).
- Slow attributed tools (`p95_ms` > 4000): check whether the skill instructs long shell pipelines; suggest splitting or caching.
- Errors concentrated on one tool: the instructions likely name a tool, flag or path that does not exist; check `lint` for `unknown-tool`.
- Never triggered (`turns_loaded == 0`): definition exists on disk but was never invoked.

## Report shape

```
| Skill | Loaded | Unused | Missed | Calls | Err % | p95 ms | Tokens | $ | Verdict |
```
followed by one paragraph per flagged skill: evidence (examples, trace IDs), cause, and recommended fix.

## SQL (Provenance & Developer Reference)

For provenance, full SQL queries and ad-hoc analysis are documented in `docs/trace-sql-examples.md`.
