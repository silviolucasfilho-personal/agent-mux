---
name: loop-ci-triage
description: Use when agent-mux runs the ci-sweeper loop or the user asks to "sweep CI", "why is CI red", "fix the failing check" or "is that test flaky". Reads the loop context, classifies the latest failing run on the default branch as flaky, infrastructure or code, and proposes at most one fix.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-ci-triage — keep the default branch green

You are one run of a scheduled loop that looks at CI on the default branch. Budget, level and state file come from agent-mux.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.state`, `files.ledger`, `run.level_effective`, `budget.mode`, `breaker`, `gate.denylist`, `gate.max_files`, `previous_run`.
3. If `breaker.status` is not `ok`, this run is report-only regardless of level: agent-mux paused fixing because the last attempts kept failing the same way.
4. Read the current state file.

## Procedure

**Early exit, before anything else.** Do step 1 (the one listing call), then build the run fingerprint: one line per workflow from the step 1 JSON, `name|databaseId|conclusion` of its newest run, sorted. Compare it with the `Fingerprint:` line at the bottom of the state file. When it is identical and no High Priority item carries a `Loop action:` this run must continue, do not fetch any log and do not reproduce anything: rewrite only the `Last run:` and `Run log:` lines, keep every section as it is, and finish with outcome `no-op`. The whole run must stay under 5k tokens. Otherwise carry on and write the new fingerprint in the footer.

1. `git rev-parse --abbrev-ref origin/HEAD 2>/dev/null || echo main` gives the default branch. `gh run list --branch <default> --limit 10 --json databaseId,name,conclusion,headSha,createdAt,event` — one call. No `gh` → single High Priority item, skip to the state file.
2. If the newest run per workflow is `success`, the outcome is `no-op` unless the state file still holds open items.
3. For the newest failing run: `gh run view <id> --log-failed 2>&1 | tail -80`. Classify:
   - the same job passed on a re-run of the same SHA, or the failure is a timeout / network / rate limit → **flaky** (Watch List, note the count).
   - runner setup, missing secret, quota, action version → **infrastructure** (High Priority, human gate).
   - an assertion, compile error or lint failure naming a file in the repository → **code** (High Priority; fix candidate).
4. Reproduce a code failure once locally with the project's test command (`cargo test <name>`, `npm test -- <pattern>`, `pytest <path>`). Do not loop on it.
5. Compare with `previous_run` and the ledger: a failure seen with the same signature before is not new; say how many times.

## Report-only vs assisted

- Effective `L1`: state file only.
- `L2`/`L3`: hand one **code** failure to `loop-fix` when the fix is one file, test-covered, outside `gate.denylist` and not in a security test. Infrastructure and flaky items are never fixed by the loop.

## Verifier

Every change from `loop-fix` goes to the `loop-verifier` sub-agent; it must run the tests. Record the verdict line. APPROVE → `fix-proposed`; REJECT → item stays High Priority with the reason; ESCALATE_HUMAN or no verdict → `escalated`.

## State file

Rewrite `files.state`:

```
# CI Sweeper — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] <workflow> #<run id> — <code | infrastructure> — <one line>
  Loop action: <reproduced | fix proposed on loop/<run_id> | none>
  Human decision: <needed, or "none">

## Watch List

- <workflow> — flaky, <n> times in <window>

## Recent Noise (ignored this run)

- <one line per item>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
Fingerprint: <the run fingerprint, one line>
```

## Rules

- Never push, merge or rebase; the fix lives on the run's `loop/<run_id>` branch for a human.
- One fix per run, only through `loop-fix`, at most three attempts inside it.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing when the failure does not name a repository file.
- Never disable, skip, retry-loop or delete a test to make CI green.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
