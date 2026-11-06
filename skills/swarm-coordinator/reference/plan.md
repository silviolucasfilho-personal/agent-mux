# How to split a goal into a swarm's tasks

The planning rules the coordinator follows before it submits
`agent_mux_swarm_plan`. The conversation around it is in `SKILL.md`.

## Split the goal

- **One task, one worker, one context window.** A task must be doable by someone who reads only its instructions, its files and the results of the tasks it depends on. Put everything else it needs in `instructions`: names, paths, the contract with neighbouring tasks.
- **Independent by default.** Tasks without `depends_on` run at the same time. Give two tasks the same files only when one depends on the other.
- **Depend only when you must.** Use `depends_on` when a task needs another's code or decision: a schema before its queries, an API before its callers. A dependency on a worktree task waits until its branch is merged.
- **Say when it is done.** `acceptance` is one line a worker can check: "`cargo test bus::` passes and a new test covers X".
- **Small enough to review, big enough to matter.** Usually 2–6 tasks, rarely more than `max_workers × 2`. Do not create tasks for "investigate", "review" or "summarize": the operator reviews.
- **Isolation.** Leave `isolation` out to use the default. Choose `worktree` for broad changes to code that others are editing too (only when `git_repo` is true), and `shared` for small, separate files.
- **Harness and agent.** Give each task a `harness` from `harnesses`, and only from them: agent-mux refuses any other. With more than one, spread the tasks across the pool by fit: `claude` for broad refactors and design, `codex` for focused code and tests, `agy` without a writing `agent` (it runs custom agents read-only). Leave `harness` out to use `default_harness`. Leave `agent` out unless a listed agent clearly suits the task.
- **Re-plan.** Tasks with `started: true` stay as they are; do not repeat them. Plan only what is still needed, following the operator's notes. Reuse a key only for the same task.

## Submit

Call `agent_mux_swarm_plan` with the plan (the `harnesses` are the context's pool, `default_harness` its first):

```json
{
  "plan": {
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
}
```

- `key`: letters, digits, `-` and `_`, unique in the plan.
- `title` and `instructions` are required. `files` are paths or globs relative to the workspace.
- The tool answers **errors** when a key repeats, a dependency is unknown or forms a cycle, a harness is not installed, an agent is unknown, or a worktree is asked for outside a git repository. Nothing changes then: fix them and call it again. It also lists **warnings**, for tasks that may run at once and touch the same files in a shared workspace.
- The accepted answer carries the plan as a table (`table`): show it to the user as it is.
- Without the MCP tools: write the same JSON (the `plan` object) to a file and run `agent-mux swarm submit-plan --file <path>`.
