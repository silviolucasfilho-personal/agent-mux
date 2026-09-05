# agent-mux as a workbench for skills, agents, and loop engineering

**Status:** Proposed (plan: `../plans/2026-09-05-agent-workbench.md`)
**Request (2026-09-05):** "transform the agent-mux in a valuable tool for helping people developing skills, agents and loop engineering"
**Builds on:** the SQLite trace store, the hook channel, the Langfuse backend, the tree and timeline views, the per-harness launch options and `src/harness.rs`.

## Purpose

agent-mux now records an agent session precisely: every turn, generation, tool call and subagent, with tokens, cost, hook-pinned timing, and the skills that were loaded. That record is the raw material for engineering agent behaviour — and nothing in the tool yet turns it into a decision. Three people would use it if it did:

- **The skill author** writes a `SKILL.md` and wants to know: did it trigger when it should, did it trigger when it shouldn't, what did each use cost, and is version two better than version one.
- **The agent author** defines a subagent (prompt, tools, model) and wants to know: how long it runs, what it spends, how often it fails, and what its tool loop looks like from the inside.
- **The loop engineer** tunes the outer loop — prompts, tool sets, models, approval policy, budgets — and wants to know where time and money go inside a turn, which loops spin, and whether a change moved the numbers.

All three ask the same shape of question: *run it, look at the loop, compare, judge, repeat*. This design makes each step a first-class thing in the tool.

## Principles

1. **Measure, never estimate.** Every number shown is a provider-reported count, a hook-pinned time, or arithmetic over those. This week's comparison against Langfuse showed estimation double-counting real calls; the tool's credibility is that its figures are the bill.
2. **The store is the substrate.** Metrics are SQL views over rows that already exist. New capture is a column or a small table on the same writer; there is no second pipeline. Anything that cannot be derived from a trace is not a metric.
3. **An experiment is a launch.** The one-shot prompt, model, approval and resume options built this week are the runner; an experiment run is an ordinary launch carrying two labels. Nothing runs that the user could not have launched from the dialog.
4. **Skills and agents are files, discovered per working directory.** Each harness has documented roots; the tool reads them where a session runs and never registers or copies anything.
5. **Human judgment is the only fact the tool invents.** A score is a person's verdict on a turn, stored beside the turn, and exported to Langfuse as a score — the one write Langfuse accepts on every deployment mode.
6. **Fail-open, harness-explicit.** No feature may slow or block a session unless the user asked for a guard; and where a CLI lacks a concept (agy has no per-launch hooks, Codex spells resume as a subcommand) the difference is stated, not papered over.

## What already exists to build on

| Fact | Where it lives today |
|---|---|
| Skills loaded per turn, and per generation/tool | `traces.skills`, `observations.skill` |
| Subagent invocations with type, model, role, nesting | agent observations, `metadata.agent_type`, `parent_id` |
| Precise tool start/end, running rows, declined calls, exit codes | `hook_timed`, `status_message`, `is_error` (now only for real failures) |
| Turn events: compaction, interruption, API error, model switch, stray subagent stops | `traces.metadata` |
| Tokens by bucket (input, cache read/write, output, reasoning) and cost per generation | observation columns, priced by the store |
| A turn's geometry as a tree and a timeline | `tracing::view` |
| One-shot launches with model, approvals and resume per harness | `harness::LaunchOptions` |
| A live and replayable path to Langfuse, including its score event type | `tracing::langfuse` |

## Phase 1 — Make the loop legible

Derived, per-turn *loop metrics*, defined exactly so they mean the same thing on every harness:

| Metric | Definition |
|---|---|
| `tool_calls`, `distinct_tools` | tool observations in the turn; distinct by name |
| `tool_errors`, `declined` | `is_error` rows; rows with `status_message = declined by the user` |
| `retries` | a tool called again with byte-identical input within the same turn |
| `model_ms`, `tool_ms`, `idle_ms` | union of generation spans; union of tool spans; turn span minus both — idle is the model thinking between calls or the user reading |
| `context_first`, `context_last`, `context_growth` | input + cache-read tokens of the first and last generation; the difference |
| `cache_ratio` | cache-read / (input + cache-read) over the turn |
| `compactions`, `subagents`, `subagent_cost` | from turn metadata and agent rows |
| `cost_per_turn` | already in `trace_stats`; joined here for one row per turn |

These live in a `loop_stats` view and two rollups: `skill_stats` (per skill: turns it was loaded in, generations and tools attributed to it, tokens, cost, and turns where it was loaded but nothing was attributed — the *loaded-but-unused* signal) and `agent_stats` (per agent type: invocations, mean and p90 duration, tokens, cost, error rate).

Surfaces: `trace loops [session]`, `trace skills`, `trace agents` in the CLI; a *loop* view in the browser's Detail pane beside list, tree and timeline (one screen of the numbers above with the retries and errors named); and a compact loop line on each turn in the Turns pane (`7🔧 2↻ 1!` — calls, retries, errors).

## Phase 2 — Make variants comparable

An **experiment** is a named task run under several **variants**. Schema v3 adds `experiments (id, name, prompt, cwd, check_cmd, created_ns)` and `experiment_runs (launch_id, experiment_id, variant, outcome, outcome_detail)`; a run is a normal launch whose row is linked here.

