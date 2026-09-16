---
name: loop-triage
description: Use when agent-mux runs the daily-triage loop or the user asks for a "daily triage", "repo health check", "what needs attention today" or "triage the repository". Reads the loop context, inspects tests, lint, TODOs and stale branches within a fixed budget, rewrites the state file and reports.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-triage — daily triage of one repository

You are one run of a scheduled loop. You have a budget, a level and a state file; agent-mux computed all of them before you started. You read, you triage, you write the state file, you stop.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` before running any command. If the variable is unset or the file is missing, print `no loop context` and stop.
2. From it take `files.state` (the state file to rewrite), `run.level_effective` (`L1` means report-only), `budget.mode`, `gate.denylist`, `gate.max_files`, `previous_run` and `recent_runs`.
3. Read the current state file so you can carry over items that are still open. Read `loop-constraints.md` through the `loop-rules` skill (`/loop-rules` or `$loop-rules`) when it is installed.
4. If `budget.mode` is `report-only`, treat the run as `L1` whatever the configured level says.

## Procedure

Stay inside these bounds; do not explore beyond them.

1. Detect the project kind from the root: `Cargo.toml` → `cargo test --no-run` then `cargo test 2>&1 | tail -40`; `package.json` → `npm test --silent 2>&1 | tail -40`; `pyproject.toml` → `pytest -q 2>&1 | tail -40`; otherwise skip tests and say so.
2. Lint the same way when a linter is configured (`cargo clippy --all-targets 2>&1 | tail -20`, `npm run lint --silent 2>&1 | tail -20`). One invocation each; never install tools.
3. `git status --porcelain` and `git branch --no-merged HEAD --sort=-committerdate | head -10` for uncommitted work and stale branches (older than 30 days by `git log -1 --format=%cr <branch>`).
4. `grep -rn "TODO\|FIXME\|XXX" --include=*.rs --include=*.ts --include=*.py --include=*.go . | grep -v node_modules | head -30` for open markers.
5. Compare with `previous_run` and the old state file: an item seen before is not new; an item that went away moves to Recent Noise.
6. Bucket every finding: **High Priority** (failing test, lint error, uncommitted work older than a day), **Watch List** (stale branch, growing TODO count), **Recent Noise** (things you saw and decided not to act on, one line each).

## Report-only vs assisted

- At effective `L1` you write nothing but the state file. Do not edit code, do not create branches.
- At `L2` or `L3` you may hand exactly one High Priority item to `loop-fix` (`/loop-fix` or `$loop-fix`) when it is a single-file, test-covered change outside `gate.denylist`. Pick the one with the smallest blast radius. If none qualifies, stay report-only and say why.

## Verifier

At `L2` or `L3`, after `loop-fix` returns a change, hand it to the `loop-verifier` sub-agent and record its verdict line (`## Verdict: APPROVE | REJECT | ESCALATE_HUMAN`) in the state file. Anything other than APPROVE means the change is not proposed; ESCALATE_HUMAN or a missing verdict means the item goes to High Priority with `Human decision:` requested.

## State file

Rewrite `files.state` in this exact shape, keeping items that are still open:

```
# Loop State — <project>

Last run: <RFC3339, e.g. 2026-09-16T08:00:00Z>

## High Priority (loop is acting or waiting on human)

- [ ] <id> — <one line>
  Loop action: <what this run did>
  Human decision: <what is needed, or "none">

## Watch List

- <one line per item>

## Recent Noise (ignored this run)

- <one line per item>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
```

## Rules

- Never push, merge, rebase or force-push onto a shared branch; a human merges.
- One fix per run, and only through `loop-fix`.
- Never write to a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing: an ambiguous finding goes to High Priority with a question, not a change.
- Never disable, skip or delete a test to make a run green.
- Stay inside the commands above; if the budget mode is `report-only`, spend less, not more.

## Finish

End your final message with this block, exactly once. `outcome` is one of `report-only | fix-proposed | escalated | no-op` (`no-op` when nothing changed and nothing needs a human):

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
