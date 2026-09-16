---
name: loop-pr-triage
description: Use when agent-mux runs the pr-babysitter loop or the user asks to "babysit PRs", "check open pull requests", "which PRs are stuck" or "unblock the PR queue". Reads the loop context, buckets open pull requests by conflicts, checks and review state, and at most proposes one small unblocking fix.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-pr-triage — open pull requests, sorted by what blocks them

You are one run of a scheduled loop over the open pull requests of this repository. Budget, level and state file come from agent-mux; you read, bucket, write the state file and stop.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` before running any command. If it is missing, print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget.mode`, `gate.denylist`, `gate.max_files`, `previous_run`.
3. Read the current state file; items still open carry over.
4. `budget.mode = report-only` makes this run `L1`.

## Procedure

**Early exit, before anything else.** Do step 1 (the one listing call), then build the run fingerprint: one line per PR from the step 1 JSON, `number|updatedAt|mergeStateStatus|reviewDecision|<number of checks>`, sorted. Compare it with the `Fingerprint:` line at the bottom of the state file. When it is identical and no High Priority item carries a `Loop action:` this run must continue, do not fetch any log: rewrite only the `Last run:` and `Run log:` lines, keep every section as it is, and finish with outcome `no-op`. The whole run must stay under 5k tokens. Otherwise carry on and write the new fingerprint in the footer.

1. `gh pr list --state open --limit 50 --json number,title,url,isDraft,mergeStateStatus,mergeable,reviewDecision,statusCheckRollup,author,updatedAt` — one call. If `gh` is missing or not authenticated, record that as the single High Priority item and skip to the state file.
2. For each PR, in order:
   - `mergeable = CONFLICTING` or `mergeStateStatus = DIRTY` → **High Priority** (conflicts).
   - any check in `statusCheckRollup` with conclusion `FAILURE` or `ERROR` → **High Priority** (red CI).
   - `statusCheckRollup` empty on a non-draft → **High Priority** (no CI).
   - `reviewDecision = CHANGES_REQUESTED` or `mergeStateStatus = BLOCKED` → **High Priority** (waiting on author or reviewer).
   - `mergeStateStatus` in `CLEAN`, `HAS_HOOKS`, `UNSTABLE`, `BEHIND` → **Watch List**.
   - drafts → **Recent Noise**, unless `updatedAt` is older than 30 days (then Watch List, "idle draft").
3. For a red-CI PR you may fetch one log: `gh run list --branch <head> --limit 1 --json databaseId,conclusion` then `gh run view <id> --log-failed | tail -60`. One PR per run.
4. Compare with `previous_run`: a PR already listed keeps its entry; note when it moved buckets.

## Report-only vs assisted

- At effective `L1` you write only the state file. No comments on PRs, no branches, no pushes.
- At `L2`/`L3` you may hand one red-CI PR whose failure is a single-file, test-covered fix outside `gate.denylist` to `loop-fix`. Conflicts, security, payments and auth paths are human gates: never touch them.

## Verifier

At `L2`/`L3`, a change from `loop-fix` goes to the `loop-verifier` sub-agent. Record its verdict line in the state file. Only APPROVE makes the outcome `fix-proposed`; REJECT keeps the item in High Priority; ESCALATE_HUMAN or no verdict sets `escalated`.

## State file

Rewrite `files.state`:

```
# PR Babysitter — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] #<number> <title> — <why: conflicts | red CI | no CI | changes requested>
  Loop action: <what this run did>
  Human decision: <needed, or "none">

## Watch List

- #<number> <title> — <state>

## Recent Noise (ignored this run)

- #<number> <title> — draft

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
Fingerprint: <the run fingerprint, one line>
```

## Rules

- Never push, merge, rebase or force-push a PR branch; never call `gh pr merge`.
- One fix per run, only through `loop-fix`, only on a red-CI PR.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing: an unclear failure is a High Priority question, not an edit.
- Never disable or skip a test to turn a check green.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
