---
name: swarm-plan
description: Use when agent-mux asks for a swarm to be planned, or the user asks to "plan a swarm", "split this goal for parallel agents" or "re-plan the swarm". Reads the planner context, splits the goal into independent tasks with files, dependencies and a done-when line, and answers with one swarm-plan block that agent-mux checks, shows to the operator and dispatches. Does not change any file.
allowed-tools: Read, Grep, Glob, Bash
---

# swarm-plan — split one goal into tasks for parallel workers

agent-mux runs a swarm: you write the plan, the operator reviews it, and agent-mux starts one worker session per task, each with only its task to go on. You do not do the work, start workers or message anyone, and you change no file.

## Setup

1. Read the file named by `$AGENT_MUX_SWARM_PLAN` before anything else. If it is unset or missing, print `no planner context` and stop. It holds:
   - `goal`: what the swarm should achieve.
   - `workspace` (layout, languages, test and lint commands, git status) and `workspace_path`, `git_repo`.
   - `harnesses` (installed CLIs) and `default_harness`.
   - `agents`: personas a task may run as.
   - `max_workers`: how many tasks run at once.
   - `default_isolation`: `shared` (one folder, file leases) or `worktree` (a branch per task).
   - `replan`, on a re-plan only: the operator's `notes` and the board as it stands.
2. Look at the code the goal touches: `ls`, the manifest, `git log --oneline -20`, and a few targeted reads or greps. Know which files each task will change before you write it. Stop at a dozen commands.

## Split the goal

- **One task, one worker, one context window.** A task must be doable by someone who reads only its instructions, its files and the results of the tasks it depends on. Put everything else it needs in `instructions`: names, paths, the contract with neighbouring tasks.
- **Independent by default.** Tasks without `depends_on` run at the same time. Give two tasks the same files only when one depends on the other.
- **Depend only when you must.** Use `depends_on` when a task needs another's code or decision: a schema before its queries, an API before its callers. A dependency on a worktree task waits until its branch is merged.
- **Say when it is done.** `acceptance` is one line a worker can check: "`cargo test bus::` passes and a new test covers X".
- **Small enough to review, big enough to matter.** Usually 2–6 tasks, rarely more than `max_workers × 2`. Do not create tasks for "investigate", "review" or "summarize": the operator reviews.
- **Isolation.** Leave `isolation` out to use the default. Choose `worktree` for broad changes to code that others are editing too (only when `git_repo` is true), and `shared` for small, separate files.
- **Harness and agent.** Leave them out unless a task clearly suits another installed harness or a listed agent.
- **Re-plan.** Tasks with `started: true` stay as they are; do not repeat them. Plan only what is still needed, following the operator's notes. Reuse a key only for the same task.

## Answer

End with exactly one fenced block and nothing after it:

````
```swarm-plan
{
  "summary": "Why this split, in one or two sentences.",
  "tasks": [
    {
      "key": "store-queries",
      "title": "Bus store: list peers across workspaces",
      "instructions": "In src/bus/store.rs, … Keep the signature of list_peers; add …",
      "acceptance": "cargo test bus::store passes; a new test covers all_workspaces",
      "files": ["src/bus/store.rs"],
      "depends_on": []
    },
    {
      "key": "mcp-flag",
      "title": "MCP: wire all_workspaces to the new query",
      "instructions": "…",
      "acceptance": "…",
      "files": ["src/mcp/server.rs"],
      "depends_on": ["store-queries"],
      "harness": "codex",
      "isolation": "worktree"
    }
  ]
}
```
````

- `key`: letters, digits, `-` and `_`, unique in the plan.
- `title` and `instructions` are required. `files` are paths or globs relative to the workspace.
- agent-mux rejects the plan if a key repeats, a dependency is unknown or forms a cycle, a harness is not installed, an agent is unknown, or a worktree is asked for outside a git repository. It warns when tasks that may run at once touch the same files in a shared workspace.
