---
name: loop-dependency-triage
description: Use when agent-mux runs the dependency-sweeper loop or the user asks to "sweep dependencies", "what is outdated", "any vulnerable packages" or "bump the safe ones". Reads the loop context, lists outdated and vulnerable dependencies from the project's own tooling and proposes at most one patch-level bump.
allowed-tools: Read, Grep, Glob, Bash, Write, Edit
---

# loop-dependency-triage — outdated and vulnerable dependencies

You are one run of a scheduled loop over the project's dependencies. Budget, level and state file come from agent-mux.

## Setup

1. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` first. Missing → print `no loop context` and stop.
2. Take `files.state`, `files.ledger`, `run.level_effective`, `budget.mode`, `breaker`, `gate.denylist`, `gate.max_files`, `previous_run`.
3. `breaker.status` other than `ok` → report-only this run.
4. Read the current state file.

## Procedure

Use only tools already present; never install one.

1. Detect the ecosystem and run one listing: `cargo outdated --root-deps-only 2>&1 | head -40` (if installed) or `cargo update --dry-run 2>&1 | head -40`; `npm outdated --json 2>/dev/null | head -80`; `pip list --outdated 2>/dev/null | head -40`; `go list -m -u all 2>/dev/null | grep '\[' | head -40`.
2. Run one audit when available: `cargo audit 2>&1 | tail -40`, `npm audit --json 2>/dev/null | head -80`, `pip-audit 2>&1 | tail -40`, `govulncheck ./... 2>&1 | tail -40`.
3. Bucket:
   - a vulnerability rated high or critical → **High Priority**, human gate (never auto-bumped).
   - a major version behind → **High Priority** when the package is a direct dependency, human gate.
   - a minor or patch behind, direct dependency, lockfile present → **Watch List** (fix candidate).
   - transitive-only drift → **Recent Noise**.
4. Check the denylist of packages in `loop-constraints.md` (Paths / packages section) and the ledger: a bump that failed before with the same error is not retried.

## Report-only vs assisted

- Effective `L1`: state file only.
- `L2`/`L3`: hand **one** patch-level bump of one direct dependency to `loop-fix` (manifest and lockfile only, tests must pass). Minor bumps only when the project pins minors; major bumps, CVE fixes that need code changes and denylisted packages are human gates.

## Verifier

The bump goes to the `loop-verifier` sub-agent, which must run the tests and the audit again. Record the verdict line. APPROVE → `fix-proposed`; REJECT → back to Watch List with the reason; ESCALATE_HUMAN or no verdict → `escalated`.

## State file

Rewrite `files.state`:

```
# Dependency Sweeper — <project>

Last run: <RFC3339>

## High Priority (loop is acting or waiting on human)

- [ ] <package> <current> → <available> — <CVE-… | major bump>
  Loop action: <what this run did>
  Human decision: <needed, or "none">

## Watch List

- <package> <current> → <available> — patch | minor

## Recent Noise (ignored this run)

- <one line per item>

---
Run log: <timestamp> | <findings> findings | <actions> actions | <escalations> escalations
```

## Rules

- Never push, merge or rebase; the bump lives on `loop/<run_id>` for a human.
- One bump per run, only through `loop-fix`.
- Never write a path matching `gate.denylist`; never exceed `gate.max_files`.
- Escalate instead of guessing whether a bump is safe.
- Never disable a test, an audit or a lockfile to make a bump pass.

## Finish

End with this block, once. `outcome` ∈ `report-only | fix-proposed | escalated | no-op`:

```loop-result
{"outcome":"report-only","items_found":0,"actions_taken":0,"escalations":0,"summary":"one line"}
```
