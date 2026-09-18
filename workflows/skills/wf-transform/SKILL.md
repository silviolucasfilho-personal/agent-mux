---
name: wf-transform
description: Use when an agent-mux workflow applies one mechanical change to one file inside an isolated worktree, runs the relevant tests, and reports whether the file changed.
allowed-tools: Read, Grep, Glob, Bash, Edit, Write
---

# wf-transform — one file, one change

## Inputs

`args.path`, `args.pattern`, `args.replacement`; `worktree` in the context is where you are. You are in an isolated worktree on your own branch: edit freely, commit once.

## Procedure

1. Read the file. Apply the replacement to every occurrence of the pattern, adjusting imports and call sites in the same file.
2. Run the narrowest test command that covers the file (a single test module or file); fix what the change broke inside this file only.
3. `git add -A && git commit -m "migrate <path>: <pattern> -> <replacement>"`.
4. `changed` is false when the file needed nothing; say why in `summary`.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

