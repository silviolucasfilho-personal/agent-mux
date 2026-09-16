# Loops — fixture-full

## Active Loops

| Loop | Pattern | Cadence | Level | State |
|------|---------|---------|-------|-------|
| Daily Triage | daily-triage | 1d | L1 | STATE.md |

## Human Gates

Design decisions and multi-file refactors escalate to a human.

## Budget

Max tokens per day: 100k. Kill switch: `loop-pause-all`.

## Worktrees

Assisted runs work in a git worktree under `.loop-worktrees/`.

## Connectors

The agent-mux MCP server answers questions about the trace store.

## Safety

`gate.yaml` holds the path denylist; nothing is auto-merged. Update STATE.md after each run.
