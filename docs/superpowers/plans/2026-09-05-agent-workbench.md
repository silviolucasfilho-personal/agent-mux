# Agent workbench: implementation plan

Spec: `../specs/2026-09-05-agent-workbench-design.md`. Four phases, each shippable on its own and each ending green (`cargo clippy --all-targets`, `cargo test --no-fail-fast`). Phase 1 is pure derivation over existing rows and carries no schema change; Phase 2 introduces schema v3.

## Phase 1 — Make the loop legible

### Task 1: Loop metrics (`src/tracing/loops.rs`, `src/tracing/store/schema.rs`)

**Interfaces:** `LoopMetrics { tool_calls, distinct_tools, tool_errors, declined, retries, model_ms, tool_ms, idle_ms, context_first, context_last, cache_ratio, compactions, subagents, subagent_cost }`; `loop_metrics(turn: &TraceStat, obs: &[ObservationView]) -> LoopMetrics` (pure); `retries(obs) -> Vec<(name, count)>`; `time_split(turn_start, turn_end, obs) -> (model_ms, tool_ms, idle_ms)` using span unions; a `loop_stats` view for the SQL-only parts (counts, tokens, cost).

- [x] **Step 1:** Tests: retries count byte-identical tool inputs only; overlapping tool spans are unioned, not summed (subagent children inside their parent); idle is never negative; context growth from first/last generation; cache ratio with no cache reads is 0; an empty turn yields zeros.
- [x] **Step 2:** Implement. Views live only in migrations here, so phase 1 carries a small V3 after all: `loop_stats`, `skill_stats`, `agent_stats`, and `trace_stats` recreated with `retries` and `declined`. Read-only CLI commands migrate an older store in place first (`store::migrate_in_place`), without opening a run.

### Task 2: Skill and agent rollups (`src/tracing/store/query.rs`, schema views)

**Interfaces:** `skill_stats` view (skill, turns_loaded, generations, tools, tokens, cost, turns_unused); `agent_stats` view (agent_type, invocations, mean_ms, p90_ms, tokens, cost, error_rate); `query::skill_stats(conn, filter)`, `query::agent_stats(conn, filter)`.

- [x] **Step 1:** Tests over a seeded store: a skill loaded in two turns and attributed in one reports `turns_unused = 1`; agent p90 over five invocations; an agent with one failed child counts one error.
- [x] **Step 2:** Implement; p90 is nearest-rank over per-invocation durations, computed in Rust beside the view.

### Task 3: Surfaces (`src/tracing/cli.rs`, `src/app.rs`, `src/ui.rs`)

- [x] **Step 1:** Tests: `trace loops` prints one row per turn with the metric columns; `trace skills` and `trace agents` shapes; the browser's `loop` detail view renders the numbers and names each retried tool; the Turns pane line shows `N🔧 R↻ E!` and hides the zero parts.
- [x] **Step 2:** Implement; `v` cycles list → tree → timeline → loop.

## Phase 2 — Make variants comparable

### Task 4: Schema v3 and the registry (`src/tracing/store/schema.rs`, `src/tracing/experiments.rs`)

**Interfaces:** tables `experiments`, `experiment_runs`, `scores` (created here, used in Phase 4); `Experiment { id, name, prompt, cwd, check_cmd }`; `Run { launch_id, variant, outcome: Outcome, detail }`; `Outcome { Pass, Fail, Unknown }`; `store::upsert_experiment`, `store::record_run`, `query::experiment_summary(name) -> Vec<VariantSummary { variant, runs, pass_rate, mean_cost, p50_cost, mean_turns, mean_wall_ms, scores }>`.

- [ ] **Step 1:** Tests: v2 store migrates to v3 and keeps every row; a run links to its launch; summary math over seeded runs; a variant with no check reports `Unknown`, never `Pass`.
- [ ] **Step 2:** Implement.

### Task 5: The runner (`src/main.rs`, `src/tracing/experiments.rs`)

