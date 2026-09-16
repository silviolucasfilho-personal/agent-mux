# Loop Constraints — agent-mux

Binding for every loop run. The `loop-rules` skill reads this file at the start of a run; agent-mux enforces the mechanical rules whether or not the skill did.

## Push & Merge

- Never push to a shared branch; never force-push anywhere.
- Never merge, rebase or squash; a human merges the `loop/<run_id>` branch.
- Never open, close or merge a pull request.

## Paths

- Never write a path matching a `gate.yaml` denylist entry: environment files, secrets, keys, credentials, auth, payments, migrations, workflows, infrastructure.
- Never create files outside the workspace or the run's worktree.

## Code

- One fix per run; at most three attempts; one commit.
- At most `maxFiles` files from `gate.yaml` (default 10).
- Never disable, skip, delete or weaken a test; never change CI configuration to pass.
- No new dependencies, no formatter sweeps, no refactors beyond the named item.

## Communication

- Escalate instead of guessing: an open question goes to High Priority with `Human decision:`.
- Every run rewrites the state file and ends with a `loop-result` block.
- Report numbers agent-mux measured; never estimate tokens or cost.

## Budget

- At 80 % of the daily token cap the run is report-only; at 100 % it does not start.
- `loop-pause-all` in the state file or `LOOP.md` stops every loop of this workspace.
