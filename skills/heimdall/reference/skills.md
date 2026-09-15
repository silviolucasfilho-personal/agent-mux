# Skills: deep evaluation

A skill is a `SKILL.md` a harness loads into the context when its description matches. Two costs matter: the tokens of loading it, and the behaviour it drives afterwards. The store attributes both.

## Where the numbers come from

- `trace skills --json` rows: `skill`, `harness`, `path`, `turns_loaded`, `turns_unused`, `generations`, `tools`, `tokens`, `cost_usd`, `missed`, `triggers`, `note`, `last_ns`.
  - `turns_loaded`: turns whose `traces.skills` JSON contained the skill.
  - `turns_unused`: loaded turns with no observation attributed to the skill (`observations.skill`). Pure context cost.
  - `missed`: stored prompts containing one of the description's quoted trigger phrases in a turn that did not load the skill.
  - `note`: `not on disk` (ran, but no definition found now) or `never triggered` (on disk, never loaded).
- `skill_stats` view: the same per-skill aggregates straight from SQL.
- `trace skills lint <name>`: frontmatter problems (`no-frontmatter`, `name-mismatch`, `empty-description`, `one-word-description`, `long-description`, `no-triggers`, `unknown-tool`).

## Thresholds (state them when you use them)

| Signal | Flag when |
| :--- | :--- |
| Wasted loads | `turns_unused / turns_loaded` ≥ 0.5 and `turns_loaded` ≥ 5 |
| Missed triggers | `missed` ≥ 3 |
| Expensive | `cost_usd / (generations + tools)` in the top quartile across skills |
| Slow | attributed tool p95 > 4000 ms (SQL below) |
| Failing | attributed tool error rate ≥ 10 % with ≥ 5 calls |
| Stale | `last_ns` older than 30 days while still on disk |

## SQL

Attributed activity per skill, with latency percentiles (nearest rank) and errors:

```sql
WITH d AS (
  SELECT (COALESCE(o.end_ns,o.start_ns)-o.start_ns)/1000000 AS ms, o.level='ERROR' AS err
  FROM observations o WHERE o.skill = ?1 AND o.type IN ('tool','agent')
), r AS (
  SELECT ms, err, ROW_NUMBER() OVER (ORDER BY ms) AS rn, COUNT(*) OVER () AS n FROM d
)
SELECT n AS calls, SUM(err) AS errors,
       MAX(CASE WHEN rn = MAX(1, -CAST(-0.5*n AS INT)) THEN ms END) AS p50_ms,
       MAX(CASE WHEN rn = MAX(1, -CAST(-0.95*n AS INT)) THEN ms END) AS p95_ms,
       MAX(ms) AS max_ms
FROM r;
```

Turns where the skill was loaded but nothing was attributed (what did the model do instead?):

```sql
SELECT t.id, t.session_key, datetime(t.start_ns/1000000000,'unixepoch','localtime') AS at, substr(t.input,1,160) AS prompt
FROM traces t, json_each(t.skills) j
WHERE j.value = ?1
  AND NOT EXISTS (SELECT 1 FROM observations o WHERE o.trace_id = t.id AND o.skill = ?1)
ORDER BY t.start_ns DESC LIMIT 20;
```

Load frequency over time (is the skill still relevant?):

```sql
SELECT date(t.start_ns/1000000000,'unixepoch','localtime') AS day, COUNT(*) AS loads
FROM traces t, json_each(t.skills) j WHERE j.value = ?1 GROUP BY day ORDER BY day DESC LIMIT 30;
```

Skills that co-load (context bloat from overlapping descriptions):

```sql
SELECT a.value AS skill_a, b.value AS skill_b, COUNT(*) AS together
FROM traces t, json_each(t.skills) a, json_each(t.skills) b
WHERE a.value < b.value GROUP BY skill_a, skill_b ORDER BY together DESC LIMIT 15;
```

Recurring error messages attributed to a skill:

```sql
SELECT o.tool_name, substr(o.status_message,1,120) AS error, COUNT(*) AS n
FROM observations o WHERE o.skill = ?1 AND o.level = 'ERROR'
GROUP BY o.tool_name, error ORDER BY n DESC LIMIT 10;
```

Prompts that should have triggered the skill (adapt the phrase from the description):

```sql
SELECT t.id, substr(t.input,1,160) FROM traces t
WHERE lower(t.input) LIKE '%<trigger phrase>%'
  AND NOT EXISTS (SELECT 1 FROM json_each(t.skills) j WHERE j.value = ?1)
ORDER BY t.start_ns DESC LIMIT 10;
```

## Interpretation guide

- High `turns_unused` with a broad description: narrow the description to the quoted trigger phrases; every wasted load costs the skill's full text in input tokens per turn.
- `missed` > 0 with a narrow description: add the phrases users actually type (quote three from the prompts you found).
- Slow attributed tools: check whether the skill instructs long shell pipelines; suggest splitting or caching.
- Errors concentrated on one `tool_name`: the instructions likely name a tool, flag or path that does not exist; `skills lint` will show `unknown-tool` when the frontmatter is wrong.
- `not on disk`: the skill ran under another name or was deleted; say which sessions still reference it.

## Report shape

```
| Skill | Loaded | Unused | Missed | Calls | Err % | p95 ms | Tokens | $ | Verdict |
```
followed by one paragraph per flagged skill: evidence (trace IDs, SQL), cause, fix.