A headless runner drives it: `agent-mux run --experiment <name> --variant <label> --prompt "…" [--harness claude|codex|agy] [--model M] [--bypass] [--check "cargo test"] [--repeat N]`. It launches through the existing planner with the one-shot option, traces as usual, waits for exit, runs the optional check command in the working directory, and records the outcome (exit code, check result, final assistant message). The dialog gets the same two labels as optional fields, so an interactive session can be a run too.

`trace compare <A> <B>` puts two turns or two sessions side by side: the loop metrics in two columns with deltas, then a diff of the tool sequence (`Read a.rs → Edit a.rs → Bash cargo test` against the other) so a changed loop is visible as a path, not only as a total. `trace experiments [name]` summarises per variant: runs, pass rate, mean and p50 cost, mean turns, mean wall time.

What this deliberately does not do: prepare the working directory. Two variants that edit files will interfere unless run in separate checkouts; v1 states this and leaves setup to the user (`--cwd` per run, `git worktree` recommended). A `--setup` hook is a natural follow-up once the runner exists.

## Phase 3 — Skills and agents as first-class objects

An **inventory** of what could trigger, read from disk for the session's working directory and the user's home, per harness:

| Harness | Roots (verified at implementation) |
|---|---|
| Claude Code | `.claude/skills/*/SKILL.md`, `.claude/agents/*.md`, `.claude/commands/*.md`, and the same under `~/.claude/` |
| Antigravity | `.agents/skills/`, `.agents/plugins/*/`, `~/.gemini/config/` (skills, plugins, `skills.json`/`plugins.json` entries) |
| Codex | `~/.codex/skills/`, `AGENTS.md` in the repository |

Frontmatter is parsed (name, description, allowed tools, model) and joined to the trace attribution from Phase 1, giving each skill and agent a record: first and last seen, trigger count, sessions and turns it appeared in, cost, and — the number authors most want — turns whose prompt matched the skill's own trigger phrases but where it did *not* load. That last one needs the description's trigger phrases, which is exactly what Claude's skill format asks authors to write.

A conservative **lint** reports what stops a skill working before any run does: missing or malformed frontmatter, an empty or one-word description, tools named that the harness does not have, an agent whose model is unknown to the price table. It does not judge prose quality; that is what a run is for.

Surfaces: `trace skills` and `trace agents` grow the inventory columns; `trace skills lint`; a Skills pane in the browser (`K`) listing the inventory with trigger counts, `Enter` filtering the Turns pane to that skill's turns.

## Phase 4 — Feedback and guards

**Scores.** A `scores (id, target, target_id, name, value, comment, created_ns)` table; `s` on a turn in the browser and `trace score <turn> good|bad [--note …]` in the CLI. Scores flow to Langfuse as `score-create` events through the existing exporter — the one event type a v4 `events_only` deployment accepts on the ingestion endpoint, verified this week — so a judgment made in the terminal appears on the same trace in Langfuse. `trace experiments` then shows the score distribution per variant, which is what "is v2 better" finally means: pass rate, cost, and a human verdict on the same rows.

**Budget guards.** An optional `--max-cost` / `--max-turns` per launch, enforced through the hook channel: the hook binary already sees every `PreToolUse`, and on Claude and Codex a hook may refuse the call (a `decision` in its JSON reply, or exit code 2). Past the budget the guard blocks the next tool call with a message naming the limit and the spend, and the status bar says so. Antigravity is excluded: its `PreToolUse` requires a `decision` on every call, which is why it was left unregistered, and a guard that must answer every call is a different design.

**Loop warnings.** Three patterns the metrics make detectable, surfaced as a status-bar notice and a turn metadata flag, never as an intervention: a *tool storm* (more than N calls of one tool in a turn), *ping-pong* (the same file read and edited alternately more than N times), and *no progress* (three consecutive turns with identical tool sequences). The thresholds are settings with conservative defaults.

## What this is not

Not a skill marketplace or a place to publish them; not automatic prompt optimisation; not LLM-judged scoring (a later feature could write scores through the same table); not cross-machine aggregation. Each is compatible with this design and none is required by it.

## Risks and what gets verified before code

- **Skill attribution per harness.** Claude's transcript marks skill loads explicitly and the parser already reads them; whether agy and Codex expose an equivalent is unverified. Phase 1 reports attribution where the transcript supports it and says `n/a` elsewhere rather than inferring.
- **Retry and pattern thresholds** are heuristics by nature. They are settings, they default conservatively, and they are shown as counts the user can inspect, never as verdicts.
- **Hook refusal semantics** differ per CLI version; the guard is tested against the installed binaries the way the launch options were.
- **Experiment isolation** is the user's responsibility in v1, stated plainly in the runner's help.

## Files

| File | Change |
|---|---|
| `src/tracing/store/schema.rs` | v3: `experiments`, `experiment_runs`, `scores`; views `loop_stats`, `skill_stats`, `agent_stats`. |
| `src/tracing/loops.rs` (new) | retry detection, time-split arithmetic, pattern detection. |
| `src/tracing/inventory.rs` (new) | per-harness skill/agent discovery, frontmatter parsing, lint. |
| `src/tracing/experiments.rs` (new) | registry, runner, compare, summaries. |
| `src/tracing/cli.rs` | `loops`, `skills`, `agents`, `compare`, `experiments`, `score`; `agent-mux run`. |
| `src/tracing/hooks/` | budget guard replies on `PreToolUse`. |
| `src/tracing/langfuse/map.rs` | `score-create` export. |
| `src/app.rs`, `src/ui.rs` | loop view, loop line, Skills pane, scoring key, experiment labels in the dialog. |
