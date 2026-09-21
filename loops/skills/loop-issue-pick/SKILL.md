---
name: loop-issue-pick
description: Use when agent-mux runs the continuous-pr loop or the user asks to "pick up a ready issue", "implement the next issue", "turn an issue into a PR" or "work the ready queue". Reads the loop context, picks the one smallest ready issue, hands it to loop-fix on a branch and to the checkers, and proposes the branch to a human.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-issue-pick — one ready issue, one branch, one proposal

You are one run of a scheduled loop over the issues a human marked ready. Budget, level, worktree and state file come from agent-mux; you pick, delegate one change, record the verdicts, write the state file and stop. You never open, edit, close or comment on an issue and you never push.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` before running any command. If it is missing, print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget.mode`, `worktree`, `gate.denylist`, `gate.max_files`, `breaker`, `previous_run`.
3. Read the current state file; the issue in progress and its verdicts carry over.
4. `budget.mode = report-only` makes this run `L1`. `breaker.status` other than `ok` means you list, you do not fix.

## Procedure

**Early exit, before anything else.** Do step 1 (the one listing call), then build the run fingerprint: one line per issue from the step 1 JSON, `number|updatedAt|<labels>`, sorted, followed by `git rev-parse HEAD`. Compare it with the `Fingerprint:` line at the bottom of the state file. When it is identical and no High Priority item carries a `Loop action:` this run must continue, do not read any issue body: rewrite only the `Last run:` and `Run log:` lines, keep every section as it is, and finish with outcome `no-op`. The whole run must stay under 5k tokens. Otherwise carry on and write the new fingerprint in the footer.

1. `gh issue list --state open --label ready --limit 30 --json number,title,url,labels,body,updatedAt,assignees` — one call. Without a `ready` label in the repository, fall back to `--label good-first-issue`; without `gh` or without authentication, record that as the single High Priority item and skip to the state file.
2. Drop every issue that is assigned to a human, labelled `security`, `migration`, `auth`, `payments` or `blocked`, or whose title or body names a path matching `gate.denylist`: those are human gates and go to **Watch List** with the gate named.
3. Rank the rest by expected blast radius: the body names one file or one function first, a failing test named in the body next, everything else last. An issue already in the state file with a `Loop action: rejected` line from a previous run is skipped unless the issue changed since; after three rejected attempts on the same issue it moves to **High Priority** with `Human decision:` requested.
4. Pick exactly one issue. Put it in **High Priority** with `Loop action: picked #<n>`. Every other candidate goes to **Watch List** in rank order; issues you dropped in step 2 go to **Recent Noise** when they were already listed last run.
5. At effective `L1` stop here: the pick is the report.
6. At `L2`/`L3` hand the picked issue to `loop-fix` (`/loop-fix` or `$loop-fix`) with the issue number, its title and the one-line acceptance test you derive from the body. `loop-fix` reproduces, changes at most `gate.max_files` files, runs the tests and commits once on the run's `loop/<run_id>` branch inside `worktree`. Its return value (`gave up`, `out of scope`, `cannot reproduce`, or a diff summary) becomes the `Loop action:` line.

## Report-only vs assisted

- At effective `L1` you write only the state file. No branch, no edit, no comment on the issue.
- At `L2`/`L3` the only change is the one commit `loop-fix` makes in the worktree. One issue per run; a `loop-fix` failure ends the run as `escalated` with the reason, it never picks a second issue.

## Verifier

At `L2`/`L3`, after `loop-fix` returns a commit, hand it to every checker sub-agent the workspace installs for this pattern (`loop-verifier`, and `loop-reviewer` when it is present) and record each `## Verdict:` line in the state file under the issue. agent-mux requires every checker to say APPROVE before the outcome is `fix-proposed`; one REJECT means the commit stays on its branch as `escalated` with the rejecting checker's reason as `Human decision:`; ESCALATE_HUMAN or a missing verdict is `escalated` as well.

## State file

Rewrite `files.state`:

```
# Continuous PR — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] #<number> <title> — <picked | rejected ×<n> | gh missing>
  Loop action: <picked #n | commit <sha> on loop/<run_id> | loop-fix: <reason>>
  Human decision: <review and open the PR from loop/<run_id>, or "none">
  Verdicts: loop-verifier APPROVE · loop-reviewer REJECT — <reason>

## Watch List

- #<number> <title> — <rank reason | human gate: <label or path>>

## Recent Noise (ignored this run)

- #<number> <title> — <why>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
Fingerprint: <the run fingerprint, one line>
```

## Rules

- Never push, never open a pull request, never merge or rebase; a human opens the PR from the branch agent-mux keeps in the Inbox.
- Never comment on, label, assign or close an issue.
- One issue per run, one commit per run, only through `loop-fix`, only inside `worktree`.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- An issue whose acceptance test you cannot state in one line is not ready: Watch List, with the question a human must answer.
- Never disable, skip or delete a test to make the change pass.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`; `fix-proposed` only when every checker approved:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
