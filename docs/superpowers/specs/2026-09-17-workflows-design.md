# Workflows: multi-agent workflows in Rust and skills, on every harness

Status: Implemented on 2026-09-17 (see the plan of the same date); the planner key is `c`, not `n`, so `n` keeps opening a new session; a Changes tab did not ship, worktrees with changes are noted in the Progress tab and the step rows.
Date: 2026-09-17
Baseline: `6c263b7` on `master` (configuration library merged).
Reference: Anthropic, "A harness for every task: dynamic workflows in Claude Code" (claude.com/blog, 2026). Quotations are from that post.

## 1. Problem and outcome

The post names the failure modes of one long context that plans and executes at once: *agentic laziness* (declaring partial progress done), *self-preferential bias* (a model approving its own work) and *goal drift* (objectives eroding through summaries). Its answer is a **dynamic workflow**: a harness "custom-built for the task at hand" that orchestrates "separate Claude subagents with their own context windows and focused, isolated goals", decides "which models an agent uses and whether subagents are run in their own worktree", can be resumed, carries "explicit token usage budgets", and can be saved and shared. It describes six shapes: classify-and-act, fan-out-and-synthesize, adversarial verification, generate-and-filter, tournament, loop until done.

That capability exists only inside Claude Code, as a JavaScript program the model writes. agent-mux already has the shape this project uses for model-facing automation: **a registry of patterns in TOML, skills that do the model-facing work, and Rust that computes facts, enforces limits and orchestrates** (loops, Heimdall). Workflows follow the same shape, with one addition: a small fixed vocabulary of composition primitives interpreted in Rust.

Outcome: a **Workflows** sidebar section and view; a **workflow document** (TOML) whose steps are skills or inline prompts composed by `single`, `fanout`, `pipeline`, `route`, `tournament` and `until`, with adversarial `verify`, transforms and schemas; a **Rust interpreter** that runs each step as a headless, traced session of **Claude Code, Codex CLI or Antigravity**, per run and per step; and a **planner** skill that composes a workflow for a task the user types. Every step is watchable live in Active; results are structured; the document and the step skills are editable in the Configuration view.

Out of scope: a scripting language inside workflows (section 3), harness-native subagents (a step is one session), scheduling a workflow from a loop pattern (section 15), and importing Claude Code's JavaScript workflows (a Claude Code skill can call `agent-mux workflow run` instead).

## 2. Vocabulary

| Term | Meaning |
| --- | --- |
| Workflow | A TOML document: `[workflow]`, `[args]`, `[schemas]`, `[[steps]]`. Built-in (compiled in), library (`~/.agent-mux/workflows/<name>.toml`), skill-distributed (`<package>/workflows/<name>.toml`) or dynamic (composed by the planner for one task). |
| Step | One `[[steps]]` entry: a skill or an inline prompt, a `kind`, its inputs and its result schema. |
| Session | One headless one-shot run of a harness for one step and one item, traced like any launch, with `launches.metadata.workflow_run_id`, `workflow_step`, `workflow_item`. |
| Step skill | An agent-mux skill package (`SKILL.md` + `skill.toml`) written to be a workflow step: reads `$AGENT_MUX_WORKFLOW_CONTEXT`, does one bounded thing, ends with a fenced `workflow-result` block. Names start with `wf-`. |
| Run | One execution of a workflow against one workspace: a `workflow_runs` row, a run directory, a journal. |
| Item | One element a `fanout`, `pipeline`, `tournament` or `until` step works on; sessions are per item. |
| Journal | `journal.jsonl` in the run directory: one line per session with step id, item hash, result and launch id. The unit of resume. |
| Planner | The headless session that composes a dynamic workflow, using the `workflow-author` skill. Any of the three harnesses. |

## 3. Design decision and alternatives

Chosen: **a declarative workflow document interpreted in Rust, with skills as the unit of model-facing work**, executed through the existing headless launch path per harness.

