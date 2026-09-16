---
name: loop-issue-triage
description: Use when agent-mux runs the issue-triage loop or the user asks to "triage issues", "which issues are unanswered", "find duplicate issues" or "label the new issues". Reads the loop context, sorts open issues by attention needed and drafts labels and replies for a human to apply.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-issue-triage — open issues, sorted by attention needed

You are one run of a scheduled loop over the open issues. Budget, level and state file come from agent-mux. You never post to the tracker.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget.mode`, `previous_run`.
3. Read the current state file; drafts already written carry over.

## Procedure

1. `gh issue list --state open --limit 50 --json number,title,url,labels,createdAt,updatedAt,author,comments` — one call. No `gh` → single High Priority item, skip to the state file.
2. For each issue:
   - security-sounding title or label, or `p0`/`p1` label → **High Priority**, human gate, no draft reply.
   - non-bot author, zero comments, older than 7 days → **High Priority** (unanswered), draft a one-paragraph reply.
   - no labels → draft up to two labels from the repository's existing set (`gh label list --limit 50 --json name`), Watch List.
   - title similar to another open issue (same nouns, same error text) → **Watch List** (possible duplicate); never close.
   - non-bot author, idle more than 14 days → Watch List (stale); closures are a human gate.
   - everything else → Recent Noise, one line, at most 10 lines.
3. Read at most five issue bodies in full (`gh issue view <n> --json body`) to draft replies; the rest by title.
4. Compare with the previous state file: a draft already present is kept, not rewritten.

## Report-only vs assisted

- Effective `L1`: state file only.
- `L2`/`L3`: still no writes to the tracker; the only assisted action is applying labels a human already approved in the previous state file (`Human decision: apply`), one `gh issue edit <n> --add-label <l>` per run.

## Verifier

Not used by this pattern's reply drafting. When a label is applied at `L2`/`L3`, ask the `loop-verifier` sub-agent to confirm the label existed and the human decision was recorded; record its verdict line.

## State file

Rewrite `files.state`:

```
# Issue Triage — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] #<number> <title> — unanswered <n>d | security | p0
  Loop action: reply drafted below
  Human decision: <post | rewrite | ignore>
  Draft: <one paragraph>

## Watch List

- #<number> <title> — labels: <a>, <b> | possible duplicate of #<m> | stale <n>d

## Recent Noise (ignored this run)

- #<number> <title>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
```

## Rules

- Never post, close, reopen or edit an issue except the one approved label per run.
- One action per run.
- Never write any file but the state file.
- Escalate instead of guessing whether two issues are the same.
- Never touch code or tests.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
