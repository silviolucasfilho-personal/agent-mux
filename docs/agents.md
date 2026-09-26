# Agents in agent-mux

A **workflow** decides the order: which sessions run, what each one reads, what it must answer. An **agent** decides who runs a session: a persona with its own instructions, tools and model, defined once and used on Claude Code, Codex CLI or Antigravity. A step names an agent the same way it names a skill:

```toml
[[steps]]
id = "review"
kind = "fanout"
over = ["correctness", "security"]
agent = "reviewer"          # who
skill = "wf-review-find"    # what (or prompt = "…")
harness = "codex"           # where
result = "findings"
```

The skill or prompt stays the user message, as before: read the context file, do the job, answer with the `workflow-result` block. The agent's instructions become the session's system prompt. Nothing else about the run changes: order, budgets, isolation, schema checks and resume stay the workflow's.

The idea comes from agent-builder's plan (`~/workspace/agent-builder/docs/plan.md`, section 6). It is implemented here natively, in `src/agents/`, so agent-mux depends on nothing new.

---

## 1. First agent

Five agents ship built in, ready for `agent = "…"`:

| Agent | For | Tools |
| --- | --- | --- |
| `reviewer` | reviewing a change and reporting only what the code proves | read, shell |
| `skeptic` | refuting one finding; answers refuted when unsure (a natural voter) | read, shell |
| `planner` | breaking a task into ordered, verifiable steps | read, shell |
| `doc-writer` | writing docs that match the code (edits files) | read, edit, shell |
| `judge` | picking the better of two candidates, with the deciding reason | read |

Every built-in can be edited, the same way built-in workflows and loops are:
- **Where:** `Enter` on it in the Configuration view (`C`), or `e` on it in the flow builder's agent picker.
- **What changes:** an edit makes your copy in `~/.agent-mux/agents/<name>.toml`, and that copy replaces the built-in everywhere.
- **Undo:** `R` in the Configuration view restores the built-in.

To start a new agent from one:

```sh
agent-mux agent new my-reviewer --template reviewer  # writes ~/.agent-mux/agents/my-reviewer.toml
agent-mux agent show my-reviewer                     # what it becomes on each harness
agent-mux workflow check                             # every document, with its agents
```

Add `--workspace .` to `new` to write the agent into `./.agent-mux/agents/`, where it is committed with the repository and wins over a library agent of the same name. `agent-mux agent templates` lists the starting points: `blank` and every built-in agent. To create an agent in a single line:

```sh
agent-mux agent new sec --description "Security reviewer" \
  --instructions "You report a vulnerability only with the line and the input that reaches it." \
  --tools read,shell
```

While you build a flow (`f` in the Workflows section, `docs/workflows.md`), `g` on a step picks its agent or writes a new one in a form: what it is for, instructions, tools as checkboxes, a model per harness, and whether to save it to the workspace or the library. The form shows as you type whether the agent fits the step.

In the TUI: `C` opens the Configuration view. Its **Agents** group lists library agents; before the first one exists it shows `no agents yet · n creates one`. On that row or on any agent, `n` asks for a name, writes the agent from the blank template and opens it in your editor; `Enter` edits an existing one. `agent-mux config new agent <name>` does the same from the shell.

## 2. The file

```toml
name = "reviewer"                      # ^[a-z][a-z0-9_-]*$, equal to the file name
description = "Reviews a change for defects and reports only what the code proves"
instructions = """
You are a careful code reviewer. …
"""
tools = ["read", "shell"]              # optional; left out = the harness's own set
model = "claude-opus-5"                # optional; passed as --model
effort = "high"                        # optional; Codex's model_reasoning_effort, agy's --effort

[backends.codex]                       # optional, per harness: model, effort
model = "gpt-5"

[backends.agy]
model = "pro"
```

| Tool | Claude Code | Codex CLI | Antigravity |
| --- | --- | --- | --- |
| `read` | `Read`, `Glob`, `Grep` | always | not enforced |
| `edit` | `Edit`, `Write`, `NotebookEdit` | without it: `-s read-only` in place of `--yolo` | not enforced |
| `shell` | `Bash` | always has a shell (warned) | not enforced |
| `web` | `WebFetch`, `WebSearch` | not mapped (warned) | not enforced |
| `mcp:<server>` | `mcp__<server>` | configure it in `~/.codex/config.toml` (warned) | not enforced |

"Not enforced" means the session gets that harness's own tools, and `workflow check` says so. Antigravity tool names are not written into the agent file because they have not been verified, and a misspelled agy tool name can hang the session.

## 3. How a session becomes the agent