| Approach | Benefit | Limitation |
| --- | --- | --- |
| TOML document + step skills + Rust interpreter (chosen) | Same shape as loops and Heimdall; no engine dependency; every part is a file the Configuration view edits and validates; determinism and resume are structural; the planner emits a schema-validated document, reviewable before it runs; works on all three harnesses through the skill install path | A fixed vocabulary: shapes outside it need a new Rust primitive or an agent step that does the transformation; no script portability with `~/.claude/workflows` |
| Embedded JavaScript engine running Claude Code's script API (previous draft) | Script portability in both directions; arbitrary control flow | A new engine dependency and an async host bridge; scripts are programs to audit; nothing else in agent-mux is shaped that way |
| A conductor session orchestrating through MCP tools | No orchestration code | The conductor is one long context, the problem the post sets out to remove; Antigravity has no per-launch MCP registration |

The six shapes of the post map onto the vocabulary one to one (section 4.3), which is the test of sufficiency. Data processing between steps is limited to what Rust implements (`dedupe_by`, `keep`, `take`, `flatten` through paths); anything richer is itself a step.

## 4. The workflow document

### 4.1 Example

```toml
[workflow]
name = "review-changes"
description = "Find issues per dimension, try to refute each one, report the survivors."
when_to_use = "Reviewing a diff or working tree for defects with independent verification."
harness = "any"            # "any" | "claude" | "codex" | "agy": the harnesses this workflow may run on
output = "report"          # the step whose result is the run's result

[args.scope]
description = "A path, a ref range, or empty for the working tree."
default = ""

[schemas.findings]
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.title = { type = "string", required = true }
fields.why = { type = "string", required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }
fields.reason = { type = "string" }

[[steps]]
id = "find"
kind = "fanout"
phase = "Review"
over = ["correctness", "security", "performance", "tests"]
skill = "wf-review-find"
args = { dimension = "{item}", scope = "{args.scope}" }
result = "findings"

[[steps]]
id = "confirmed"
kind = "pipeline"
phase = "Verify"
over = "find[*].findings[*]"
dedupe_by = ["file", "line"]
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2" }

[[steps]]
id = "report"
kind = "single"
phase = "Report"
skill = "wf-synthesize"
input = "confirmed"
```

### 4.2 Fields

`[workflow]`: `name` (`^[a-z0-9][a-z0-9_-]*$`, equals the file stem), `description`, `when_to_use`, `harness` (default `any`), `output` (a step id; default the last step), `default_isolation` (`none` | `worktree`), `budget_tokens`, `max_concurrent` (ceiling `[workflows] max_concurrent`).

`[args.<name>]`: `description`, `default`, `required`. Values come from the run dialog or `--args`; a plain string given to a workflow with one required arg fills that arg.

`[schemas.<name>]`: `fields.<field> = { type, required, items, enum }` with `type` in `string`, `integer`, `number`, `boolean`, `array`, `object`; `items` names another schema or a primitive type. This is the subset agent-mux validates; it renders a JSON Schema into the step context so the model sees the exact shape.

`[[steps]]`:

| Field | Meaning |
| --- | --- |
| `id` | Unique, `^[a-z][a-z0-9_-]*$` |
| `kind` | `single` (default), `fanout`, `pipeline`, `route`, `tournament`, `until` |
| `skill` or `prompt` | Exactly one. `skill` names a step skill; `prompt` is inline text (dynamic workflows), sent after the `[workflow] inline` preamble from `prompts.toml` |
| `args` | Table of values placed in the step context and interpolated: `{item}`, `{item.<field>}`, `{args.<name>}`, `{steps.<id>.result}` (JSON), `{index}` |
| `over` | Items for `fanout`, `pipeline`, `tournament`, `until`: an inline array, `args.<name>`, or a path into prior results |
| `input` | A step id (or path) whose result is handed to the step as `inputs` |
| `result` | Schema name the fenced block must satisfy; omitted means free text |
| `verify` | `{ skill \| prompt, votes = N, result, keep }`: after each item's session, `N` independent refuter sessions; the item survives when `keep` holds over the aggregated votes (`refuted` counts, `and`/`or`) |
| `keep` | Predicate on the item result: `<field> <op> <value>` with `== != < <= > >=`, `and`, `or`, `has(<field>)` |
| `dedupe_by` | Field names; keeps the first item per key, logs the dropped count |
| `take` | Keep the first N items after `keep` and `dedupe_by`; the drop is logged |
| `branches` | `route` only: `{ <label> = ["step-id", …] }`; the classifier's `result` must have `label`; steps not on the chosen branch are skipped (`null`) |
| `judge`, `n` | `tournament` only: the judging skill or prompt (result must have `winner = "a" \| "b"`) and, when `over` is absent, how many candidates to generate with `skill`/`prompt` |
| `rounds_without_new`, `max_rounds` | `until` only (defaults 2 and 10); requires `dedupe_by` |
| `harness`, `profile`, `model`, `effort`, `isolation`, `cwd`, `timeout_s`, `concurrency` | Per-step overrides (section 5) |
| `phase` | Progress group in the view |

