# Loop Engineering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Loops sidebar section from which the user schedules, watches, gates and decides on loop-engineering-style runs against a workspace, with every fact (spend, breaker, readiness, tokens, files touched) computed in Rust and every skill authored in this repository.

**Architecture:** `src/loops/` holds the pure logic (patterns, registry, readiness, gate, breaker, cost, run log, schedule, worktree, context) and the store access for `loop_runs`; `App` runs the scheduler on its tick and launches runs through the existing traced spawn path; the `PreToolUse` guard reads `launches.metadata.loop_policy`; `ui.rs` gains the fourth sidebar section, the preview, the dialog and the Loops view; `loops/` carries the embedded skills, verifier and templates.

**Tech Stack:** Rust 2024, ratatui, rusqlite, serde, time, globset (new), Tokio (present). No third-party loop tooling.

**Spec:** [Loop Engineering: scheduled, gated loops from the sidebar](../specs/2026-09-15-loop-engineering-design.md) (revision 2)

## Global Constraints

- Nothing from other vendors is copied, installed or executed; the skills, templates and algorithms are this repository's.
- Claude Code and Codex only; Antigravity profiles are not offered for loops (spec section 16).
- Blocked runs are stored but never appended to `loop-run-log.md`; tokens in the run log always come from the store.
- The loop guard fails closed for write tools on Claude launches (`--loop` on the per-launch hook); Codex, through the installed hooks, fails open on a store error and is caught by the post-run re-check.
- Harness flags are the ones probed on 2026-09-15 (`claude -p --session-id --settings --mcp-config --max-budget-usd`, `codex exec -C -s --json -o -c`); re-probe before changing them.
- Tests use temporary stores, homes, workspaces and fake harness scripts.

---

## Execution map

Baseline: `070020f` on `feat/loop-engineering`.

### Task 1: Shared types and pure modules
- [x] `src/loops/mod.rs`: `Level`, `Outcome`, `Pattern`, `PatternCost`, `LoopPolicy`, `LoopLaunch`, file constants, timestamp and interval helpers.
- [ ] `readiness.rs`: the audit (weights, detectors, 14-day activity, level gates, JSON and human output) with fixtures under `tests/fixtures/loops/`.
- [ ] `gate.rs` (gate.yaml parser + globset), `breaker.rs` (ledger, check, prune), `cost.rs`, `runlog.rs` (append, replace, prune).

### Task 2: Pattern library and scaffolding
- [ ] `loops/registry.toml`, `patterns.rs`.
- [ ] `loops/skills/*` (nine skills), `loops/agents/loop-verifier.md`, `loops/templates/*` (eight files).
- [ ] `scaffold.rs`, `skill::install::install_project`, `tests/loop_skills.rs`.

### Task 3: Store, guard and launch metadata
- [ ] Schema v12 `loop_runs` + `loop_run_stats`; `src/loops/store.rs` (rows, spend, recent, inbox, activity, run facts).
- [ ] `LaunchPlan.loop_launch` → `launches.metadata.loop_*` and `loop_policy`.
- [ ] Guard rules (push/merge, denylist, report-only, max files) and fail-closed `--loop`; hook CLI wiring; `tests/trace_guard.rs`.

### Task 4: Registry, scheduler, worktrees, context, config
- [ ] `registry.rs` (`~/.agent-mux/loops.json`), `schedule.rs`, `worktree.rs` (git + manifest), `context.rs` (snapshot), `config::LoopRunnerConfig`.

### Task 5: Runs in the App
- [ ] Pre-flight, launch composition per harness, post-run accounting, auto-pause, inbox decisions; `tests/loop_runs.rs`.

### Task 6: TUI
- [ ] Four sidebar sections, Loops preview, add/edit dialog, Loops view (`E`), keys, hints, help, mouse; UI tests.

### Task 7: CLI, MCP, doctor, docs
- [ ] `agent-mux loop …`, `agent_mux_get_loop_context`, `trace doctor` lines.
- [ ] README, `docs/loops.md`, `docs/skills.md`, `AGENTS.md`, Heimdall playbook line; spec and plan marked implemented.
