# Flows, skills and personas: a Library screen and every harness everywhere — design

Status: proposal, 2026-11-14. Nothing is implemented yet; this spec is
the deliverable. Implementation starts only on the operator's `go`.

Operator decisions (2026-11-14):
- **Deliverable:** this spec first, code after approval.
- **Placement:** a dedicated screen of its own (the Swarm screen's
  model), not a section or toggle inside the Agents sidebar.
- **Harnesses:** every flow, skill and persona can be launched on every
  available harness. No item is hidden because a harness is awkward:
  the launch picker lists all of them and explains, per harness, whether
  the launch is ready, degraded or blocked, and why.

## 1. What the code shows today

Read from the source on `master`, 2026-11-14.

| # | Finding | Where |
| --- | --- | --- |
| 1 | The Agents list mixes definitions with launched work: harness profiles, then `scheduled`, `flows`, `skills`, `on demand` and `personas` headings, with sessions nested under their owner | `App::agent_lines` (`src/app/agents_list.rs`) |
| 2 | A clean list already exists as a view option: `App::clean` (toggled by `m`) shows only harnesses and their sessions. It is a toggle, not the structure | `App::toggle_agent_sections` (`src/app.rs`) |
| 3 | Skills already have a screen of their own, the Skills view (`S`): packages grouped by harness, install state, Details and Executions tabs. It is read-only except install/uninstall | `src/app/skills_view.rs` |
| 4 | Flows live in the Agents sidebar's Workflows section, a run dialog (`WorkflowDialogState`) and the Run screen (`Mode::Run`); the flow builder is a tab set inside the agent editor | `src/app/workflows_view.rs`, `src/app/workflows.rs`, `src/app/flow_builder.rs` |
| 5 | A persona is edited in the same agent editor as a scheduled agent and a flow (Who · What · When · Limits · Review) | `src/app/agent_editor.rs` |
| 6 | There are seven harnesses: Claude, Codex, Antigravity (`agy`), OpenCode, Grok, Pi, Muse. "Available" on this machine means the command resolves on `PATH` or at its installer's fallback path | `Harness::ALL` (`src/harness.rs`), `session::command_available` |
| 7 | Skills are the closest to universal: install roots exist for all seven harnesses, a package with no `harnesses` in `skill.toml` defaults to all seven, and `build_skill_launch` composes a command line for each. A package may still restrict itself with `harnesses = [...]` | `skill::install::supported_harnesses`, `skill/mod.rs`, `skill::launch::build_skill_launch` |
| 8 | Personas are applied on five harnesses. On Grok and Pi the launch plan is empty and the only trace is a warning ("agents are not applied yet; the session runs with its default instructions and tools"). On Muse the instructions are not applied via CLI flags (warning) | `agents::launch::plan`, `agents::launch::tool_notes` |
| 9 | Flows exclude Grok and Pi end to end: the fit check filters them out of a step's candidate harnesses, the print-mode command lines and envelopes are `TODO(workflows)` no-ops, and the document's own `harness = [...]` filter can narrow a flow further | `agents::fit::candidates`, `workflows::harness::{extra_args, parse_envelope}`, `workflows::document::HarnessFilter` |
| 10 | Per-harness degradation already has a vocabulary: `agents::launch::tool_notes` returns `(Level, String)` notes (warning shown, error stops the run), and `agents::fit::check` turns them into diagnostics for `workflow check` and run start | `src/agents/launch.rs`, `src/agents/fit.rs` |
| 11 | The Swarm screen is the proven pattern for "a world of its own": `Mode::Swarm` reuses the Agents geometry (sidebar at `ui::SIDEBAR_WIDTH`, main pane at `ui::PANE_ORIGIN`, the global status bar as the only footer), a tree on the left (`Row::Swarm/Coordinator/Task/Agent`), the selected row's card or terminal on the right, local verbs registered in `keymap.rs` | `src/app/swarm_board.rs`, `src/ui/swarm_board.rs`, spec `2026-11-02-swarm-agents-layout-design.md` |

The problem, in one line: definitions (flows, skills, personas) and
launched work (sessions) share one list, so the list is long, the
launched work is hard to scan, and harness support is invisible — a
skill's `harnesses`, a persona's silent no-op on Grok/Pi and a flow's
Grok/Pi exclusion are only discoverable by running into them.

## 2. The proposal in one paragraph

