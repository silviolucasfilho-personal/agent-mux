# Agents in this repository

Scheduled loops run here through agent-mux. Each run is one traced session with a budget, a level and a state file.

- Week one is report-only (L1): a run writes its state file and nothing else.
- Assisted runs (L2+) work in `.loop-worktrees/<run_id>` on branch `loop/<run_id>` and propose one change at most; a human merges.
- State lives in the pattern's state file (`STATE.md` or `<pattern>-state.md`); the run history in `loop-run-log.md`.
- Rules: `loop-constraints.md`; path gate: `gate.yaml`; caps: `loop-budget.md`.
- Test command: `npm test` / `cargo test` / `pytest -q` as the project provides; lint the same way. Never disable a test to pass.