| Harness | Command line | Files |
| --- | --- | --- |
| Claude Code | `--agents '{"<name>":{"description","prompt","tools"}}' --agent <name>` | none |
| Codex CLI | `-c developer_instructions="…"`; `-s read-only` when the agent has no `edit` | none |
| Antigravity | `--agent agent-mux-<name>` | `~/.gemini/config/agents/agent-mux-<name>/agent.md`, written before the session and rewritten when the agent changes. A directory agent-mux did not write is never touched. |

These were checked against the installed CLIs on 2026-09-25. On Claude Code 2.1.282, the agent's prompt, tools and model all applied in print mode, and a `/skill` in the prompt still loaded when the tool list was restricted. On Codex 0.155.1, a probe was answered from the developer instructions. The Antigravity file shape was probed on agy 1.2.2. Two things were not re-probed: the file shape on the installed agy 1.2.10, and a Codex `-s read-only` session that has to run tools. `src/agents/launch.rs` records these checks.

**Model and effort.** Each session resolves them in this order: the step's own `model` and `effort` first (including `--step` and the run dialog), then the agent's `[backends.<harness>]` entry, then its `model`/`effort`, then the profile. The model goes through `--model` on every harness, never through the agent definition, so a step always stays in control of it.

**Roles.** A step's agent runs the step's own sessions: the main session, fan-out items and tournament candidates. Refuters (`verify`) and judges are other roles and do not inherit it. They name their own agent, or run as none:

```toml
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2", agent = "skeptic" }
judge = { skill = "wf-judge", agent = "architect", harness = "claude" }
```

For one run, `--step <id>.agent=<name>` swaps or sets the agent of a step without editing the document.

## 4. What `workflow check` enforces

For every agent a document names (use `--workspace DIR` to include that workspace's agents):

- it exists and loads: `step report: agent "ghost" not found; create it with agent-mux agent new ghost`;
- it can `read`, because every session reads its context file;
- it can `edit` on a step that edits, either through a `writes = true` skill or with `isolation = "worktree"`;
- for every harness the session may run on (the harness it names, otherwise every one `workflow.harness` allows), it lists what that harness cannot enforce, as warnings.

Errors refuse the run before anything launches, and warnings become the run's notes. `workflow check` exits 1 on errors.

## 5. Records and resume

- The journal line of each session carries `agent` and `agent_hash` (the sha256 of the agent file), and every launch row carries `launches.metadata.workflow_agent`.
- The Steps tab and `agent-mux workflow status <run> --steps` show `reviewer@codex` for a session that ran as an agent. `--json` adds `agent` per session.
- On `--resume`, if a session's agent changed since the journaled run (edited, swapped or removed), its step and every later step run again, because their inputs may change with it. The run's notes say so. Earlier steps replay.

## 6. The planner creates agents too

The planner (`c` in the Workflows section, or `agent-mux workflow plan`) reads the agents it may use from its context, with their descriptions and tools, and may put `agent = "…"` on steps, votes and judges. When a role needs a persona none of them has, the planner defines it in a fenced `agent-toml` block before the `workflow-toml` block:

````text
```agent-toml
name = "security-reviewer"
description = "Reviews a change for injection, secrets and unsafe process execution"
instructions = """…"""
tools = ["read", "shell"]
```
```workflow-toml
…
agent = "security-reviewer"
…
```
````

agent-mux checks the new agents along with the document. A new agent must load, must fit the steps that use it, and must not replace an existing agent that has other content. The new agents are written into `~/.agent-mux/agents/` when the plan runs or is saved (`s`), so the saved workflow keeps working afterwards.

## 7. Command line

```sh
agent-mux agent ls [--workspace DIR] [--json]
agent-mux agent show <name> [--workspace DIR]
agent-mux agent new <name> [--template T] [--workspace DIR]
    [--description TEXT] [--instructions TEXT] [--tools a,b] [--model M] [--effort E]
agent-mux agent check [<name>] [--workspace DIR]
agent-mux agent templates
agent-mux agent import <file> [--force]     # a Claude-shaped file as a loop agent (docs/loops.md)
agent-mux workflow check [<name>] [--workspace DIR]
agent-mux workflow run <name> --workspace DIR --step <id>.agent=<name>
```

## 8. Not yet

- **An agent's own sub-agents.** In agent-builder's plan, an agent lists `subagents`, which would join the launch (Claude's `--agents` JSON, agent files for Codex and agy). Delegation inside a step would stay model-driven, and order across steps would stay the workflow's.
- **Editing an existing agent in the flow builder's form.** The builder writes new agents; an existing one is edited in the Configuration view (`C`).
- **Enforced tool lists on Antigravity**, once agy's tool names are verified.