Paths: `<step>` is that step's result (or collected items); `[*]` flattens an array; `.field` descends; `args.<name>` and `item` are the other roots. Paths are resolved statically at load time against declared steps and schemas, so a typo is a validation error, not a run-time surprise.

### 4.3 The six shapes

| Post | Document |
| --- | --- |
| Classify-and-act | `kind = "route"` with a classifier step and `branches` |
| Fan-out-and-synthesize | `fanout` then `single` with `input` |
| Adversarial verification | `verify = { …, votes = 3, keep = "refuted < 2" }` on any step |
| Generate-and-filter | `fanout` + `dedupe_by` + `keep` + `verify` + `take` |
| Tournament | `kind = "tournament"` with `judge`; bracket logic in Rust, pairwise judging by a session |
| Loop until done | `kind = "until"` with `rounds_without_new` and `dedupe_by` |

Quality patterns from the reference material (judge panel, multi-modal sweep, completeness critic, no silent caps) are workflows built from these, shipped as built-ins (section 8).

## 5. Execution model

### 5.1 The interpreter

`src/workflows/` (new):

| Module | Responsibility |
| --- | --- |
| `document.rs` | Parse and validate a workflow (ids, one of `skill`/`prompt`, schema and path resolution, branch targets, `until` invariants, harness compatibility with the chosen run harness) |
| `plan.rs` | Expand the document into a dependency graph of steps; compute what can run concurrently; static step-count estimate for the dialog |
| `interp.rs` | Drive the graph: resolve `over`, spawn item sessions through the App, apply `keep`/`dedupe_by`/`take`, run `verify` votes, evaluate `route`, run `tournament` brackets and `until` rounds, collect results, honour budget and caps |
| `context.rs` | The step context document (`$AGENT_MUX_WORKFLOW_CONTEXT`, `schema_version` 1, section 5.3), written under the runtime dir like loop contexts |
| `harness.rs` | Per-harness print-mode command lines and stdout envelope parsing (section 5.2) |
| `result.rs` | Final-text precedence, fenced-block extraction, schema validation, the one retry |
| `journal.rs` | `journal.jsonl` read/write and resume matching (step id + item hash + options) |
| `library.rs` | Built-in workflows, the library, skill-distributed workflows, `Kind::Workflow` for the configuration catalog |
| `planner.rs` | The dynamic-workflow planner (section 6) |
| `store.rs` | `workflow_runs`, `workflow_steps` |
| `cli.rs` | `agent-mux workflow …` |

`src/app/workflows.rs` owns the App side: the sidebar section, live runs, session scheduling through `spawn_traced_full`, post-session accounting; `src/app/workflows_view.rs` the view; `draw_workflows_*` in `src/ui.rs`.

A session is spawned by the same path as a loop run: a traced PTY in Active named `<workflow> ▸ <step>[<item>]`, attachable while it runs, with the workflow keys in `launches.metadata`. Completion is `PtyExit`; accounting waits for the store writer as the loop post-run does, then reads `RunFacts` (tokens, cost, files touched, final message).

### 5.2 One session per harness

A session is a print-mode run: nobody answers approval prompts, so the harness's own prompts are bypassed as loop runs do, and the controls are section 10's.

