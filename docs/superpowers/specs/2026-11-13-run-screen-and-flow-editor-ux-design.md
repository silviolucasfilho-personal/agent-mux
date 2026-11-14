# Flows: a Run screen in the swarm's layout, and a clearer flow editor — design

Status: implemented, 2026-11-14 (branch `spec/run-screen-ux`). Section 8
lists where the code differs from this text. Skills and personas (§6) are
not designed yet.

Operator decisions (2026-11-13):
- **Order:** flows first. Skills and personas follow in a second round (§6).
- **Placement:** the new Run screen replaces the detail of flow rows in the
  runs view. The runs view keeps its list.
- **Both sides:** the complexity is in following a run and in editing a
  flow, so both are in scope.
- **Model:** the Swarm screen's Agents layout
  (`2026-11-02-swarm-agents-layout-design.md`): a tree on the left, the
  selected row in the main pane, and the chosen harness and agent on every row.

## 1. What the code shows today

Read from the source on `master` (`ac3e4ed`); none of it was observed on a
live run.

### 1.1 Following a run

| # | Finding | Where |
| --- | --- | --- |
| 1 | A live flow's detail is four lines of text: its name and folder, "N session(s) so far", and a hint | `runs_view::tab_lines`, `RunRef::FlowLive` |
| 2 | Its Steps tab lists only sessions already started, as `▶ label harness` | same |
| 3 | Steps that have not started are not shown, so there is no "step 3 of 6" | `RunState.steps` holds every step's phase but is private |
| 4 | The sidebar's progress is sessions done over sessions **started**, not over the total | `LiveWorkflowRun::progress` |
| 5 | The agent, model and effort of a step, a refuter or a judge are not on the live view | `LiveWorkflowRun.agents` and `SessionSpec` have them |
| 6 | No live output: `Enter` attaches to one session | `LiveWorkflowSession.stdout` already buffers each session's output |
| 7 | Loops, flows, plans and next runs share one list and five tabs whose names change by row kind (History or Steps, Setup or Document) | `RunTab::label` |
| 8 | A second flow run view exists with four tabs of its own | `workflows_view::WorkflowsViewState`, `ViewTab` |

### 1.2 Editing a flow

| # | Finding | Where |
| --- | --- | --- |
| 9 | Two navigations at once: the editor's tabs Who · What · When · Limits · Review, and the builder's screens Steps · What passes · Review. Tabs 1 and 2 are the same screen with a different row selected | `AgentEditorState::set_tab`, `sync_flow_tab` |
| 10 | "Who" for a flow opens the flow's settings (name, arguments, budget), not who runs it | `set_tab`, `Tab::Who` → `selected = 0` |
| 11 | The chain shows a step's number, id, shape and `as <agent>`. Its harness, model and effort are fields of the right pane, seen one step at a time | `ui::flow::draw_chain`, `Field::Harness`, `Model`, `Effort` |
| 12 | Refuters and the judge, who choose their own agent, are not rows of the chain | `Field::VotersWho`, `JudgeWho` |
| 13 | When and Limits are text panels that point at fields on other tabs ("tab 1, Budget") | `ui::agent_editor::draw_flow_panel` |

## 2. The Run screen

### 2.1 Layout

A new screen (`Mode::Run`) with the Swarm screen's geometry: the tree at
`ui::SIDEBAR_WIDTH`, the main pane at `ui::PANE_ORIGIN`, and the global
status bar as its only footer.

```
┌ review-pr · running 4m ────┐┌ confirmed · each + check · step 3/5 ──────────────────────┐
│ ▾ ◆ review-pr   3/5  41k   ││  Board   Live   Timeline   Result   Document              │
│   ✓ 1 scope   claude       ││                                                           │
│   ✓ 2 find    ⇉ 4  codex   ││  # step       runs          who              state   tok  │
│   ▾ ◷ 3 confirmed  2/4     ││  1 scope      once          claude·planner   ✓ 0:42   6k  │
│       ✓ [0]  claude·skeptic││  2 find       ⇉ 4           codex            ✓ 2:10  19k  │
│       ✗ [0]/vote1  codex   ││  3 confirmed  ↓ each·2 v.   claude·skeptic   ◷ 2/4   16k  │
│       ▶ [1]  claude·skeptic││  4 rank       ★ best of 3   codex·judge      ⋯            │
│       ▶ [1]/vote1  codex   ││  5 report     once          claude·doc-writer ⋯           │
│   ⋯ 4 rank    ★ 3  codex   ││                                                           │
│   ⋯ 5 report  claude       ││  41k of 200k tokens · $0.38 of $2.00 · 2 running          │
└────────────────────────────┘└───────────────────────────────────────────────────────────┘
[F1 Commands] Run · ↵ open · x stop · r resume · e edit the flow · Esc back
```

