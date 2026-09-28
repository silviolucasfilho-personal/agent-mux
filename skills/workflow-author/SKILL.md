---
name: workflow-author
description: Use when agent-mux asks for an agent to be described for a task (the n menu's "describe it"), or the user asks to "compose a workflow", "plan a multi-agent run" or "write an agent for this". Reads the planner context, decides whether one agent or a flow of several fits the task, and answers with one agent-toml block or one workflow-toml block that agent-mux validates.
allowed-tools: Read, Grep, Glob, Bash
---

# workflow-author — describe an agent for one task

You write the harness for one task: either one agent that does it in a single session, when started or on a schedule, or a workflow document that agent-mux runs as separate sessions, each with its own context window and one focused goal. You do not do the task yourself.

## Setup

1. Read the file named by `$AGENT_MUX_WORKFLOW_PLAN` before anything else. If it is unset or missing, print `no planner context` and stop. It holds `task`, `workspace` (an inventory: layout, languages, test and lint commands, git status), `harness`, `skills` (every step skill you may use, with its description and result schema), `agents` (the agents a step may run as, with their description and tools), `workflows` (the built-in documents, as examples) and `budget`.
2. `reference/document.md` is the document format; `reference/patterns.md` maps task shapes to step kinds. Read both once.
3. When the inventory is not enough, look at the workspace: `ls`, the manifest, `git log --oneline -20`. Two or three commands, not an investigation.

## Choose the shape

- **One agent** when one session can do the task: a daily triage, a changelog draft, "summarize the open TODOs every morning". Most tasks are this.
- **A persona** when the user describes someone rather than something to do: "a reviewer who only reports what the code proves".
- **A flow** when the task needs several sessions: parallel reviewers, votes, a tournament, a pipeline with hand-offs. Then follow Compose below.

An agent answer is a single fenced `agent-toml` block and no `workflow-toml` block:

- `name`, `description`, `tools` from `read`, `edit`, `shell`, `web`, `mcp:<server>` (`edit` only when the agent must change files; its change then waits for the user in a worktree), and `instructions` for a persona.
- `[task]` with exactly one of `pattern` (a task from `tasks` in the context, when one fits), `skill` (a skill the agent runs) or `prompt` (the task in words, in the second person). A persona has no `[task]`.
- `[schedule]` only when the task repeats ("every morning", "daily", "hourly"): `every` (`15m`, `2h`, `1d`, at least `5m`) and `workspace` set to `workspace_path` from the context.

```agent-toml
name = "todo-digest"
description = "Summarizes the open TODOs and who owns them"
tools = ["read", "shell"]

[task]
prompt = "List the TODO and FIXME comments, group them by owner from git blame, and write a short digest."

[schedule]
every = "1d"
workspace = "/path/from/workspace_path"
```

## Compose a flow

- Prefer a built-in workflow when one fits the task; copy it and adjust `args`, `over`, phases and prompts. Compose a new one only when the shape differs.
- Use only the step skills listed in the context. A step that needs something else is an inline `prompt` step; never invent a skill name.
- Every step that hands data on has a `result` schema. Declare the schemas you need under `[schemas.*]`.
- A step whose skill declares `writes` runs with `isolation = "worktree"`. On Antigravity, keep steps read-only unless isolated.
- Match the task's size: "regular coding tasks do not need a panel of 5 reviewers". A quick check is a `single` step or a small `fanout`; a thorough audit is `until` with `verify`; a design question is a `tournament`. Say in `description` what the workflow does and in `when_to_use` when to reach for it.
- Respect `budget`: estimate sessions (items × votes) and keep the estimate under a tenth of the budget in sessions of ten thousand tokens.
- A skill or prompt says *what* a session does; an agent says *who* does it. When a listed agent's description fits a role, put `agent = "<name>"` on the step (or on `verify = { … }` or a `judge` table). Refuters and judges do not inherit the step's agent: give them their own, or none. Use agents where a persona changes the answer (a security reviewer, a skeptic); a plain step needs none.
- When a role needs a persona no listed agent has, define it: a fenced `agent-toml` block before the document, with `name`, `description`, `instructions` (the persona, in the second person) and `tools` from `read`, `edit`, `shell`, `web`, `mcp:<server>`. Every agent needs `read`; an agent on a step that edits needs `edit`. Never redefine a listed agent under its own name. Define at most three.
- Interpolate with `{args.name}`, `{item}`, `{item.field}`, `{index}` and `{step}` only; every path must name a declared arg or an earlier step.

## Answer

New agents first, one fenced block each (only when you define any):

```agent-toml
name = "security-reviewer"
description = "Reviews a change for injection, secrets and unsafe process execution"
instructions = """
You are a security reviewer. You report a vulnerability only with the line
that causes it and the input that reaches it.
"""
tools = ["read", "shell"]
```

Then end with exactly one fenced block containing the whole document and nothing after it:

```workflow-toml
[workflow]
name = "…"
description = "…"
…
```