| | Claude Code | Codex CLI | Antigravity |
| --- | --- | --- | --- |
| Command | `claude -p <prompt> --output-format json` | `codex exec <prompt> --json` (else plain `exec`) | `agy --print <prompt> --print-timeout <s> --output-format json --sandbox --mode accept-edits` |
| Skill invocation | `/wf-…` opens the prompt | `$wf-…` in the prompt | `/wf-…` opens the prompt |
| Model | `--model` | `--model` | `--model` |
| Approvals | `--dangerously-skip-permissions` | `--yolo` | print mode, `--sandbox` |
| Budget | `--max-budget-usd` when a USD cap is set | agent-mux guard (`max_cost_usd` through the hook channel) | none native; agent-mux kills the session at the run's ceiling |
| Final text | envelope `result`, then `traces.output`, then scrollback | `traces.output` (transcript tail; `Stop.last_assistant_message`), `--output-last-message` file if probed, then scrollback | envelope, then `traces.output`, then scrollback. agy's `Stop` hook carries no message and there is no `SessionEnd`, so the envelope is primary |
| Usage | envelope, store | store | store via the agy usage database (may lag a step) |
| MCP (agent-mux tools) | per launch | per launch | installed entry only (`agent-mux mcp install agy`) |
| Path guard | per launch, fail-closed | per launch after `trace hooks install codex` | none (section 10) |

Step skills are agent-mux skill packages installed **user-level** through `skill::install` for the run's harness before the first session (manifest-checked, unchanged packages untouched), which is the one install path all three harnesses share; nothing is written into the workspace. `harness.rs` today renders `-p` for Antigravity; the executor renders `--print` and its companions, probed at agy 1.2.3 for the loop design but never implemented; section 12 lists what must be re-checked against the installed CLIs first. Stdout is captured raw into `<run dir>/sessions/<n>.out` next to the PTY's normal flow into the terminal parser, so an envelope is parsed from bytes, never from the screen.

### 5.3 The step context

Every session receives `$AGENT_MUX_WORKFLOW_CONTEXT`, a JSON file the skill reads first, mirroring `$AGENT_MUX_LOOP_CONTEXT`:

```json
{
  "schema_version": 1,
  "run": { "id": "…", "workflow": "review-changes", "harness": "codex", "workspace": "/abs/path", "started_at": "…" },
  "step": { "id": "find", "kind": "fanout", "phase": "Review", "index": 2, "of": 4 },
  "args": { "scope": "" },
  "item": "security",
  "inputs": { "confirmed": [ … ] },
  "result_schema": { "type": "object", "properties": { … }, "required": [ … ] },
  "budget": { "tokens_total": 400000, "tokens_spent": 61234, "mode": "normal" },
  "worktree": null,
  "gate": { "denylist": [ … ], "max_files": 5 },
  "previous_rounds": { "seen": [ … ] }
}
```

The opening prompt is `[workflow] run` from `prompts.toml` (`{invocation} …`, editable like the loop prompt); an inline `prompt` step is sent after `[workflow] inline`, which names the context file and the fenced-block requirement. Both are new keys of the existing prompts file.

### 5.4 Structured output

`result` is implemented once, above the harness: the session must end with a fenced block

````text
```workflow-result
{ … JSON matching result_schema … }
```
````

`result.rs` extracts the last such block, validates it, and on failure re-launches the session once with the validation errors appended. A second failure yields `null` for that item, journaled with the reason; the view shows the raw text. The precedent is the loop skills' `loop-result` block. Native structured-output flags (Claude Code's `--json-schema`, if probed) are used in addition, never instead.

### 5.5 Isolation

`isolation = "worktree"` on a step (or `default_isolation` on the workflow or in config) calls `loops::worktree::create` with branch `wf/<run_id>/<step>[-<item>]`, seeds the untracked skill files as loop runs do, runs the session there, and records `changed_files` and `diff_stat` in the journal. A worktree without changes is removed at once; one with changes is kept and listed in the view's **Changes** tab with `a` (keep the branch, remove the worktree) and `x` (remove both), the loop inbox semantics; agent-mux never merges. `pipeline` and `fanout` steps that edit files must be isolated; the validator refuses a non-isolated editing step whose `skill.toml` declares `writes = true` (a new, optional key on step skills), and the planner is told the rule.

