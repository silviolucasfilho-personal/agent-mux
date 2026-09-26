# Agent-first agent-mux — design

Status: agreed direction, 2026-09-26. Supersedes the separate Loops and Workflows sections in the UI; storage stays compatible (section 6).

## 1. Why

agent-mux asks a user to learn six nouns — skills, agents, loops, loop patterns, workflows, plans — each with its own sidebar section, view, builder and keys. Underneath they are one thing: something that runs, with a persona, a job, a moment it starts and limits on what it may change. The only thing a user creates is an **agent**; everything else is an attribute of one.

## 2. The model

An agent answers four questions.

| Question | Attribute | Values |
| --- | --- | --- |
| **Who** is it? | identity | name, description, instructions, tools (`read`, `edit`, `shell`, `web`, `mcp:<server>`), model and effort per harness |
| **What** does it do? | body | a **task** (a skill or a prompt), or a **flow** of steps where each step runs another agent |
| **When** does it run? | trigger | **on demand**, or **every** interval (15m, 2h, 1d) |
| **What may it change?** | limits | follows from its tools (section 3), plus a budget, the circuit breaker and the file denylist |

Everything that exists today maps onto it:

| Today | As an agent |
| --- | --- |
| A harness profile (Claude Code, Codex, Antigravity) | a built-in agent: no instructions, on demand, the harness's own tools |
| A workflow agent (`reviewer`, `skeptic`) | an agent with a task, on demand |
| A skill package launched from the sidebar (Heimdall) | an agent whose task is that skill |
| A loop | an agent with a task and an **every** trigger, in a workspace |
| A loop pattern | an agent template |
| A workflow | an agent whose body is a flow |
| A built-in workflow | an agent template |
| The planner ("compose for a task") | **Describe an agent**: a planner session drafts one for you to review |

Skills stay a separate thing: a skill is a capability an agent uses, not an agent. Harness profiles stay as configuration behind the built-in agents.

### Rules for the first version

- **Only a task can be scheduled.** A flow runs on demand; scheduled flows come later, once scheduled tasks are proven (a flow on a timer multiplies sessions and cost).
- **Flows do not nest.** A step runs an agent whose body is a task.
- **One workspace per scheduled agent**, as loops have today.

## 3. Limits without levels

L1 / L2 / L3 are removed. What an agent may change follows from its tools:

- **Without `edit`** it reads and reports: it writes its findings (the state file, its answer) and nothing else. The guard enforces it, as it enforces L1 today.
- **With `edit`** it always works in its own git worktree, never in the user's tree. Its change lands in the inbox; the user applies or rejects it. The guard enforces the denylist and the file cap there, as it does for L2 today; nothing pushes or merges.
- **Unattended changes (L3) are gone.** Nothing changes the user's working tree without them applying it.

Kept as limits, not levels: token budget per run and per day, runs per day, the USD cap, the circuit breaker, the denylist (`gate.yaml`), the readiness audit as advice.

## 4. The UI

### Sidebar: two sections

- **Agents** — every agent, built-in and the user's, one row each: name, a badge (`⟳ 1d` scheduled, `⚙` flow), and its state (`● running`, `✓ 2h ago`, `‖ paused`, `! needs you`). A running session is a row **under its agent**, attachable as today; a flow's sessions group under its run. Plain sessions (`n` on a built-in harness agent) live under that harness's agent.
- **History** — past sessions, as today.

The Loops and Workflows sections, and the separate Active section, go.

### One agent editor

Replaces the flow builder, the loop builder and the loop dialog. Tabs follow the questions: **Who · What · When · Limits · Review**.

- **Who** — the agent form (today's new-agent form), with the built-in agents editable as the user's copy (`R` restores).
- **What** — Task (a skill or a prompt) or Flow; a flow shows today's Steps and What passes screens.
- **When** — on demand, or every interval, the workspace and the harness it runs on.
- **Limits** — the tools rule in words ("edits: works in a worktree, you apply its change"), budgets, breaker, denylist.
- **Review** — the agent in words, the checks, the files it writes; `s` saves, `r` runs.

`n` in the Agents section offers three starts: **describe it** (the planner drafts), **from a template** (the built-in agents, the old loop patterns and workflows), **blank**.

### One runs view

Replaces the Loops view and the Workflows view: every run of every agent, its report, its sessions, its result. The inbox stays the one place for what needs the user: a change to apply, a plan to review, a run that failed.

### Keys

The keymap does not change (`docs/keyboard.md`). `n` new agent, `e` edit, `r` run now, `p` pause a schedule, `d` delete, `x` stop a running session or run.

## 5. What goes away

| Removed | Replaced by |
| --- | --- |
| Levels L1 / L2 / L3, the level fields, the readiness gates on levels | the tools rule (section 3) |
| The Active, Loops and Workflows sidebar sections | Agents, with running rows under each agent |
| The Loops view and the Workflows view | the runs view |
| The loop dialog, the loop builder, the flow builder as separate screens | the agent editor |
| "Compose a workflow for a task" | "Describe an agent" |
| Loop patterns and built-in workflows as separate libraries | agent templates |

## 6. Storage: compatible first

Phase one changes what users see, not what is on disk. The Agents list and the editor read and write through adapters:

- an agent with a task and a schedule ↔ an entry in `loops.json` plus its pattern;
- an agent with a flow ↔ a workflow document in the library;
- an agent's identity ↔ `~/.agent-mux/agents/<name>.toml`;
- a built-in harness agent ↔ a profile in `profiles.toml`.

A single agent file (identity, body, trigger and limits in one TOML) comes in phase three, with a migration that reads the old files, only once the model has held up in use.

## 7. Phases

1. **This spec and the mockups** (design canvas "Agent-first agent-mux").
2. **The Agents section and the agent editor** over the existing loop and workflow runtimes; levels removed; the runs view.
3. **One agent file** and the migration of `loops.json`, patterns and workflow documents into it.
4. **Scheduled flows**, if wanted.

## 8. Open

- Whether a scheduled agent's report-only findings keep a state file in the workspace (today's loops do) or live in agent-mux's store only.
- How a template is shown when the user's copy of it exists (as with built-in workflows: "your copy of a built-in", `R` restores).
