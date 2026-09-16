---
name: loop-post-merge
description: Use when agent-mux runs the post-merge-cleanup loop or the user asks for a "post-merge cleanup", "what did the last merges leave behind", "dead feature flags" or "stale branches after merge". Reads the loop context, scans the merges since the previous run for leftovers and proposes at most one cleanup.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-post-merge — what the last merges left behind

You are one run of a scheduled loop that reads recent merges and lists their leftovers. Budget, level and state file come from agent-mux.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget.mode`, `gate.denylist`, `gate.max_files`, `previous_run.id` (the lower bound of the window).
3. Read the current state file.

## Procedure

1. Window: `git log --merges --since="<previous_run.id or 7 days ago>" --format="%h %s" | head -30`. No merges → `no-op` unless open items remain.
2. For the files those merges touched (`git diff --name-only <first>^..HEAD | head -100`):
   - feature flags: `grep -rn "feature_flag\|FEATURE_\|is_enabled(\"" <files>`; a flag whose both branches exist and one is unreachable after the merge is a **dead flag** (Watch List, or High Priority when it guards user-facing behaviour).
   - markers: `grep -n "TODO\|FIXME\|XXX\|HACK" <files>` added by the merge (compare with `git blame -L`) → **stale TODO**.
   - docs drift: a merged change to a public function, flag or command whose name appears in `README.md` or `docs/` with the old signature → **docs drift**.
3. Branches: `git branch -r --merged origin/HEAD | grep -v HEAD | head -20` → **merged branches still present** (Watch List; deletion is a human decision).
4. Compare with the previous state file; unchanged items keep their entry.

## Report-only vs assisted

- Effective `L1`: state file only.
- `L2`/`L3`: hand one leftover to `loop-fix` when it is a single-file removal or doc line outside `gate.denylist` (a dead flag with one call site, one stale TODO, one wrong doc line). Feature flags that change behaviour, large diffs and anything architectural are human gates.

## Verifier

A change from `loop-fix` goes to the `loop-verifier` sub-agent. Record its verdict line. APPROVE → `fix-proposed`; REJECT → item stays open; ESCALATE_HUMAN or no verdict → `escalated`.

## State file

Rewrite `files.state`:

```
# Post-Merge Cleanup — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] <path or branch> — <dead flag | docs drift | stale TODO>
  Loop action: <what this run did>
  Human decision: <needed, or "none">

## Watch List

- <one line per item>

## Recent Noise (ignored this run)

- <one line per item>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
```

## Rules

- Never push, merge, rebase or delete a branch; a human does.
- One cleanup per run, only through `loop-fix`.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing whether a flag or a TODO is really dead.
- Never disable or delete a test as a "cleanup".

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