### 5.6 Budget, concurrency and time

- `budget_tokens` (workflow, dialog or `--budget`) is a hard ceiling: `tokens_spent` sums the run's launches from the store plus live `TraceStats`; when reached, no new session starts, running ones finish, the run ends `budget-exhausted` with partial results. A USD cap per run maps to `--max-budget-usd` on Claude Code and to the hook guard elsewhere.
- Concurrency: `[workflows] max_concurrent` (default `min(4, cpus - 2)`, ceiling 16) across all runs in one App queue; loop runs and workflow sessions share the accounting, loops first when both are due.
- Time: `[workflows] session_timeout_s` (default 900) per session, `run_timeout_s` (default 7200) per run. A timed-out session yields `null`; a timed-out run is cancelled.
- Counts: `[workflows] max_sessions` (default 1000) per run; `until` is bounded by `max_rounds`; `tournament` by `n`.

### 5.7 Cancel and resume

`x` on a live run (or `workflow cancel`) kills its sessions, marks the run `cancelled`, keeps the journal. `r` on a finished, failed, cancelled or budget-exhausted run re-runs it with resume: the interpreter replays journaled results for every (step, item, options) it meets again in order and launches from the first miss. Because the document has no clock or randomness, replay is exact; `run.started_at` is journaled.

### 5.8 What a run returns

The `output` step's result (or collected items) is the run's result: `workflow_runs.result`, the view's **Result** tab, `workflow run` stdout, `result.json`. A run that fails stores the error and the step and item it stopped at.

## 6. Dynamic workflows: the planner

"Custom-built for the task at hand" is a two-session affair:

1. `n` in the Workflows section; the user types the task.
2. agent-mux launches the **planner**: a headless session of the run's harness with the `workflow-author` skill (compiled-in package, installed user-level per harness before launch), the task, and a context file: workspace inventory computed in Rust (top-level layout, languages, test and lint command guesses, git status summary), the catalog of step skills with their descriptions and result schemas, the built-in workflows as examples, the available profiles and harnesses, and the budget. The skill carries the document reference (section 4), the six shapes, the quality patterns, and the rule from the post that "regular coding tasks do not need a panel of 5 reviewers". It must answer with one fenced `workflow-toml` block. It may use inline `prompt` steps and inline `[schemas]`; it may not invent step skills (those are curated; the user creates one with `n` in the Configuration view).
3. agent-mux validates the document exactly as a library file (section 5.1, `document.rs`) and shows it: `[Enter] run  [e] edit  [s] save to library  [Esc] discard`. `[workflows] dynamic_approval = "always" | "never"` decides whether `Enter` is required.
4. The run proceeds as in section 5; the document is stored with the run, so `s` works afterwards too.

The planner is one session on any harness; its tokens count against the run's budget.

## 7. User experience

### 7.1 The sidebar section

`SidebarSection` gains `Workflows` between Loops and History; `Tab` cycles Active → Agents → Loops → Workflows → History. `sidebar_areas` splits the height five ways with the same shrink order (Workflows shrinks after Loops). Rows are the workflows the catalog knows, built-in first, then library, then skill-distributed, each with a glyph: `▶` live (`n/m` sessions), `✓` last run finished, `!` last run failed or has changes awaiting a decision, `‖` paused by the kill switch. The main pane shows the **preview card**: `when_to_use`, the steps by phase with their kinds, args, and the last run (when, harness, sessions, tokens, cost, outcome, result summary).

```text
┌ Workflows ─────────────┐   ┌ review-changes · last run 12 min ago ────────────────────────────┐
│ > review-changes  ▶ 3/7│   │ Find issues per dimension, refute each one, report the survivors. │
│   research           ✓ │   │ Review   find (fanout ×4)                                          │
│   understand           │   │ Verify   confirmed (pipeline, verify ×3)                           │
│   audit-until-dry    ! │   │ Report   report (single)                                           │
│   my-migration         │   │ Last run codex · 7 sessions · 412k tokens · $1.84 · finished        │
└────────────────────────┘   └───────────────────────────────────────────────────────────────────┘
 [Enter] run / view  [n] compose for a task  [e] edit  [x] cancel  [W] view  [K] kill  [?] help
```

