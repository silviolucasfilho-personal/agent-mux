# Skills and personas: a run screen for one session — design

Status: draft for the operator's approval, 2026-11-14. Nothing here is
implemented. Second round of
`2026-11-13-run-screen-and-flow-editor-ux-design.md` (its section 6).

Operator decisions (2026-11-14):
- **Where:** a screen of its own, as the Swarm screen's task row; not only
  a card in the Agents list.
- **When:** starting a skill or persona still lands in its terminal; the
  screen is one key away.
- **History:** a persona's past runs are listed in the runs view.
- **Skills view:** its Details and Executions tabs stay as they are.

## 1. What the code shows today

Read from the source on `spec/run-screen-ux`; none of it was observed on a
live run.

| # | Finding | Where |
| --- | --- | --- |
| 1 | A skill or persona run is one interactive session with a terminal, nested under its row in the Agents list | `agents_list::owner_of` |
| 2 | The running session's pane title is the profile, the folder and the status; it names neither the skill or persona, nor the model, nor the tools | `ui::draw_session_pane` |
| 3 | The card that says those things shows only while the cursor is on the skill's or persona's own row | `draw_persona_card`, `draw_generic_agent_preview` |
| 4 | The session's row shows model, turns, tokens and cost, or the tool running now; its sub-agents are not rows | `ui::session_detail` |
| 5 | A skill's row names the harness it runs on; a persona's row shows nothing | `ui.rs`, `AgentKind::Skill`, `AgentKind::Persona` |
| 6 | A skill's past runs are in the Skills view's Executions tab, found by `launches.metadata.skill_id` | `query::skill_launches` |
| 7 | A persona session's launch row does not record the persona (only a flow step's does, as `workflow_agent`), so a persona's past runs cannot be found in the store | `tracing::LaunchPlan` |

## 2. The screen

### 2.1 Layout

The Run screen's geometry (`Mode::Run`): the tree at `ui::SIDEBAR_WIDTH`,
the selected row in the main pane, the global status bar as the footer.
The body of the screen is a session instead of a flow.

```
┌ skeptic · working 4m ──────┐┌ skeptic · claude · opus-5 · working ───────────────────┐
│ ◇ skeptic  claude  12t 41k ││  1 Card   2 Live   3 Trace   4 Changes                  │
│   ↳ Explore        ✓       ││                                                         │
│   ↳ general-purpose ▶      ││  persona   skeptic · refutes one finding                │
│                            ││  harness   claude (Claude Code) · model opus-5          │
│                            ││  tools     read, shell                                  │
│                            ││  folder    ~/code/api                                   │
│                            ││  asked     Refute the finding in src/app/loops.rs…      │
│                            ││  so far    12 turns · 41k tokens · $0.38 · ⚙ Bash       │
│ i back to the session      ││                                                         │
└────────────────────────────┘└─────────────────────────────────────────────────────────┘
```

### 2.2 The tree

| Row | Depth | Shows | Selected, the pane shows |
| --- | --- | --- | --- |
| the session | 0 | its glyph (`⚡` a skill, `◇` a persona), the skill's or persona's name, harness, turns and tokens | the session's tabs (§2.3) |
| a sub-agent | 1 | `↳` its name and state, from the trace store by launch, as the Swarm tree's `Row::Agent` | its own rows of the trace |

### 2.3 The session row's tabs

| Tab | Shows |
| --- | --- |
| Card | who: skill or persona with its description, harness and profile, model and effort, tools (a persona's list; a skill takes the harness's own), folder; what it was asked (the task's prompt or the skill's invocation); so far: turns, tokens, cost, the tool running now |
| Live | the session's own terminal, as the Swarm screen's task row draws it; `Enter` gives it the keyboard and the Focus chord returns to the tree. The PTY is not resized for the screen |
| Trace | its turns and tools (`swarm_report::launch_trace_lines`) |
| Changes | the files its write tools touched (`loops::store::files_touched`), and `git diff --stat` of its folder when it is a git repository |

A session that has ended keeps Card, Trace and Changes; Live shows its
last screen while the session is still listed, else says it has ended.

### 2.4 Opening it

- `i` on a session row in the Agents list opens the screen on that
  session; `i` or `Esc` on the screen returns to it. `i` is unused in the
  list today (checked in `app::dispatch`).
- Starting a skill or a persona is unchanged: the session's terminal has
  the keyboard.
- `Enter` on a persona's or skill's past run in the runs view opens the
  screen from the store (§3).
- Any session opens it, also a plain harness session: its card has no
  skill or persona line.

## 3. Persona history

- A session started as a persona records it on its launch row as
  `metadata.agent` (a new key in the JSON column; no schema change), the
  way a skill's records `metadata.skill_id`.
- The runs view lists persona runs under **Running** and **Earlier** with
  the persona as the agent, read by `metadata.agent`.
- Runs from before this record are not found; nothing is guessed from
  session names.
- A skill's runs stay in the Skills view's Executions tab and are not
  added to the runs view.

## 4. The Agents list

- The running session's pane title names what it is: `skeptic · Claude
  Code — ~/code/api [working]`, with the model when the trace knows it.
- A persona's row shows the harness it is running on, as a skill's does.

## 5. Data

| Need | Source | Change |
| --- | --- | --- |
| Skill or persona of a live session | `Session.skill_id`, `Session.agent` | none |
| Model, turns, tokens, cost, running tool | `Session.trace_stats` | none |
| Sub-agents | the trace store by launch, as the Swarm screen | a shared reader |
| Trace, files touched | the trace store by launch | none |
| A persona's past runs | `launches.metadata.agent` | the new key, a query beside `skill_launches` |
| An ended run's card | the launch row and its metadata | none |

## 6. Work packages

| # | Package | Files | Depends on |
| --- | --- | --- | --- |
| A | Record the persona on the launch; the query for a persona's launches; the session's card and tree as data, with tests | `src/tracing/mod.rs`, `src/tracing/store/query.rs`, `src/app/session_view.rs` (new) | — |
| B | The screen: the session body of `Mode::Run`, its tabs, the terminal in Live, `i` | `src/app/run_screen.rs`, `src/ui/run_screen.rs`, `src/app.rs` | A |
| C | The Agents list: the pane title and the persona row | `src/ui.rs` | — |
| D | Persona runs in the runs view, opened on the screen from the store | `src/app/runs_view.rs`, `src/ui/runs.rs` | A, B |
| E | `docs/agents.md`, `docs/skills.md`, `docs/keyboard.md`, `AGENTS.md` | docs | B, D |

## 7. Open points

1. **Changes** uses the folder's `git diff --stat`, which also shows
   changes the session did not make. The files-touched list is exact; the
   diff is labelled as the folder's.
2. **Skill runs in the runs view** are left out (§3). Adding them later
   needs no new record.
