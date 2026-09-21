# The workflow document

A TOML file with four tables.

```toml
[workflow]
name = "review-changes"          # ^[a-z][a-z0-9_-]*$
description = "…"                # one sentence, what it does
when_to_use = "…"                # when to reach for it
harness = "any"                  # "any" | "claude" | "codex" | "agy" | ["claude", "codex"]
output = "report"                # the step whose result is the run's result (default: the last)
default_isolation = "none"       # "none" | "worktree" for steps that omit isolation
budget_tokens = 400000           # optional ceiling

[args.scope]                     # one table per argument
description = "…"
default = ""                     # or required = true

[schemas.findings]               # one table per result shape
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.severity = { type = "string", enum = ["high", "medium", "low"], required = true }
# types: string integer number boolean array object; items: a type or another schema

[[steps]]
id = "find"                      # unique
kind = "fanout"                  # single (default) | fanout | pipeline | route | tournament | until
phase = "Review"                 # progress group
over = ["a", "b"]                # items: an array, args.<name>, or a path into an earlier step
skill = "wf-review-find"         # or prompt = "inline text with {item} and {args.scope}"
args = { dimension = "{item}" }  # values the skill reads from its context
input = "earlier-step"           # a path handed to the skill as inputs
result = "findings"              # the schema the answer must match
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2" }
keep = "severity in [high, medium]" # predicate on each item's result: == != < <= > >= in [a, b] not in [a, b] and or not has(field); runs before the votes
dedupe_by = ["file", "line"]     # keep the first item per key
take = 40                        # keep at most N items
harness = "codex"                # per-step overrides: harness profile model isolation cwd timeout_s concurrency
```

Paths: `find` (a step's result), `find[*].findings[*]` (`[*]` flattens one level), `args.scope`, `item`, `item.file`, `index`.

Kinds:

| kind | needs | sessions | result |
| --- | --- | --- | --- |
| `single` | skill or prompt | one | the answer |
| `fanout` | skill or prompt, `over` | one per item, plus `verify` votes | array of item answers |
| `pipeline` | `over`; skill optional | per item: optional answer, votes | array of surviving items |
| `route` | skill or prompt, `result` with a `label` field, `branches = { label = ["step", …] }` | one | the answer; steps on other branches are skipped |
| `tournament` | `judge` (a skill) or `judge_prompt`, and `over` or `n` with skill or prompt | n generators, then pairwise judges | the winning candidate |
| `until` | skill or prompt, `dedupe_by`; `rounds_without_new` (2), `max_rounds` (10) | one per round (or per item of `over`) | every distinct item found |

`verify`: after each item, `votes` independent sessions of the refuter; boolean fields of their answers are counted and `keep` is evaluated on the counts (`refuted < 2` means at most one of three refuted).