Keys: `Enter` opens the run dialog (or the view when a run is live), `n` new dynamic workflow, `e` edit the document in the editor (library copy created for a built-in, as the Configuration view does), `x` cancel the live run, `W` the view from anywhere, `K` the kill switch also stops workflow scheduling.

### 7.2 The run dialog

Same shape as the loop dialog: **Workspace** (shared directory picker), **Harness / Profile** (every harness the workflow allows; the profile list is not filtered to two harnesses), one field per declared **arg** (with its description and default), **Budget** (tokens), **USD cap**, **Isolation default**, **Approval** (dynamic runs). The dialog shows the static session estimate (`4 + 3×k + 1` for the example, with `k` unknown until `find` returns). `Enter` validates and starts.

### 7.3 The Workflows view (`W`)

A modal like the Loops view: a left list of runs (live first, then recent), tabs on the right:

| Tab | Content |
| --- | --- |
| Progress | Phase tree: one group per phase, one line per session with step, item, harness, state (queued, running, done, null, failed), tokens, cost, wall time; interpreter notes as narrator lines (items dropped by `dedupe_by`/`take`, a branch skipped, a round without new items); `Enter` attaches to a running session, `T` opens its traces |
| Document | The TOML with validation notes; for a dynamic run, the task and the planner's session; `e` edits (applies to the next run), `s` saves to the library |
| Result | The run's result pretty-printed, or the error and where it stopped |
| Journal | One row per session: step, item hash prefix, options, result kind, launch id; `r` resumes after the selection |
| Changes | Worktrees with changes: branch, path, diff stat, `a`/`x` |

### 7.4 The Configuration view and the library

`Kind::Workflow` joins the catalog: built-in documents are `workflows/<name>.toml` items with source `built-in`, library ones `user`; `Enter`/`R`/`n` apply, validation is `document.rs`. Step skills are skill packages, so they are listed under Skills as today, and `n` there creates one from a skeleton whose body already reads the context file and ends with the fenced block. Skills may distribute workflows: a package's `workflows/*.toml` are listed as `<skill>/<name>`.

## 8. Built-in workflows and step skills

Compiled in under `workflows/` in the repository (documents) and `workflows/skills/wf-*/` (step skills), editable through the library:

| Workflow | Shape | Step skills |
| --- | --- | --- |
| `review-changes` | fanout → pipeline with `verify` → single | `wf-review-find`, `wf-refute`, `wf-synthesize` |
| `understand` | fanout over top-level directories → single | `wf-read-map`, `wf-synthesize` |
| `research` | fanout over search modes → pipeline deep-read → single → single critic | `wf-search`, `wf-deep-read`, `wf-synthesize`, `wf-critic` |
| `audit-until-dry` | `until` finders with `dedupe_by` → pipeline with three-lens `verify` | `wf-find`, `wf-refute` |
| `judge-panel` | `tournament` with `n` attempts → single synthesis | `wf-attempt`, `wf-judge`, `wf-synthesize` |
| `migrate` | single discover → pipeline transform in worktrees → pipeline verify | `wf-discover-sites`, `wf-transform` (`writes = true`), `wf-verify-site` |
| `triage-route` | `route` on a classifier → per-branch steps | `wf-classify`, `wf-triage-bug`, `wf-triage-feature` |

Every step skill: reads the context first; refuses to run without it; makes its calls within a stated budget; never edits outside a worktree unless `writes = true`; ends with the fenced block and nothing after it. The `workflow-author` skill's `reference/` copies the documents as examples.

## 9. Configuration, store and files

`profiles.toml`:

```toml
[workflows]
enabled = true
max_concurrent = 4          # sessions at a time, across runs (ceiling 16)
session_timeout_s = 900
run_timeout_s = 7200
max_sessions = 1000
default_isolation = "none"  # "none" | "worktree"
dynamic_approval = "always" # "always" | "never"
default_budget_tokens = 0   # 0 = none
```

