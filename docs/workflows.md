# Workflows in agent-mux

A **workflow** runs several harness sessions, each with its own context window and one focused goal, and composes their answers: fan out over items, refute every finding with independent votes, judge attempts pairwise, keep searching until nothing new turns up. agent-mux is the runtime: a TOML document says what runs, Rust interprets it, and every session is an ordinary traced run of **Claude Code, Codex CLI or Antigravity**, chosen per run and overridable per step. The planner (`c` in the Workflows section) composes a document for a task you type.

Design: `docs/superpowers/specs/2026-09-17-workflows-design.md`. The idea follows Anthropic's "dynamic workflows" for Claude Code; here the harness is a document and step skills rather than a script, so it runs on all three CLIs.

---

## 1. First run

1. `Tab` to the **Workflows** section (between Loops and History). The eight built-in workflows are listed: `review-changes`, `understand`, `research`, `audit-until-dry`, `judge-panel`, `migrate`, `triage-route`, `santa-review`. The main pane describes the selected one: its steps by phase, its args, the last run.
2. `Enter` opens the run dialog: the workspace (the shared directory picker), the profile (one per harness the document allows), one field per declared arg, a token budget, a USD cap, the isolation default, and one **Steps** row per step: the harness that step runs on for this run (`←`/`→` or `Space`; it starts on the step's default, the document's `harness` when it names one, else the run's profile, and offers only harnesses you have a profile for). `Enter` again starts the run.
3. The run's sessions appear in **Active** under one header — `▾ ⚙ <workflow> <running>/<total>▶ #<run>` — with each step hanging off it by its own name; attach to any of them to watch it. `space` folds the run away into that single row, and the trace browser (`T`) groups the same run the same way. The section row shows `▶ done/started`.
4. `W` opens the Workflows view: the run's report, the step ledger behind it, the document and the result.
5. When the run finishes, the notice leads with what the run answered; the preview card shows the last run; the report is in the view and in `agent-mux workflow status <run>`, and the raw result in the **Result** tab and in `result.json` under the run directory.

### Reading a run in the view

`W` opens the view on the **Report** tab: what the run answered, the
evidence behind it, and what it dropped on the way. The report is built
from the document's schemas, so a new document gets one without saying
anything: a `file` (and `line`) field becomes an openable location, a
schema enum such as `severity` becomes a badge the rows group by, a
`verify` step's boolean becomes a `refuted N/M` count on each row plus a **Refuted** block
carrying each refuter's reason, and an object step that is not the output
(a critique, a classification) appears as its own block instead of being
lost.

```text
✓ finished   Review of the branch
1 finding · 1 high · 1 refuted · 7/7 answered · 1.9M tokens · $3.10 · 6m 12s
Sep 18 10:02 → 10:08 · claude · /Users/me/code/agent-mux
────────────────────────────────────────
Report
    High
Findings (1)
  HIGH     src/app/loops.rs:1572           refuted 1/3
      final_message is cut at 2000 bytes
      a long narrative loses its loop-result block
Refuted (1)
  LOW      src/ui.rs:40                    refuted 2/3
      not a bug
      refuted: the caller checks it
Notes
  · find: dropped 1 duplicate item(s)
```

A text answer is shown as its heading outline; the **Result** tab has the
text itself, wrapped and scrollable, with the position in the pane title
(`12–41/380`). A harness with no prices in `pricing.toml` reads `unpriced
(agy)`, never `$0.00`.

The **Steps** tab is the ledger: one group per step, one row per session,
with its kind, tokens and duration, and under a session that answered
`null` the reason it gave (read from the run's journal). The journal is
part of this tab; there is no separate one. While a run is going the Report
tab shows the same headline with a per-step progress line and the sessions
in flight.

| Key | In the view |
| --- | --- |
| `Tab`, `Shift+Tab` | Report → Steps → Result → Document. The tab change goes back to the top. |
| `↑` `↓` / `j` `k` | The runs list when the left pane has focus (`←`), the result when the right one does (`→`). |
| `PgUp`, `PgDn`, `Space` | Scroll by a screen, whichever pane has focus. |
| `Home` / `g`, `End` / `G` | Top and bottom. |
| Wheel | Scrolls the pane that has focus. |

### Writing a prompt in a field

The task and every argument are full text fields, because an argument is often a whole prompt: what to look for, what each site should become, what the report should say. They wrap over several rows while focused and preview one line when they are not.

| Key | In a text field |
| --- | --- |
| `Alt+Enter` | A newline. `Shift+Enter` and `Ctrl+J` work where the terminal sends them. |
| `Enter` | Starts the run. It never inserts a newline, in this dialog or any other. |
| `←` `→` `Home` `End` | Move the cursor; at the edges of the text, `↑` and `↓` move to the next field. |
| `Tab`, `Shift+Tab` | Always move to the next or previous field, wherever the cursor sits. |
| `Backspace`, `Delete`, `Ctrl+W`, `Ctrl+U` | Delete a character, a character forward, a word, the whole field. |
| `Ctrl+V` | Paste the clipboard, newlines included. |
| `Ctrl+E` | Compose the field in `$EDITOR`. agent-mux leaves the screen, your editor opens on the text, and what you save comes back into the same field. |

`Ctrl+E` is the one to reach for when the prompt is long: it is the same editor round-trip the Configuration view uses, so you get your own key bindings, undo and syntax highlighting. On the command line the shell does this job, with a quoted string or a heredoc:

```sh
agent-mux workflow plan "$(cat prompt.md)" --workspace .
agent-mux workflow run migrate --workspace . \
  --arg pattern="old_api(" \
  --arg replacement="Call new_api instead.
Keep the argument order."
```

To compose a workflow for a task instead: `c`, type the task, pick the workspace and the profile, `Enter`. The planner session runs the `workflow-author` skill and answers with a document; it appears under **Planned** in the view (`W`) with its validation, where `Enter` runs it, `e` edits it, `s` saves it into the library and `x` discards it. `[workflows] dynamic_approval = "never"` runs a valid document as soon as the planner answers.

## 2. The document

```toml
[workflow]
name = "review-changes"
description = "Find issues per dimension, refute each one with three votes, report the survivors."
when_to_use = "Reviewing a diff or the working tree with independent verification."
harness = "any"            # any | claude | codex | agy | ["claude", "codex"]
output = "report"          # the step whose result is the run's result
default_isolation = "none" # none | worktree, for steps that omit isolation
budget_tokens = 400000     # optional ceiling

[args.scope]
description = "A path, a ref range, or empty for the working tree."
default = ""               # or required = true

[schemas.findings]
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.severity = { type = "string", enum = ["high", "medium", "low"], required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }

[[steps]]
id = "find"
kind = "fanout"
phase = "Review"
over = ["correctness", "security", "concurrency and resources", "tests and coverage"]
skill = "wf-review-find"
args = { dimension = "{item}", scope = "{args.scope}" }
result = "findings"

[[steps]]
id = "confirmed"
kind = "pipeline"
phase = "Verify"
over = "find[*].findings[*]"
dedupe_by = ["file", "line", "title"]
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2" }

[[steps]]
id = "report"
skill = "wf-synthesize"
input = "confirmed"
args = { subject = "confirmed review findings", shape = "a report grouped by severity" }
```

### Steps

| Field | Meaning |
| --- | --- |
| `id` | Unique, `^[a-z][a-z0-9_-]*$` |
| `kind` | `single` (default), `fanout`, `pipeline`, `route`, `tournament`, `until` |
| `skill` or `prompt` | A step skill (an agent-mux package named `wf-…`) or inline text; `{…}` interpolates |
| `args` | Values the session reads from its context; `{item}`, `{item.field}`, `{index}`, `{args.name}`, `{step}` interpolate |
| `over` | Items for `fanout`, `pipeline`, `tournament`, `until`: an array, `args.name`, or a path into an earlier step's result |
| `input` | A path handed to the session as `inputs` |
| `result` | The schema the fenced answer must match |
| `verify` | `{ skill \| prompt, votes = N, result, keep }`: `N` independent refuter sessions per item; boolean fields are counted and `keep` decides on the counts. It may carry its own `harness`, `profile`, `model` and `effort`, so the refuters run on a different model than the step they check |
| `keep` | A predicate on each item's result: `== != < <= > >=`, `in [a, b]`, `not in [a, b]`, `and`, `or`, `not`, `has(field)`. Inside a set a bare word is a string: `severity in [high, medium]`. On a `pipeline` step it runs before the item's votes, so a dropped item costs no refuter session |
| `dedupe_by`, `take` | Keep the first item per key; keep at most N |
| `branches` | `route`: `{ label = ["step", …] }`; the classifier's result carries `label`; other branches are skipped |
| `judge` / `judge_prompt`, `n` | `tournament`: the pairwise judge and how many candidates to generate. `judge` is a skill name or a table (`{ skill \| prompt, harness, profile, model, effort }`) when the judge should run on its own model |
| `rounds_without_new`, `max_rounds` | `until`: stop after this many quiet rounds (2) or rounds (10); needs `dedupe_by` |
| `harness`, `profile`, `model`, `effort`, `isolation`, `cwd`, `timeout_s`, `concurrency`, `phase` | Per-step overrides. `harness` and `profile` pick the CLI and its configuration, `model` becomes `--model`, and `effort` becomes Codex's `model_reasoning_effort` (Claude Code and Antigravity take none, and the run notes that it was ignored). The step skills are installed for every harness the document names, so a step may switch CLI safely. |

Paths: `find` is a step's result; `[*]` flattens one level; `.field` descends; `args.name`, `item`, `index` are the other roots. They are checked when the document loads.

### Which agent and model runs each step

Every session of a run can be placed on its own CLI and model. A step says
it for itself, a `verify` block for its refuters, and a `judge` for its
judges; anything unset falls back to the step's, then to the run's.

```toml
[[steps]]
id = "find"
kind = "fanout"
over = ["correctness", "security"]
harness = "codex"            # this step runs on Codex
model = "gpt-5-mini"         # on a cheap model: it only has to notice things
effort = "low"               # Codex reads this as model_reasoning_effort

[[steps]]
id = "confirmed"
kind = "pipeline"
over = "find[*].findings[*]"
# three refuters, each on a careful model, whatever the step above used
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2", model = "claude-opus-5" }

[[steps]]
id = "best"
kind = "tournament"
n = 4
skill = "wf-attempt"
judge = { skill = "wf-judge", harness = "claude", model = "claude-opus-5" }

[[steps]]
id = "report"
skill = "wf-synthesize"
input = "confirmed"
model = "claude-opus-5"      # the writing is worth the strong model
```

The Workflows section lists what each step will run on under its row, and
the Steps tab of a finished run names the harness every session actually
used. For a single run, without editing the document:

```sh
agent-mux workflow run review-changes --workspace . \
  --step find.model=gpt-5-mini --step find.harness=codex \
  --step report.model=claude-opus-5
```

A step naming a harness the document's `workflow.harness` forbids is a
load-time problem. A profile that does not exist falls back to the first
profile for that harness, and an unknown model is the harness's own error:
agent-mux passes `--model` through and does not keep a list of model names.

### Kinds and the six shapes

| Shape | Document |
| --- | --- |
| Classify-and-act | `route` with `branches` |
| Fan-out-and-synthesize | `fanout` then `single` with `input` |
| Adversarial verification | `verify = { votes = 3, keep = "refuted < 2" }` |
| Check it twice | `verify = { votes = 2, keep = "refuted == 0" }`: two independent checkers must both fail to refute an item (`santa-review`) |
| Generate-and-filter | `fanout` + `dedupe_by` + `keep` + `verify` + `take` |
| Choose the lenses first | a `single` step answers an array, the `fanout` runs `over = "<step>.<field>[*]"`; `review-changes` picks its review dimensions per change this way |
| Tournament | `tournament` with `judge`; the bracket runs in Rust, byes advance |
| Loop until done | `until` with `dedupe_by` and `rounds_without_new` |

Per-item chains (the main session, then its votes) run independently; a step waits only for the steps it references. Concurrency is `[workflows] max_concurrent` across runs, shared with loop runs.

The built-in `review-changes` is the worked example of the last two rows. Its first step runs `wf-review-dimensions`, which reads the diff and answers the lenses the change deserves: `correctness`, `concurrency and resources` and `tests and coverage` always; one `<language> conventions and idioms` lens per language in the changed files (at most four); and `security` only when a changed path or hunk touches input handling, shell or process execution, SQL, file paths, authentication, secrets, network calls or CI files. The finders fan out over that array. Before the three refutation votes, `keep = "severity in [high, medium]"` drops the low findings so no session is spent refuting them; the dropped count appears in the run's notes.

`santa-review` is the same review with the other gate: one finder lists everything, and each finding then meets two independent `wf-refute` checkers. `keep = "refuted == 0"` on the votes means a single refutation is enough to drop it, so what reaches the report has passed both.

## 3. Sessions and harnesses

Every session is a print-mode run with the harness's own approval prompts bypassed (nobody is there to answer), reading its facts from `$AGENT_MUX_WORKFLOW_CONTEXT`: the run, the step, `args`, `item`, `inputs`, `result_schema`, the budget, the worktree, the gate. A structured answer is a fenced block at the end of the final message:

````text
```workflow-result
{ "findings": [ … ] }
```
````

agent-mux validates it against the schema and retries the session once with the errors when it does not match; a second failure makes the item `null`, which the view and the notes report.

| | Claude Code | Codex CLI | Antigravity |
| --- | --- | --- | --- |
| Command | `claude -p … --output-format json` | `codex exec … -o <file> --skip-git-repo-check` | `agy -p … --output-format json --print-timeout <s> --mode accept-edits` |
| Final text | the JSON envelope, then the transcript, then the screen | the last-message file, then the transcript, then the screen | the JSON envelope, then the transcript, then the screen |
| Path guard | per launch | per launch (after `trace hooks install codex`) | none: rely on isolation and budgets |
| Worktree isolation | yes | yes | not applied |

Step skills are agent-mux packages (`wf-review-dimensions`, `wf-review-find`, `wf-refute`, `wf-synthesize`, `wf-read-map`, `wf-search`, `wf-deep-read`, `wf-critic`, `wf-find`, `wf-attempt`, `wf-judge`, `wf-discover-sites`, `wf-transform`, `wf-verify-site`, `wf-classify`, `wf-triage-bug`, `wf-triage-feature`) with `hidden = true` in their `skill.toml`, so they stay out of the Agents sidebar. They are installed user-level for the run's harness before the first session. `writes = true` marks a skill that edits files; a step running it must have `isolation = "worktree"`, which the validator enforces.

Mixed harnesses: a step's `harness = "codex"` runs it on Codex whatever the run's harness; `profile` picks a named profile. For one run, the run dialog's Steps rows and `--step <id>.harness=<h>` do the same without editing the document. Such a choice must be one `workflow.harness` allows, or the run is refused before it launches, and a `profile` the document named for another CLI is dropped, so the step launches with the chosen harness's own profile. Refuters and judges keep their own `harness` and are not changed by the step's row.

## 4. Isolation, budgets, resume

- **Worktrees.** `isolation = "worktree"` on a step (or the run dialog's default) runs the session in `git worktree` under `[loops] worktrees_dir` on branch `wf/<run>-<step>`. A worktree without changes is removed when the session ends; one with changes is kept and noted (`kept worktree … on branch …`), and the step row records the changed files. agent-mux never merges.
- **Budget.** A token ceiling from the document, the dialog or `--budget`; once spent, no new session starts and the run ends `budget-exhausted` with the partial result. A USD cap becomes `--max-budget-usd` on Claude Code and the hook guard elsewhere.
- **Timeouts.** `session_timeout_s` (900) per session, `run_timeout_s` (7200) per run; a timed-out session answers `null`.
- **Cancel.** `x` in the section or the view; running sessions are killed and settle as `null`.
- **Resume.** Every session is journaled (`journal.jsonl` in the run directory). `r` on a stored run in the view, or `--resume <run>` on the CLI, replays the journaled sessions and launches only what is new: same document, same args, same session keys.

## 5. Library and configuration

Documents live in `~/.agent-mux/workflows/<name>.toml` (`AGENT_MUX_LIBRARY_DIR` overrides the root). A file named like a built-in replaces it; a new name is added. The Configuration view (`C`) lists them under **Workflows** with their validation, `Enter` edits (a built-in is copied first), `n` creates one from a skeleton, `R` resets to the built-in. `e` in the Workflows section does the same for the selected document. A skill package may distribute documents as `workflows/<name>.toml` among its files; they are listed as `<skill>/<name>`.

```toml
[workflows]
enabled = true
max_concurrent = 4          # sessions at a time across runs (ceiling 16)
session_timeout_s = 900
run_timeout_s = 7200
max_sessions = 1000
default_isolation = "none"  # "none" | "worktree"
dynamic_approval = "always" # "always" | "never"
default_budget_tokens = 0   # 0 = none
```

`prompts.toml` holds the three prompts agent-mux composes for workflows: `[workflow] run` (a session running a step skill), `[workflow] inline` (the preamble of an inline prompt step) and `[workflow] plan` (the planner), all editable in the Configuration view.

## 6. Command line

```sh
agent-mux workflow ls [--json]                       workflows with their last run
agent-mux workflow show <name>                       the document
agent-mux workflow check [<name>]                    validate documents and step skills; exit 1 on problems
agent-mux workflow skills                            step skills and where they are installed
agent-mux workflow run <name> --workspace DIR [--harness claude|codex|agy] [--profile P]
    [--arg name=value …] [--budget N] [--max-cost USD] [--isolation none|worktree]
    [--resume RUN_ID] [--json]                       exit 0 finished, 1 failed, 2 cancelled, 3 budget
agent-mux workflow plan "<task>" --workspace DIR [--harness H] [--run] [--save NAME] [--json]
agent-mux workflow runs [--json]
agent-mux workflow status <run_id> [--json] [--result] [--steps]
                                                     the report; --result the answer alone,
                                                     --steps the session ledger with reasons
agent-mux workflow save <run_id> <name>
```

`run` and `plan` drive a private App, so cron and the TUI share the runtime. The MCP server offers `agent_mux_get_workflow_run` so a session (or Heimdall) can read a run's progress and prior results.

## 7. Files

| Location | Meaning |
| --- | --- |
| `~/.agent-mux/workflows/<name>.toml` | Library documents |
| `<runtime>/workflows/<run_id>/` | `workflow.toml`, `args.json`, `journal.jsonl`, `contexts/<n>.json`, `sessions/<n>.last.md`, `result.json` |
| `<runtime>/workflows/plans/<id>.json`, `<id>.toml` | Planner contexts and planned documents |
| `workflow_runs`, `workflow_steps` | Store tables; `launches.metadata.workflow_run_id` / `workflow_step` / `workflow_phase` on every session |
| `AGENT_MUX_WORKFLOW_CONTEXT`, `AGENT_MUX_WORKFLOW_RUN_ID`, `AGENT_MUX_WORKFLOW`, `AGENT_MUX_WORKFLOW_STEP`, `AGENT_MUX_WORKFLOW_PLAN` | Set on workflow and planner sessions |

## 8. When something trips

| Symptom | What to do |
| --- | --- |
| A step answers `null` with `no fenced workflow-result block` | The skill or prompt did not end with the block; edit it (`C`) or the document's `prompt`; the retry hint is in the session's scrollback |
| `does not allow <harness>` | The document's `harness` excludes the profile you picked |
| `edits files (writes = true) and must run with isolation` | Set `isolation = "worktree"` on that step |
| An Antigravity step in a `migrate`-style workflow | Antigravity gets no worktree and no guard; run editing steps on Claude Code or Codex (`harness = "claude"` on the step) |
| The planner answers without a document | The view shows the raw answer under Planned; `c` again with a clearer task, or write the document by hand |
| `budget-exhausted` | Raise the budget in the dialog, or `r` to resume from the journal with a higher one |
