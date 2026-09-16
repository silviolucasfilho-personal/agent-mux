# Loops — agent-mux

agent-mux schedules the loops below, runs each one as a traced session, keeps the run log and enforces the gates. This file is for humans; the schedule itself lives in agent-mux's registry.

## Active Loops

| Pattern | Cadence | Level | State file | Harness |
| --- | --- | --- | --- | --- |
| pr-babysitter | 15m | L1 | pr-babysitter-state.md | claude |

Week one is report-only (L1). A loop is promoted to L2 after its verifier has been right for a week, and demoted or paused on the first incident.

## Human Gates

The loop never decides these; it writes them to the state file's High Priority section and waits:

- security
- payments
- auth
- max-fix-attempts

## Budget

Max tokens per day and max runs per day are in `loop-budget.md` and enforced by agent-mux before a run starts: at 80 % of the daily cap a run is report-only, at 100 % it does not start. Kill switch: put the literal `loop-pause-all` in the state file or in this file, or press `K` in agent-mux; loops resume only after a human clears it.

## Worktrees

Assisted runs (L2 and above) work in `.loop-worktrees/<run_id>` on branch `loop/<run_id>`; the workspace itself is never edited by an assisted run. Report-only runs write the state file and the run log and nothing else.

## Safety

`gate.yaml` holds the path denylist and the file cap; agent-mux refuses those writes while the run executes and re-checks the diff afterwards. Auto-merge is never performed: a proposed fix stays on its branch until a human merges it. Every run appends one JSON line to `loop-run-log.md` with the tokens the trace store measured.
