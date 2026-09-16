---
name: loop-changelog
description: Use when agent-mux runs the changelog-drafter loop or the user asks to "draft the changelog", "release notes since the last tag", "what shipped this week" or "update the unreleased section". Reads the loop context, collects merges since the last tag and drafts a release-notes section for a human to edit.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-changelog — release notes from the merges since the last tag

You are one run of a scheduled loop that drafts release notes. Budget, level and state file come from agent-mux.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.state`, `run.level_effective`, `budget.mode`, `gate.denylist`, `previous_run`.
3. Read the current state file and the existing `CHANGELOG.md` (or `RELEASE_NOTES*.md`) head, if any.

## Procedure

1. `git describe --tags --abbrev=0 2>/dev/null` gives the last tag; none → use `--since="30 days ago"`.
2. `git log <tag>..HEAD --merges --format="%h %s%n%b" | head -120` and, for repositories that squash, `git log <tag>..HEAD --no-merges --format="%h %s" | head -80`. One pass; no per-commit diffs except to disambiguate a title (`git show --stat <sha> | head -20`, at most five).
3. Group entries: **Added**, **Changed**, **Fixed**, **Removed**, **Security**, **Breaking**. Drop merge-noise (version bumps, "merge branch", CI-only changes) into Recent Noise.
4. Items that are **Breaking**, **Security**, a major feature or anything a marketing reader would see are human gates: draft them, but list them in High Priority with `Human decision: wording`.
5. Compare with the previous state file so entries already drafted are not duplicated.

## Report-only vs assisted

- Effective `L1`: the draft goes into the state file only.
- `L2`/`L3`: you may write the draft as an `## Unreleased` section at the top of `CHANGELOG.md` (one file, no other change, never a tag or a version bump) and hand it to the verifier. That is the only write outside the state file, and only when `CHANGELOG.md` is not in `gate.denylist`.

## Verifier

At `L2`/`L3` the `loop-verifier` sub-agent checks that every entry maps to a commit in the window and nothing else changed. Record its verdict line. APPROVE → `fix-proposed`; REJECT → draft stays in the state file; ESCALATE_HUMAN or no verdict → `escalated`.

## State file

Rewrite `files.state`:

```
# Changelog Drafter — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] <entry> — breaking | security | major | wording
  Loop action: drafted
  Human decision: <needed>

## Watch List

- <draft entries, grouped Added / Changed / Fixed / Removed>

## Recent Noise (ignored this run)

- <merge noise, one line each>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
```

## Rules

- Never push, merge, tag or bump a version; a human publishes.
- One file at most (`CHANGELOG.md`), one draft per run.
- Never write a path matching `gate.denylist`.
- Escalate instead of guessing what a commit did; quote the title and ask.
- Never disable or edit a test.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