`prompts.toml` gains `[workflow] run` and `[workflow] inline`.

Store (schema `user_version` + 1):

| Table | Columns |
| --- | --- |
| `workflow_runs` | `id`, `workflow`, `source` (`builtin`/`library`/`dynamic`/`skill:<id>`), `document_hash`, `document` (text), `workspace`, `harness`, `profile`, `args` (JSON), `budget_tokens`, `started_ns`, `ended_ns`, `status` (`running`/`finished`/`failed`/`cancelled`/`budget-exhausted`), `sessions`, `tokens`, `cost_usd`, `result` (JSON), `error`, `resumed_from` |
| `workflow_steps` | `run_id`, `step`, `item_hash`, `item` (JSON), `launch_id`, `phase`, `harness`, `kind` (`text`/`object`/`null`), `started_ns`, `ended_ns`, `tokens`, `cost_usd`, `worktree`, `changed_files` (JSON), `votes` (JSON) |

`launches.metadata` gains `workflow_run_id`, `workflow_step`, `workflow_item`; `TraceService` gets `Request::WorkflowRun { run_id }`; the MCP server a tenth read tool, `agent_mux_get_workflow_run`, so a step (or Heimdall) can read the run's progress and prior results.

Files: `<runtime dir>/workflows/<run_id>/` with `workflow.toml`, `args.json`, `journal.jsonl`, `contexts/<n>.json`, `sessions/<n>.out`, `result.json`; swept after 7 days except runs with undecided changes. Library: `~/.agent-mux/workflows/<name>.toml`.

## 10. Safety and controls

- **Isolation.** Editing steps run in worktrees; agent-mux never merges, pushes or deletes a branch with changes without `x`.
- **The path guard.** Claude Code and Codex sessions get the per-launch `PreToolUse` guard with the workspace's `gate.yaml` denylist when present, plus the no-push rule. Antigravity has no registrable selective guard (the loop design's finding stands): an Antigravity session relies on `--sandbox`, isolation, budget and timeout; the view marks it `unguarded` and the planner keeps Antigravity steps read-only unless isolated.
- **Budget and caps.** Token ceiling, USD cap, session count, concurrency, timeouts, `max_rounds`, `n`.
- **Validation before running.** A document runs only after `document.rs` accepts it: unknown skills, unresolvable paths, missing schemas, cycles in `branches`, an editing step without isolation, a harness the workflow forbids.
- **The kill switch.** `K` pauses workflow scheduling with loops; running sessions finish.
- **Provenance.** A dynamic document is shown before it runs unless `dynamic_approval = "never"`; the task, the planner's session and the document are stored with the run.
- **No network installs.** Workflows are text; sessions are the user's own harness CLIs; nothing from another vendor is fetched or executed.

## 11. Command line and MCP

```sh
agent-mux workflow ls [--json]                          workflows with their last run
agent-mux workflow show <name>                          the document
agent-mux workflow run <name> --workspace DIR [--harness claude|codex|agy] [--profile P]
        [--arg name=value …] [--budget N] [--max-cost USD] [--isolation none|worktree]
        [--resume RUN_ID] [--json]                      exit 0 finished, 1 failed, 2 cancelled,
                                                        3 budget exhausted
agent-mux workflow plan "<task>" --workspace DIR [--harness H] [--run] [--save NAME]
agent-mux workflow runs [--json]
agent-mux workflow status <run_id> [--json]
agent-mux workflow cancel <run_id>
agent-mux workflow save <run_id> <name>
agent-mux workflow check [<name>]                       validate every document and step skill
agent-mux workflow skills                               step skills and where they are installed
```

`workflow run` and `plan` drive a private `App` as `loop run --now` does. MCP: `agent_mux_get_workflow_run`. `trace doctor` gains a `workflows` section: library path, step skills installed per harness, runs today, undecided changes.

## 12. Harness probe table

Every row is verified against the installed CLI (`--help` and one real run) before the executor is written; the module docs record the outcome, as `src/harness.rs` does.