**Interfaces:** `agent-mux run --experiment E --variant V --prompt P [--harness H] [--model M] [--bypass] [--cwd DIR] [--check CMD] [--repeat N]`; builds a `Profile` for the harness, renders `LaunchOptions { one_shot: P, model, bypass_approvals }` through `harness::compose`, launches through the trace runtime headlessly (no TUI), waits for exit, runs the check in `cwd`, records the run; the help text states that variants which edit files need separate checkouts.

- [ ] **Step 1:** Tests: the composed argv per harness; a fake `claude` script drives a run end to end and the run row carries exit code, check outcome and the final message; `--repeat 3` records three runs under one variant.
- [ ] **Step 2:** Implement; the dialog gains optional Experiment and Variant fields that link an interactive launch the same way.

### Task 6: Compare and summarise (`src/tracing/cli.rs`)

- [ ] **Step 1:** Tests: `trace compare A B` prints two metric columns with signed deltas and a tool-sequence diff with `+`/`-`/` ` lines; `trace experiments E` prints one row per variant.
- [ ] **Step 2:** Implement.

## Phase 3 — Skills and agents as first-class objects

### Task 7: Inventory (`src/tracing/inventory.rs`)

**Interfaces:** `Definition { harness, kind: Skill|Agent|Command, name, path, description, tools, model, triggers: Vec<String> }`; `inventory(harness, cwd, home) -> Vec<Definition>`; frontmatter parsing tolerant of missing fields; the root list per harness verified against the installed CLIs before it is written down.

- [ ] **Step 1:** Tests: fixture trees per harness; a skill with no frontmatter still lists with its directory name; project roots shadow home roots of the same name; trigger phrases split from the description.
- [ ] **Step 2:** Implement.

### Task 8: Attribution join and lint (`src/tracing/inventory.rs`, `src/tracing/cli.rs`, `src/app.rs`, `src/ui.rs`)

- [ ] **Step 1:** Tests: `trace skills` joins inventory to `skill_stats` and reports `never triggered` and `missed` (prompt matched a trigger phrase, skill not loaded) from a seeded store; lint flags empty description, unknown tool, unknown model, and passes a well-formed skill; the browser's Skills pane (`K`) lists definitions and `Enter` filters Turns.
- [ ] **Step 2:** Implement.

## Phase 4 — Feedback and guards

### Task 9: Scores (`src/tracing/store/mod.rs`, `src/tracing/cli.rs`, `src/app.rs`, `src/tracing/langfuse/map.rs`)

- [ ] **Step 1:** Tests: `trace score <turn> good --note x` stores a row; `s` in the browser toggles good/bad on the selected turn; a score exports as a `score-create` event with the trace id and is accepted by the fake server; `trace experiments` shows scores per variant.
- [ ] **Step 2:** Implement.

### Task 10: Budget guard (`src/tracing/hooks/mod.rs`, `src/tracing/cli.rs`, `src/tracing/mod.rs`)

**Interfaces:** `--max-cost USD` / `--max-turns N` on `run` and in the dialog; the launch row records the limits; the hook binary, on `PreToolUse`, reads the launch's current spend from the store and answers with the harness's refusal (Claude: a `decision` reply; Codex: exit 2) once the limit is passed, with a message naming the limit and the spend; a status-bar notice; agy not registered.

- [ ] **Step 1:** Tests: under the limit the hook answers permit; over it the reply is the refusal shape verified against each installed CLI; the store lookup respects the hook's 150 ms budget and permits on any error (fail-open).
- [ ] **Step 2:** Implement.

### Task 11: Loop warnings (`src/tracing/loops.rs`, `src/tracing/mod.rs`)

- [ ] **Step 1:** Tests: tool storm, ping-pong and no-progress each detected at the default threshold and not below it; a flagged turn carries the pattern in metadata; one notice per launch per pattern.
- [ ] **Step 2:** Implement; thresholds in `[tracing.loops]` with documented defaults; spec status.
