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

- [x] **Step 1:** Tests: the v1 fixture migrates all the way to the current version; a run links to its launch and re-recording replaces rather than duplicates; summary math over two real runs; a variant with no check reports `Unknown` and no pass rate.
- [x] **Step 2:** Implement. Phase 1 already took v3 for the views, so this is **V4**: `experiments`, `experiment_runs`, `scores`. The registry lives in `src/tracing/experiments.rs` rather than `store`/`query`, beside the runner that fills it; `store::open_aux` is the plain read-write connection those writes use, outside the writer thread.

### Task 5: The runner (`src/main.rs`, `src/tracing/experiments.rs`)

**Interfaces:** `agent-mux run --experiment E --variant V --prompt P [--harness H] [--model M] [--bypass] [--cwd DIR] [--check CMD] [--repeat N]`; builds a `Profile` for the harness, renders `LaunchOptions { one_shot: P, model, bypass_approvals }` through `harness::compose`, launches through the trace runtime headlessly (no TUI), waits for exit, runs the check in `cwd`, records the run; the help text states that variants which edit files need separate checkouts.

- [x] **Step 1:** Tests: the composed argv per harness; a fake `claude` drives two runs end to end (one passing check, one failing) and the rows carry exit code, outcome and the final message; the dialog's Experiment/Variant fields record an interactive launch at exit. `--repeat` reuses the single-run path, so it is covered by the runner test plus the arg parser rather than a three-run fixture.
- [x] **Step 2:** Implement. One rule added while testing: a Claude one-shot skips the planner's `--session-id` injection (the hook channel normally announces it), so the runner pins a fresh session id per run itself and correlates whether or not hooks are on. Interactive links are recorded when the session ends (or the app quits), since the run row hangs off the launch row and that row reaches the store through the writer thread.

### Task 6: Compare and summarise (`src/tracing/cli.rs`)

- [x] **Step 1:** Tests: two sessions resolve as sides with loop metrics and tool paths, the LCS diff marks `+`/`-`/` `; `summary_lines` prints one row per variant; the smoke on the real store shows unpriced sides as `-`, never `$0.00`.
- [x] **Step 2:** Implement.

## Phase 3 — Skills and agents as first-class objects

### Task 7: Inventory (`src/tracing/inventory.rs`)

**Interfaces:** `Definition { harness, kind: Skill|Agent|Command, name, path, description, tools, model, triggers: Vec<String> }`; `inventory(harness, cwd, home) -> Vec<Definition>`; frontmatter parsing tolerant of missing fields; the root list per harness verified against the installed CLIs before it is written down.

- [x] **Step 1:** Tests: fixture trees per harness (Claude project/home/plugin via `installed_plugins.json`, Codex `AGENTS.md`/home/plugin cache with two versions, agy project/workflows/home/plugins); a skill with no frontmatter lists by directory name; project shadows home shadows plugin; trigger phrases split from the description; frontmatter shapes (scalar, `|` block, `- ` list, multi-line `[ … ]`).
- [x] **Step 2:** Implement. Roots verified on the installed CLIs and written into the module header. Two additions to the interface: `Kind::Instructions` for a repository's `AGENTS.md` (listed, never linted — it is not a trigger), and `Scope { Project, Home, Plugin(name) }` on every definition, since a plugin skill is recorded by the store as `plugin:name` and the join has to know both spellings. Codex plugins keep several versions in the cache; the newest directory stands for the plugin.

### Task 8: Attribution join and lint (`src/tracing/inventory.rs`, `src/tracing/cli.rs`, `src/app.rs`, `src/ui.rs`)

- [x] **Step 1:** Tests: the join over a seeded store reports `missed` (two prompts naming a trigger phrase in turns that did not load the skill), `never triggered` and `not on disk`; `traces_with_skill` and `tool_names`; lint flags no frontmatter, empty/one-word/over-long description, name mismatch, no trigger phrases, unknown tool, unknown model, and passes a well-formed skill — and stays quiet on tools when no known list exists; the browser's `K` pane lists, `j`/`k` clamp, `Enter` filters Turns to the skill, and works without a store.
- [x] **Step 2:** Implement. The known-tool set is measured, not recalled: Claude's floor list plus every tool name the store has seen that CLI call; Codex and agy have no floor, so only what was measured counts, and with nothing measured the rule is skipped. `trace agents` gained a "defined agents" section joined to `agent_stats` by name. On this machine: 13 definitions, 0 findings; one stored prompt already matched `agent-development`'s trigger phrase without loading it.

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