A new **Library screen** (`Mode::Library`) holds every definition:
flows, skills and personas, in a left tree built exactly like the Swarm
screen's. The **Agents sidebar keeps only launched work**: harness
profiles and the sessions running on them — today's `clean` list,
made permanent. Every launch from the Library goes through one
**harness picker** that lists every available harness for the selected
item with a per-harness verdict — **Ready**, **Degraded** (launch
works, named capabilities are lost) or **Blocked** (with the reason) —
and the gaps that make Grok and Pi second-class today (findings 8
and 9) are closed as part of this work, so "Blocked" is the honest
exception, never the silent default.

## 3. The Library screen

### 3.1 Layout

The Swarm screen's geometry, unchanged: the tree at
`ui::SIDEBAR_WIDTH` on the left, the selected row in the main pane,
the global status bar as the only footer. No second hint bar.

```
┌ Library ───────────────────┐┌ reviewer · persona ─────────────────────────────────────┐
│ ▾ flows              12    ││  1 Card   2 Harnesses   3 Runs                          │
│     ci-sweeper             ││                                                         │
│     release-notes    7/7   ││  who     reviewer · refutes one finding before merge    │
│ ▾ skills             34    ││  tools   read, shell                                    │
│     heimdall         7/7 ● ││  model   opus-5 (claude) · gpt-5 (codex) · …            │
│     swarm-coordinator      ││                                                         │
│ ▾ personas            9    ││  (the Harnesses tab lists every available harness       │
│     reviewer         7/7   ││  with its verdict and reason — §5.1)                    │
│     skeptic          5/7 ! ││                                                         │
│                            ││                                                         │
│ n new · / filter           ││  Enter launch · e edit · Enter picks the harness        │
└────────────────────────────┘└─────────────────────────────────────────────────────────┘
[F1 Commands] Library · ↵ launch · e edit · n new · / filter · Esc back
```

### 3.2 The tree

| Row | Depth | Shows | Selected, the pane shows |
| --- | --- | --- | --- |
| section heading (`flows`, `skills`, `personas`) | 0 | the count of items; foldable with `←`/`→` as in the Agents list | the section card (§3.4) |
| a flow | 1 | name, harness coverage `n/7`, a `●` when a run is live, `!` when any available harness is Blocked | the flow's tabs (§3.3) |
| a skill | 1 | name, coverage `n/7`, `●` when a session runs it | the skill's tabs (§3.3) |
| a persona | 1 | name, coverage `n/7`; a schedule glyph when it has `[schedule]`, `▶` when on demand | the persona's tabs (§3.3) |

- **Coverage `n/7`:** of the seven harnesses in `Harness::ALL`, how
  many can launch the item at all (Ready or Degraded, §5.1); Blocked
  and Not installed count against it, so `7/7` is the goal state and
  anything less is visible from the tree.
- **Order:** flows, skills, personas — the order the Agents list uses
  today. Inside a section: items with live work first, then by name.
- **Folding:** `←`/`→` fold and unfold, keys and fold keys as in
  `agents_list` (`section:flows`, `skill:<id>`, `persona:<name>`,
  `flow:doc:<name>` are reused, so existing fold state survives).
- **Filter:** `/` filters all three sections by name and description;
  `Esc` clears it. (The Agents list has no filter today; this is new
  and Library-only.)
- **Live sessions are not rows here.** A launched session lives in the
  Agents list (§4). The item row only carries the `●` marker, and
  `Enter` on a running item offers "jump to the session" before
  "launch another".

### 3.3 The item tabs

One tab set per kind, all sharing the **Harnesses** tab (§5):