| Question | Claude Code | Codex CLI | Antigravity |
| --- | --- | --- | --- |
| Print mode flag and prompt position | `-p <prompt>` (known) | `exec <prompt>` (known) | `--print <prompt>` (probed 1.2.3, unimplemented) |
| JSON envelope on stdout; final-text field; usage fields | `--output-format json`, `result`, `total_cost_usd`? | `--json` events? `--output-last-message FILE`? | `--output-format json`; fields? |
| Native structured output | `--json-schema`? | none known | none known |
| Model override honoured in print mode | `--model` | `--model` | `--model` with `--print`? |
| Effort flag | none known | `-c model_reasoning_effort=`? | none known |
| Working directory flag | cwd only | `-C DIR`? | cwd only |
| Timeout | none (agent-mux) | none (agent-mux) | `--print-timeout` |
| Sandbox / mode | guard | `--sandbox`? / `--yolo` | `--sandbox`, `--mode accept-edits\|plan` |
| Skill invocation honoured in print mode | `/name` (known for loops) | `$name` (known for loops) | `/name` with `--print`? |
| Exit codes on model error and budget stop | ? | ? | ? |
| A print run writes a transcript the tailer finds | yes | yes | verify |

Any "no" in the envelope row falls back to `traces.output` and the scrollback; the design does not depend on a "yes".

## 13. Testing

- `document.rs`: parse and validation cases (every error class), path resolution, schema rendering.
- `interp.rs` with a fake session runner: `fanout`, `pipeline` ordering without barriers, `route`, `tournament` brackets, `until` termination, `verify` majorities, `dedupe_by`/`keep`/`take`, budget ceiling, caps, cancel, resume from a journal.
- `tests/workflow_runs.rs`: fake `claude`, `codex` and `agy` scripts that print an envelope, a fenced block or nothing; final-text precedence, schema retry, timeout, mixed harnesses, worktree isolation with and without changes, step-skill install per harness.
- `tests/workflow_ui.rs`: the section, Tab order, five-section heights, the run dialog with declared args, the view's tabs, `n` with a fake planner, `s`, `e`.
- `tests/workflow_cli.rs`: every command through the built binary against a temporary home.
- Built-ins: a validation test over every document and step skill, and an end-to-end run of `review-changes` per fake harness.

## 14. Delivery

| Delivery | Content | Gate |
| --- | --- | --- |
| A | Document model and validator, interpreter, per-harness command lines after the probe table, context, journal, store, `workflow run|ls|show|check|skills`, built-ins, step-skill install | `review-changes` runs end to end on all three fake harnesses; one real run per harness recorded in the module docs |
| B | Sidebar section, run dialog, view, Active integration, kill switch, configuration catalog kind | UI tests; docs |
| C | Planner (`workflow-author` skill), `n`, `plan`, approval, `s` | a dynamic run on each harness |
| D | Isolation with Changes tab, skill-distributed workflows, MCP tool, doctor section | tests |

Later, out of this spec: a loop pattern that names a workflow instead of a triage skill (the post's `/loop` pairing), a `goal` field that repeats a critic step until it reports nothing missing (the `/goal` pairing), new primitives as real workflows need them, and Heimdall playbooks over `workflow_runs`.

## 15. Open questions

1. **Antigravity envelope.** If `agy --print --output-format json` does not carry the final text at 1.2.x, agy sessions read `traces.output` from the transcript; `result` still works because the fenced block is in the transcript. To confirm in the probe.
2. **Sessions in Active.** A run with many concurrent sessions adds many rows. Proposal: one collapsible row per run (`▸ review-changes (3 running)`), expanded with `→`; to decide in delivery B.
3. **Predicate language.** `keep` is deliberately tiny (comparisons, `and`/`or`, `has`). If the first real workflows need more, the answer is a transform step skill, not a bigger language; to revisit after delivery C.
4. **Where `tokens_spent` lags.** Antigravity token counts arrive through its usage database; a fast fan-out may overshoot a tight budget by a session. Documented as "enforced before the next session starts".