### 2.2 The tree

| Row | Depth | Shows | Selected, the pane shows |
| --- | --- | --- | --- |
| `◆` run | 0 | the flow's name, steps done over total, tokens | the run's tabs (§2.3) |
| step | 1 | state glyph, number, id, shape (`⇉ 4`, `★ 3`), harness, sessions done over planned when it has several | the step's tabs (§2.4) |
| session | 2 | state glyph, label (`[1]`, `[1]/vote1`, `r2/judge`), harness·agent | the session's tabs (§2.4) |

- **Every step is a row from the start**, in document order; a step not yet
  reached shows `⋯`.
- **A step with one session has no child row**: the step row is the session.
- **Glyphs** are the Swarm tree's: `⋯` waiting, `▶` running, `◷` partly
  done, `✓` done, `✗` failed, `∅` answered null, `↷` replayed from the
  journal on a resume.
- **Folding** with `←` and `→`; a finished step folds, the running one
  opens.
- **A route** shows the path taken; the steps it skipped are dimmed with `–`.

### 2.3 The run row's tabs

| Tab | Shows |
| --- | --- |
| Board | the table of §2.1: one line per step with its shape, who runs it (harness·agent, model when set), state, duration, tokens; the budget line under it |
| Live | a mosaic of the sessions running now, each a tail of its output |
| Timeline | one lane per session over time, as the Swarm screen's Timeline |
| Result | the run's answer, its verdict and notes (today's Report and Result) |
| Document | the TOML the run started from |

### 2.4 A step's and a session's tabs

| Tab | Shows |
| --- | --- |
| Step | who (harness, profile, agent, model, effort), what it was asked (skill or prompt), what it received, what it answered, the null reason when it answered none |
| Live | the tail of its output while it runs; its full output after |
| Trace | the session's turns and tools, from the trace store by launch id |
| Changes | the files it changed, for a step that edits in a worktree |

### 2.5 Live output is a tail, not a terminal