| Kind | Tabs |
| --- | --- |
| Flow | **Overview** (description, the step list with each step's agent, skill and harness; the coverage strip) · **Harnesses** · **Runs** (this flow's runs, newest first; `Enter` opens the Run screen as today) · **Document** (the TOML, read-only; `e` opens the flow builder) |
| Skill | **Details** (the Skills view's Details tab, unchanged: source, `skill.toml`, install state per harness) · **Harnesses** · **Executions** (the Skills view's Executions tab, unchanged) |
| Persona | **Card** (who: description, instructions summary, tools, per-harness model/effort from `[backends.<harness>]`; what/when: its `[task]` pattern, skill or prompt, and `[schedule]` when present; limits) · **Harnesses** · **Runs** (the runs view's persona runs, `metadata.agent`; `Enter` opens the session run screen as today) |

The Skills view's two tabs move here verbatim; nothing about their
content changes (§7).

### 3.4 The section card

Selecting a heading shows the section's health, which is the
coverage audit the operator asked for, at a glance:

- flows: how many, how many run on every available harness, the items
  with a Blocked harness and the blocking reason, grouped.
- skills: the same, plus packages whose `skill.toml` restricts
  `harnesses` (a deliberate restriction is shown as such, not as a
  failure).
- personas: the same, plus personas whose writing tools make them
  read-only on Antigravity (finding: `tool_notes`, agy).

### 3.5 Keys

Local verbs follow `docs/keyboard.md` and are registered in
`keymap.rs` (`ActionId::Library` for opening the screen), so F1 and
Help list them.

| Key | Does |
| --- | --- |
| `L` (proposed; confirmed against the keymap audit in package C) | open the Library from the Agents screen; `Esc` returns |
| `↑↓` `j/k` | choose a row |
| `←→` | fold or unfold |
| `Enter` | launch the item: opens the harness picker (§5.2); on a heading, nothing |
| `e` | edit: the flow builder for a flow, the agent editor for a persona, the external editor for a skill's `SKILL.md` (as the Configuration view does today) |
| `n` | new: a flow (the planner, as today), a skill (scaffolded package), a persona (the agent editor) — the New-agent dialog's three paths, started from the section under the cursor |
| `d` | delete a definition (with the existing confirmations; files' fate follows each kind's current rule) |
| `/` | filter (§3.2) |
| `1`–`4` | the selected item's tab |
| `i` | on an item with a live session, jump to it in the Agents list (the session run screen stays on `i` there) |
| `Esc` | back to Agents |

No key approves, merges or deletes without the confirmations those
actions already have elsewhere.

## 4. The Agents list, cleaned

- `App::agent_lines` returns only harness profiles and their sessions:
  today's `clean` projection becomes the only projection. The
  `scheduled`, `flows`, `skills`, `on demand` and `personas` headings
  and the `Loop`, `Flow`, `Skill`, `Persona` and orphan-`Run` owner
  rows leave the Agents list.
- Sessions stay grouped by harness (today's `harness_of`), and a
  session launched from the Library keeps its identity on its row:
  the skill's or persona's name as a badge, as the pane title already
  names it (spec `2026-11-14`, §4). A flow's step sessions group under
  their run as today (`GroupKind::Workflow`); the Run screen is
  unchanged.
- `m` and `App::clean` are retired: there is nothing left to hide.
  `SidebarSection::{Loops, Workflows, Agents}` lose their Agents-list
  meaning; their actions move with their items (§7).
- Swarm sessions stay listed exactly as today (`session_listed`,
  `[agents] show_swarm`); the Swarm screen is untouched.

## 5. Every harness, for every item

### 5.1 The coverage rule

For an item and a harness, the verdict is computed from what already
exists — no new source of truth:

| Kind | Ready | Degraded (warnings) | Blocked (errors) |
| --- | --- | --- | --- |
| Skill | harness ∈ `def.harnesses`, package installed (or installed on launch, as the Agents sidebar launcher does today) | — (a skill takes the harness's own tools) | harness ∉ `def.harnesses` (the package's own restriction, quoted) |
| Persona | `tool_notes(spec, harness)` has no entries | the warnings from `tool_notes`, each shown verbatim (e.g. Codex `web` unmapped, Muse instructions advisory, a writing persona read-only on Antigravity, the persona not applied yet on Grok/Pi) | the persona does not load (its parse problems, as `fit::check` reports them). Inside a flow, a step's `fit::check` errors block that step's harness (§5.3 keeps the swarm plan check's stricter rule for writing agents on Antigravity) |
| Flow | every step's agent passes `fit::check` on that harness | any warning from `fit::check` for that harness | any error from `fit::check` for that harness; the step and its reason are named |

"Available" is `session::command_available` over `Harness::ALL`,
profiles included the way `CheckEnv::for_workspace` already counts
them. An unavailable harness is listed as **Not installed**, never
omitted: the matrix always has seven columns' worth of truth.

### 5.2 The harness picker

`Enter` on any item opens one picker, the only launch surface:

```
launch reviewer on:
  ● claude     Ready
  ● codex      Ready
  ● agy        Degraded — read-only under a custom agent
  ● opencode   Ready
  ● grok       Degraded — persona not applied yet (until package B lands)
  ● pi         Degraded — persona not applied yet (until package B lands)
  ● muse       Degraded — primary instructions not applied via CLI
  ○ (not installed harnesses are not shown here; see Harnesses tab)
```

- Every *available* harness is a row. Blocked rows are shown, greyed,
  with their reason, and cannot be picked — the operator learns why
  instead of wondering where the harness went.
- The default row is the item's own default (a skill's
  `default_harness`, a flow step's named harness, a persona's first
  `[backends]` entry), else the harness of the last launch of that
  item in this session, else Claude when available.
- Picking a harness runs the existing launch path for that kind
  (skill launch, persona launch via the New-agent machinery, the
  workflow run dialog with the step's harness preset). The workflow
  run dialog keeps its per-step harness rows; the picker sets the
  run's profile.
- After a launch, the operator lands on the new session in the Agents
  list, as today. `Esc` in the picker returns to the Library.

### 5.3 Closing the gaps (the real work)

Universal coverage is not only display. Three gaps close, each probed
against the installed CLI first, per this repo's rule, with the probe
recorded in the module docs:

1. **Personas on Grok and Pi** (finding 8). Probe how each takes
   standing instructions (Grok's agent definitions, Pi's system
   prompt / extension surface) and implement them in
   `agents::launch::plan`; until a probe passes, the harness shows
   **Degraded** with today's warning text — visible in the picker,
   not silent in a log. Muse's unapplied instructions and
   Antigravity's read-only custom agents stay Degraded with
   their current notes; they are platform facts, not bugs to hide.
2. **Flows on Grok and Pi** (finding 9). Probe each one's print mode
   (Grok `--single`, Pi `--print`) for a usable result envelope, then
   implement `workflows::harness::{extra_args, parse_envelope, live}`
   for them, remove the exclusion in `agents::fit::candidates`, and
   let a document's `harness = [...]` remain an author's restriction
   (shown as such in the matrix). Step skills already install on both
   (finding 7); the stale `TODO(workflows)` comment claiming otherwise
   is corrected. If a probe shows no parseable envelope, that harness
   stays Blocked for flows with the probe's reason recorded — and the
   spec's rule (§5.1) keeps it visible rather than filtered away.
3. **Skill restrictions become visible.** A package's
   `harnesses = [...]` stays authoritative, but the Harnesses tab and
   the section card name it as the package's own restriction.
   Packages without a restriction already default to all seven.

No change is proposed to swarm workers: `swarm_harnesses` keeps
excluding Grok and Pi until their MCP registration exists — that is a
different launch path with its own TODO, out of scope here.

## 6. Data

| Need | Source | Change |
| --- | --- | --- |
| The Library's items | `App::skills`, `App::workflow_list`, the persona/task-agent catalogs (`App::personas`, `App::task_agents`) | none; the screen is a projection, like `agent_lines` |
| Coverage verdicts | `skill::install::status`, `agents::launch::tool_notes`, `agents::fit::check`, `session::command_available` | one shared `coverage` reader (package A) so the tree, the tab and the picker never disagree |
| Live markers | `App::sessions` (`skill_id`, `agent`, workflow group) | none |
| Runs per item | the runs view's queries (`metadata.agent`, `metadata.skill_id`, workflow runs) | none |
| Last-used harness per item | in-memory on `App` | none persisted; no schema change |

## 7. What moves, what stays

| Today | Becomes |
| --- | --- |
| Agents sidebar sections (scheduled, flows, skills, on demand, personas) | the Library screen's three sections |
| Skills view (`S`) | the Library opened on the Skills section; `S` keeps working as that shortcut, the standalone `SkillsView` mode is retired once its tabs live in §3.3 |
| Workflows sidebar section + run dialog | flows in the Library; the run dialog is unchanged and opens from the picker |
| Agent editor, flow builder | unchanged; opened from the Library with `e` |
| Run screen, session run screen, runs view | unchanged; opened from the Library's Runs tabs |
| `agent-mux skill …`, `agent-mux workflow …`, `agent-mux agent …` CLIs | unchanged |
| Trace store schema | unchanged |

## 8. Work packages

| # | Package | Files | Depends on |
| --- | --- | --- | --- |
| A | The coverage reader and its tests: one verdict per (item, harness) for skills, personas and flows, from §5.1's sources | `src/app/library.rs` (new, data only), `src/agents/fit.rs`, `src/skill/mod.rs` | — |
| B | Personas on Grok and Pi: CLI probes, then `agents::launch::plan` + `tool_notes`; flows on Grok and Pi: print-mode probes, then `workflows::harness` and the `fit::candidates` exclusion removed. Each harness lands separately; a failed probe lands as a recorded Blocked reason | `src/agents/launch.rs`, `src/workflows/harness.rs`, `src/workflows/live.rs`, `src/agents/fit.rs` | — |
| C | The Library screen: `Mode::Library`, the tree, the item tabs (Skills view tabs moved in), the section cards, the harness picker, keys and keymap, F1/Help entries. Render tests at 80/120/180 columns, following the Swarm screen's tests | `src/app/library.rs`, `src/ui/library.rs` (new), `src/app.rs`, `src/keymap.rs`, `src/app/skills_view.rs` | A |
| D | The Agents list cleaned (§4): sections and owner rows removed, `clean`/`m` retired, session badges kept; existing list tests re-pointed at the Library for definition rows | `src/app/agents_list.rs`, `src/app.rs`, `src/ui.rs` | C |
| E | Docs: `docs/skills.md`, `docs/agents.md`, `docs/workflows.md`, `docs/keyboard.md`, `AGENTS.md` (the Agents list and Skills paragraphs) | docs | C, D |

B is independent of the screen and can land first: the coverage it
unlocks shows up in `workflow check` and the agent editor before the
Library exists.

## 9. Open points

1. **The `L` key** is proposed, not verified: package C starts with
   the keymap audit (`agent-mux keys`, `docs/keyboard.md`) and takes
   another free letter if `L` is taken in the Agents screen's context.
2. **A flow's picker harness vs per-step harnesses.** A flow's steps
   may name different harnesses; the picker picks the run's profile
   and the run dialog's per-step rows still override per step. The
   flow's coverage verdict (§5.1) is therefore "the run profile can
   be this harness", and the Overview tab keeps showing each step's
   own harness. If that reads as two answers to one question, the
   alternative is a per-step matrix only, with no flow-level picker —
   package C decides from the Overview mock, not by adding both.
3. **Scheduled agents** appear in the Library as personas with a
   schedule glyph (§3.2), because a scheduled agent *is* a persona
   with a `[schedule]`; its runs stay in the runs view. If the
   operator wants scheduled work as a fourth section, the tree gains
   a heading and nothing else changes.
4. **DeepSeek (`dsh`)** is not a `Harness` variant (deferred in
   `src/harness.rs`); "every harness" means the seven in
   `Harness::ALL`. Adding it is its own integration, after which it
   appears in every matrix automatically.

## 10. As built (2026-11-14)

Status: implemented the same day (packages A–E). Where the code
differs from the text above:

- **The key is `o`, not `L`** (§3.5, §9.1): `l`/`L` opens History and
  `g`/`G` are the list's top/bottom aliases, so the Library opens on
  `o` in the Agents list and on F1 → Open library
  (`ActionId::Library`).
- **The Skills view stays** (§7): its Details/Executions content is
  not duplicated in the Library; the skill's Library tabs show the
  package, its install state and coverage, and point at the Skills
  view (`S`) for executions. The Agents sidebar launcher and the
  workbench are unchanged.
- **Runs tabs direct, not embed** (§3.3): a flow's and a persona's
  Runs tab says where its runs are (the runs view, `W`); the Run
  screen and the session run screen open from there as before.
- **The Agents list** (§4) keeps one grouping beside harnesses: a
  swarm's sessions still gather under their swarm row with
  `[agents] show_swarm = true` (launched work, and the row carries
  the swarm verbs). A flow's step sessions sit under their harness,
  not under a run row; the run's tree is the Run screen's.
- **`m` and `App::clean` are gone**; `n` in the list keeps opening
  the new-agent chooser (its blank path is a new session), so session
  creation is unchanged — only the list's rows changed.
- **Grok and Pi probes** (§5.3): Grok's `--output-format json`
  envelope and `--rules` persona were verified live; Pi's
  `--mode json` event shapes were observed but its default model is
  rejected by its provider on this machine, so Pi's persona flags
  (`--append-system-prompt`, `--tools`) rest on `pi --help`, as
  recorded in `src/agents/launch.rs`.
- **A fourth section: scheduled** (§9.3's option, taken during
  implementation): legacy loops from `loops.json` are not catalog
  personas, so the Library gained a `scheduled` section (its loop
  rows, a Card tab, and the `r`/`p`/`e`/`d` verbs on the row, via
  the same selected-loop machinery). Personas with a `[schedule]`
  still appear under personas as designed.
- **Skill launches keep the SkillLauncher dialog**: the picker's
  skill pick opens it preset to the chosen harness (workspace choice
  intact), instead of spawning in the current folder. Persona picks
  open the New-session dialog preset likewise; flow picks open the
  run dialog preset.
- **`n` in the Agents list still opens the new-agent chooser**; only
  the list's rows changed. The Loops/Workflows section keys also
  still work when a section is selected, with their command
  availability now following the selection rather than a list row.
