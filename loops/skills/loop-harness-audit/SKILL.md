---
name: loop-harness-audit
description: Use when agent-mux runs the harness-audit loop or the user asks to "audit the loops", "tune the harness", "why are the loops expensive", "which loop should be promoted" or "what should I change in the loop config". Reads the loop context and the trace store facts, and writes down at most three concrete tuning changes for a human to apply.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-harness-audit — what to tune, from the facts agent-mux keeps

You are one run of a scheduled loop over the loops and sessions of this workspace. You read facts agent-mux already computed, you compare them with the thresholds below, you write at most three proposals into the state file, you stop. This pattern is always report-only: you never change a pattern, a level, a denylist or a prompt yourself.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` before running any command. If it is missing, print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget`, `breaker`, `readiness`, `previous_run`, `recent_runs`.
3. Read the current state file; a proposal a human already answered (`Human decision:` other than `pending`) is not repeated.
4. Whatever the level says, this run is report-only.

## Procedure

**Early exit, before anything else.** Do step 1 (the one listing call), then build the run fingerprint: one line per loop of this workspace from the step 1 output, `pattern|level|last_outcome|runs_today|tokens_today`, sorted. Compare it with the `Fingerprint:` line at the bottom of the state file. When it is identical and no High Priority item carries a `Loop action:` this run must continue, do not read any run detail: rewrite only the `Last run:` and `Run log:` lines, keep every section as it is, and finish with outcome `no-op`. The whole run must stay under 5k tokens. Otherwise carry on and write the new fingerprint in the footer.

1. `agent-mux loop ls --json` for every loop of this workspace, then `agent-mux loop runs <loop> --json` for each, keeping the last 20 — one call per loop; `agent-mux loop show <run_id> --json` for at most three runs whose reason you need. When the read-only MCP tools are registered (`$AGENT_MUX_MCP` says `registered` or `installed`), `agent_mux_get_loop_context` and the trace tools answer the same questions without shelling out; prefer them. Without either, record that as the single High Priority item and skip to the state file.
2. For each loop, compute over its last 20 runs: the share of `no-op` outcomes, the share of `failed` and `blocked`, the mean tokens of a non-no-op run, the number of `verifier_missing` runs, the number of REJECT and ESCALATE_HUMAN verdicts, the breaker state, and whether the effective level was capped (`level_reason`) and why.
3. Apply the thresholds, each producing at most one proposal:
   - more than 80% `no-op` over 20 runs → propose a longer `default_interval_s` (double it) for that pattern.
   - mean tokens of a non-no-op run above `cost.tokens_report` → propose lowering `max_tokens_per_day` or narrowing the skill's listing call.
   - `blocked` runs with `tokens today at the cap` → propose a higher `max_tokens_per_day` or fewer `max_runs_per_day`.
   - a loop at L1 for 7 days or more with readiness score ≥ 58, no failed runs and no gate violation → propose promotion to L2.
   - any gate violation, breaker trip, or two REJECT verdicts in the last 10 runs of an L2+ loop → propose demotion to L1.
   - a touched path that a human escalated twice → propose adding its glob to `gate.yaml` `denylist`.
   - `verifier_missing` on any fix run → propose `agents = ["loop-verifier", "loop-reviewer"]` for that pattern.
4. Keep the three proposals with the largest expected saving (tokens per day) or the largest risk reduction; the rest go to **Watch List** as one line each.
5. Compare with the previous state file: a proposal already listed keeps its entry and its `Human decision:`.

## Report-only vs assisted

- This pattern has no assisted mode. At every level you write only the state file; every proposal names the file and field a human edits (`~/.agent-mux/loops/registry.toml`, `gate.yaml`, the loop's level in the Loops section).
- Never run `agent-mux loop edit`, `agent-mux config edit` or any command that writes.

## Verifier

Not used: nothing is changed. Record under each proposal the facts it rests on (run ids, counts, dates) so a human can check them in the Loops view.

## State file

Rewrite `files.state`:

```
# Harness Audit — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] <pattern>: <the change, e.g. default_interval_s 900 → 1800> — <the fact: 17/20 no-op runs since 2026-09-10>
  Loop action: proposed
  Human decision: pending | applied | declined

## Watch List

- <pattern>: <smaller proposal, one line>

## Recent Noise (ignored this run)

- <pattern>: <threshold checked, nothing to change>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
Fingerprint: <the run fingerprint, one line>
```

## Rules

- Report-only, always: no edit outside the state file, no branch, no push.
- At most three High Priority proposals; each names a pattern, a field or file, the old and the new value, and the fact behind it.
- Level promotions and config changes are human gates: propose, never apply.
- Never invent a number: every count comes from the listing or the MCP tools, with the run ids it was computed from.
- Stay inside the commands above; a question you cannot answer from them is a Watch List line, not an exploration.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`; this pattern reports `report-only` (proposals written), `escalated` (a proposal needs a decision now: a breaker trip or a gate violation) or `no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
