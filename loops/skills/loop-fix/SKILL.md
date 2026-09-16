---
name: loop-fix
description: Use only from another loop skill at level L2 or L3, when the user or a triage skill asks to "propose a minimal fix", "fix just this one thing" or "make the smallest change that passes". Makes exactly one bounded change in the run's worktree, within the gate, with the tests run, and hands it back for verification.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-fix — one minimal fix, inside the gate, tests run

You are called by a triage skill with one item. You change as little as possible, you prove it with the project's tests, you stop. You never decide what to fix; that was decided before you.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Refuse to run when `run.level_effective` is `L1` or `budget.mode` is `report-only`: print `loop-fix: report-only run` and return.
3. Take `worktree` (you must be inside it: `git rev-parse --show-toplevel` equals it), `gate.denylist`, `gate.max_files`, `files.ledger`, `breaker`.
4. If `breaker.status` is not `ok`, return `loop-fix: breaker tripped` without changing anything.

## Procedure

1. Restate the item in one line and name the single file (or two at most) you expect to touch. If the fix needs more files than `gate.max_files`, or any path matching `gate.denylist`, return `loop-fix: out of scope` with the reason.
2. Reproduce first: run the narrowest test that shows the problem (`cargo test <name>`, `npm test -- <pattern>`, `pytest <path>::<test>`). No reproduction → return `loop-fix: cannot reproduce`.
3. Make the change. Smallest diff that makes the reproduction pass; no refactors, no formatting sweeps, no new dependencies.
4. Run the narrow test, then the full test command once (`cargo test`, `npm test`, `pytest -q`). Lint once if the project has it.
5. Up to **three** attempts in total. A failed attempt is reverted (`git checkout -- <files>`) before the next. After the third failure return `loop-fix: gave up after 3 attempts` with the last error, so agent-mux can record it in the ledger.
6. On success: `git add -A && git commit -m "loop(<pattern>): <one line>"` on the current `loop/<run_id>` branch. Never push.
7. Return the diff summary (`git diff --stat HEAD~1`) and the test output tail to the caller.

## Report-only vs assisted

This skill only exists in assisted runs. At `L2` the change is a proposal on a branch a human merges; at `L3` the same, with the verifier's approval recorded. Nothing here merges.

## Verifier

You do not verify your own change. The calling skill hands your commit to the `loop-verifier` sub-agent. Do not argue with a REJECT: revert on request and report.

## State file

You do not write the state file; the caller does. Give it what it needs: files touched, tests run, attempts used, the commit hash.

## Rules

- Never push, merge, rebase or force-push; never leave the worktree.
- One fix per run; three attempts at most; one commit.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing: an unclear fix is returned as `out of scope`.
- Never disable, skip, delete or weaken a test; never change CI configuration.

## Finish

Return to the caller with one line (`loop-fix: done <hash>`, or one of the refusal lines above: `report-only run`, `breaker tripped`, `out of scope`, `cannot reproduce`, `gave up after 3 attempts`).

The triage skill that called you writes the run's `loop-result` block. Emit it yourself only when you were invoked directly as the top-level skill of the run, with `outcome` ∈ `report-only | fix-proposed | escalated | no-op` (`fix-proposed` after a committed change, `escalated` after a refusal that needs a human, `no-op` otherwise):

```loop-result
{"outcome":"fix-proposed","items_found":1,"actions_taken":1,"escalations":0,"summary":"one line"}
```

## What a good fix looks like

- The diff reads as the obvious correction a reviewer would have made: a wrong comparison, a missing `await`, an off-by-one, a stale import, a version pin.
- The narrow test that failed now passes and no other test changed state.
- The commit message names the item the triage skill gave you, not the symptom you saw.
- Nothing outside the named file moved: no formatter run, no lockfile churn beyond the one dependency, no generated files.