Flow sessions are headless print-mode runs. They have no PTY to draw or to
type into, unlike a swarm worker. Live shows the session's events as lines
(`▸ tool`, its outcome, the agent's text).

Probed on 2026-11-13 with a prompt that runs two 4-second commands, each
CLI's output timed line by line through a pipe:

| Harness | Version | As flows run it today | Streaming alternative |
| --- | --- | --- | --- |
| Claude Code | 2.1.323 | `--output-format json`: nothing, then one envelope at the end (17.9 s) | `--output-format stream-json --verbose`: one JSON line per message and tool result as it happens; the last line is the same envelope |
| Codex | 0.167.0 | `exec -o FILE`: readable lines as it works (the message, each `exec` and its output) | `exec --json`: `item.started` / `item.completed` events, `turn.completed` with usage |
| Antigravity | 1.5.8 | `--output-format json`: nothing, then one envelope at the end (20.5 s) | `--output-format stream-json`: `step_update` events, then a `result` event with the same fields |
| OpenCode | 2.0.32 | `run --format json`: one event per line as it happens | already streaming |

So Live needs Claude and Antigravity moved to `stream-json`, with
`workflows::harness::envelope` reading the last `result` line. Codex can
stay as it is (its lines are already readable) or move to `--json`.

A side finding, outside this design: in that probe Antigravity with
`--mode accept-edits` refused the shell command ("a tool required the
"command" permission that headless mode cannot prompt for") and answered
with an empty response and status `SUCCESS`.

### 2.6 Opening it

- `Enter` on a flow run in the runs view or on a live run under a flow in
  the Agents list opens the Run screen on it.
- Starting a flow opens the Run screen on the new run.
- A finished run opens the same screen from the stored rows
  (`workflow_runs`, `workflow_steps`), with Live showing the kept output.
- `Esc` returns to where it was opened from.
- `o` opens a run picker, as the Swarm screen's: live runs first, this
  folder's first among them, then recent finished runs.

### 2.6.1 Stopping

`x` on the run row stops the run; `x` on a step or session row stops that
session only, and the run goes on as it does when a session fails. Both ask
first.

### 2.7 What is removed

- The flow arms of `runs_view::tab_lines` and the runs view's flow tabs.
  A flow row there keeps its list line; its detail pane says `↵ opens the
  run`.
- `workflows_view::WorkflowsViewState` as a screen, once the Run screen
  reads finished runs. Its report builder moves under the Result tab.

## 3. The flow editor

### 3.1 One navigation

A flow's editor has three tabs, which are the builder's screens:

| Tab | Holds |
| --- | --- |
| 1 Steps | the chain and the selected row's fields |
| 2 What passes | unchanged |
| 3 Review | unchanged, plus the limits summary |

The flow's settings row gains what When and Limits said: budget, isolation
summary, and "runs when you start it". Scheduled agents and personas keep
Who · What · When · Limits · Review.

### 3.2 The chain shows who runs each step

```
 ⚙ review-pr · claude · 200k tokens

 1 scope       once            claude · planner
 2 find        ⇉ 4 in parallel codex · gpt-5
 3 confirmed   ↓ each          claude · skeptic
     └ 2 votes                 codex · skeptic
 4 rank        ★ best of 3     codex
     └ judge                   claude · judge
 5 report      once            claude · doc-writer
```

- **Every row names its harness**, then its agent, then its model when one
  is set. A step that names no harness shows the flow's, dimmed.
- **Refuters and the judge are rows** under their step. Selecting one puts
  its fields (who, harness, model, effort) in the right pane.
- **A row that will not run** (an agent a harness cannot run, agy with a
  writing agent) shows `!` and the reason from `agents::fit::check`.

### 3.3 The same picture before and after

The editor's chain and the Run screen's tree draw a step with one function,
so a flow looks the same when you build it and when it runs.

## 4. Data

| Need | Source | Change |
| --- | --- | --- |
| Every step and its phase | `RunState.steps` | a read-only accessor |
| Sessions planned for a step | the step's items, votes and attempts | a function of the document and `RunState` |
| Who runs a session | `SessionSpec`, `LiveWorkflowRun.agents` | none |
| Live output | `LiveWorkflowSession.stdout` | Claude and Antigravity run with `stream-json`; a per-harness event decoder; the envelope from the last line |
| A finished run | `workflow_runs`, `workflow_steps` | none; no schema change |
| Trace | the trace store by `launch_id` | none |

## 5. Work packages

| # | Package | Files | Depends on |
| --- | --- | --- | --- |
| A | The run model: tree rows, step states and totals from a live run and from stored rows, with tests | `src/workflows/run_view.rs` (new), `src/workflows/interp.rs` | — |
| B | The Run screen: state, keys, drawing, opened from the runs view and the Agents list | `src/app/run_screen.rs`, `src/ui/run_screen.rs` (new), `src/app/runs_view.rs`, `src/app.rs` | A |
| C | Streaming output: `stream-json` for Claude and Antigravity, the envelope from the last line, an event decoder per harness, with the probed lines as fixtures | `src/workflows/harness.rs`, `src/workflows/live.rs` (new) | — |
| C2 | Live and Trace tabs, the run picker, stopping one session | `src/app/run_screen.rs`, `src/ui/run_screen.rs`, `src/app/workflows.rs` | B, C |
| D | The flow editor: three tabs, the chain's who column, refuter and judge rows | `src/app/agent_editor.rs`, `src/ui/agent_editor.rs`, `src/app/flow_builder.rs`, `src/ui/flow.rs` | — |
| E | Removal of the older flow views; `docs/workflows.md`, `docs/agents.md`, `AGENTS.md` | `src/app/workflows_view.rs`, docs | B |

A, C and D do not share files and can run side by side.

## 6. Second round: skills and personas

Not designed here. The direction: a skill or persona run opens the Run
screen as a tree of one row, with a card naming its harness, skill or
persona, model and tools; the Skills view's usage tabs stay.

## 7. Settled questions (2026-11-13)

1. **Live output:** probed, §2.5.
2. **Picker:** yes, `o` (§2.6).
3. **Stopping:** both the run and one session (§2.6.1).

## 8. As built (2026-11-14)

- **Tabs.** The run row has Board, Live, Timeline, Result, Document; a
  step or session row Step, Live, Trace, Changes (§2.3, §2.4).
- **Kept output.** A session's output is written to `<run>/output/` when
  it ends, so Live has it afterwards; a run from before this has none.
- **A step with several sessions** has no tail or trace of its own: its
  sessions do, and the run's Live tab shows those running.
- **Resume.** A resumed run records the journal entries it replayed
  (`<run>/replayed.json`); `run_view::from_stored` reads that and keeps
  its estimate for older runs.
- **OpenCode.** The tail keeps the first session id in the stream
  (issue 131 covers a stream that carries a second one).
- **The editor's settings row** reads `⚙ flow settings · <budget>`, the
  flow's name being in the header; the chain keeps its `passes` lines.
- **A step's own pane** keeps Votes, Keep when, Voters and Judge; the
  refuters' and judge's rows add their harness, model and effort.
- **The old flow run view** is gone (`WorkflowsViewState`); a plan's
  detail in the runs view is `workflows_view::plan_lines`, and the report
  builder is `app::run_report`.
- **Keys.** The status bar shows four registry hints, so the tree lists
  the screen's own keys under its rows.
