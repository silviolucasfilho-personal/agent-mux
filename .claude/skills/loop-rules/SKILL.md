---
name: loop-rules
description: Use at the start of every loop run, or when the user asks "what are the loop rules", "load the constraints" or "what may the loop touch". Reads loop-constraints.md and the loop context and states the binding rules for this run in one line per rule.
allowed-tools: Read, Grep, Glob
---

# loop-rules — the binding rules of this run

You load the rules once, you state them, you stop. They bind every other skill in the run; agent-mux enforces the mechanical ones (paths, pushes, budgets) whether or not you read them.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.constraints` (usually `loop-constraints.md`), `gate`, `budget`, `run.level_effective`, `run.level_reason`, `kill_switch`.
3. If `kill_switch` is true, print `loops are paused` and stop the whole run.

## Procedure

1. Read `files.constraints`. Count the rules: every list item under the headings `Push & Merge`, `Paths`, `Code`, `Communication`, `Budget`.
2. Merge in the context: `gate.denylist` patterns are Paths rules; `gate.max_files` is a Code rule; `budget.mode` and `run.level_effective` are Budget rules.
3. Print exactly `Rules loaded from loop-constraints.md: N active.` followed by one line per rule, grouped by heading. When the file is missing, print `Rules loaded from loop-constraints.md: 0 active.` and then the defaults below, which apply anyway.

## Report-only vs assisted

Say which one this run is and why (`run.level_effective`, `run.level_reason`). A report-only run writes only the state file and the run log; an assisted run may propose one change on the run's branch.

## Verifier

Remind the run that at `L2`/`L3` every change goes to the `loop-verifier` sub-agent and that its verdict is recorded in the state file.

## State file

Remind the run of the shape it must keep: `Last run: <RFC3339>`, `## High Priority (loop is acting or waiting on human)`, `## Watch List`, `## Recent Noise (ignored this run)`, and the footer `Run log: <timestamp> | findings | actions | escalations`.

## Rules

The defaults, always active:

- Push & Merge: never push, merge, rebase or force-push onto a shared branch; a human merges the `loop/<run_id>` branch.
- Paths: never write a path matching `gate.denylist`; never touch `.github/workflows`, secrets, auth, payments or migrations.
- Code: one fix per run; at most three attempts; never exceed `gate.max_files`; never disable, skip or delete a test.
- Communication: escalate instead of guessing; every open question goes to High Priority with `Human decision:`.
- Budget: at 80 % of the daily cap the run is report-only; at 100 % it does not start; `loop-pause-all` in the state file stops every loop.

## Finish

Return after printing the rules. The triage skill that called you emits the run's `loop-result` block; emit it yourself only when you were invoked directly as the top-level skill of the run (`outcome` ∈ `report-only | fix-proposed | escalated | no-op`, and for a rules-only run it is `no-op`):

```loop-result
{"outcome":"no-op","items_found":0,"actions_taken":0,"escalations":0,"summary":"rules loaded, nothing else ran"}
```

## Example output

```
Rules loaded from loop-constraints.md: 9 active.
Push & Merge
- never push, merge, rebase or force-push; a human merges loop/<run_id>
Paths
- denylist: **/.env, **/secrets/**, **/auth/**, **/payments/**, **/migrations/** (+7 more)
Code
- one fix per run, three attempts, at most 10 files, tests never disabled
Communication
- escalate instead of guessing; open questions go to High Priority
Budget
- this run is report-only (tokens today at 84% of cap); loop-pause-all stops every loop
```
