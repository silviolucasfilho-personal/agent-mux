# Workflows Implementation Plan

**Goal:** Multi-agent workflows composed from a TOML document and step skills, interpreted in Rust, with every session a headless traced run of Claude Code, Codex CLI or Antigravity; a Workflows sidebar section and view; a planner that composes a workflow for a task; a CLI.

**Spec:** [Workflows design](../specs/2026-09-17-workflows-design.md)

**Tech Stack:** Rust 2024, ratatui, rusqlite, toml, serde_json; no new dependencies.

## Probe results (2026-09-17, installed CLIs)

| | Claude Code 2.1.274 | Codex CLI 0.154.0 | Antigravity 1.2.4 |
| --- | --- | --- | --- |
| Print mode | `-p/--print` | `exec <prompt>` | `-p` is `--print` |
| Envelope | `--output-format json` | `--json` (JSONL events); `-o/--output-last-message FILE` | `--output-format json` |
| Native schema | `--json-schema <schema>` | `--output-schema <FILE>` | `--json-schema <str\|path>` |
| Model | `--model` | `-m/--model` | `--model` |
| Budget | `--max-budget-usd` (print only) | none | none |
| Approvals | `--dangerously-skip-permissions` | `--dangerously-bypass-approvals-and-sandbox` (`--yolo`) | `--dangerously-skip-permissions` |
| Working dir | cwd | `-C/--cd DIR`, `--skip-git-repo-check` | cwd |
| Timeout | none | none | `--print-timeout` (default 5m) |
| Mode | `--permission-mode` | `-s/--sandbox` | `--mode accept-edits\|plan`, `--sandbox`, `--disable-slash-commands` |

Decisions from the probe: the final text is read from the envelope (Claude JSON `result`; Codex `-o` file; agy JSON) with `traces.output` and the scrollback as fallbacks; structured results stay the fenced `workflow-result` block on every harness (native schema flags are recorded, not used, so a result never depends on the harness); agy `--sandbox` is not passed (its terminal restrictions are unverified against step skills) and stays a documented follow-up.

## Execution map

| Task | Files | Verify |
| --- | --- | --- |
| A1 Document model | `src/workflows/document.rs` (parse, validate, paths, predicates, schemas) | unit tests |
| A2 Interpreter | `src/workflows/interp.rs` (pure state machine: `RunState`, `next()`, `complete()`), `src/workflows/result.rs` (fenced block, schema validation) | unit tests with a fake runner |
| A3 Execution | `src/workflows/harness.rs` (argv per harness, envelope parsing), `src/workflows/context.rs`, `src/workflows/journal.rs`, `src/workflows/store.rs`, schema migration, `src/app/workflows.rs` (live runs, spawn, stdout capture, settle, budget, timeouts, cancel) | `tests/workflow_runs.rs` with fake harnesses |
| A4 Library and built-ins | `src/workflows/library.rs`, `workflows/*.toml`, `workflows/skills/wf-*/`, `skills/workflow-author/`, `skill.toml` keys `hidden`/`writes`, catalog `Kind::Workflow`, `prompts.toml` `[workflow]` | `config check`, catalog tests |
| A5 CLI | `src/workflows/cli.rs` (`ls show check skills run runs status cancel save plan`), `main.rs` | `tests/workflow_cli.rs` |
| B Sidebar, dialog, view | `src/app.rs` (section, keys), `src/app/workflows_view.rs`, `src/ui.rs` | `tests/workflow_ui.rs` |
| C Planner | `src/workflows/planner.rs`, `n`, `plan`, approval | tests with a fake planner |
| D Isolation, MCP, doctor | worktrees per session, Changes tab, `agent_mux_get_workflow_run`, doctor section | tests |
| E Docs | `docs/workflows.md`, README, AGENTS.md | review |

## Interpreter model

A run is a pure state machine so the TUI, the CLI and the tests drive it the same way:

- `RunState::new(doc, args, caps)`; `next()` returns the sessions that may start now; `complete(key, outcome)` records a session's result; `status()` is running, finished with a value, failed, cancelled or budget-exhausted.
- A session key is `(step, item index, role)` with roles main, vote(i), generate(i), judge(round, pair). Per-item chains (main → votes) run independently; steps are barriers.
- Step kinds: `single`, `fanout` (skill required), `pipeline` (skill optional, transforms and verify over items), `route` (classifier + branches), `tournament` (generate or `over`, pairwise judge bracket), `until` (rounds, `dedupe_by`, stop after `rounds_without_new`).
- Transforms per item step, in order: `dedupe_by`, `keep`, `verify`, `take`.

### Task A1

- [x] `Workflow`, `Step`, `StepKind`, `Actor` (skill or prompt), `Over`, `PathExpr`, `Predicate`, `Schema`, `ArgSpec`; `parse(text)`; `validate(&doc, &known_skills)`; `resolve(path, env)`; `interpolate(text, env)`; `Schema::to_json_schema()`, `Schema::check(value)`.

### Task A2

- [x] `RunState` with per-step `StepState`; `next()`/`complete()`; transforms; verify aggregation; route gating; tournament bracket; until rounds; budget, caps, cancel; journal replay.
- [x] `result.rs`: `extract_fenced`, `validate_against`.

### Task A3

- [x] `harness.rs`: `session_args(harness, prompt, model, usd_cap, timeout, out_file)`; `final_text(harness, stdout, out_file)`.
- [x] `context.rs`: `Context` document and `write`.
- [x] `journal.rs`: append and load.
- [x] `store.rs`: `workflow_runs`, `workflow_steps`, `upsert_run`, `upsert_session`, `get_run`, `recent_runs`; schema v+1.
- [x] `app/workflows.rs`: `LiveWorkflowRun`, `start_workflow_run`, tick pass (timeouts, settle, complete, launch), stdout capture, cancel, store writes, `result.json`.

### Task A4

- [x] `library.rs`: built-ins, `~/.agent-mux/workflows/*.toml`, skill-distributed; `Kind::Workflow` in `assets`.
- [x] Built-in documents and step skills; `workflow-author`; `hidden`, `writes` keys; multiple compiled-in packages.

### Task A5

- [x] `cli.rs`; `main.rs` dispatch; headless runner driving a private `App`.

### Task B

- [x] `SidebarSection::Workflows`, `sidebar_areas` five ways, `draw_workflows_sidebar`, `draw_workflow_preview`, keys, run dialog, `WorkflowsViewState` + `draw_workflows_view`, `W`.

### Task C

- [x] `planner.rs`: inventory, launch with `workflow-author`, extract `workflow-toml`, validate, approval; `n`, `plan`.

### Task D

- [x] Worktree per isolated session (kept worktrees are noted in the Progress tab and recorded on the step row), `agent_mux_get_workflow_run`. Not shipped: a separate Changes tab and a `trace doctor` section.

### Task E

- [x] `docs/workflows.md`, README, AGENTS.md, spec status.
