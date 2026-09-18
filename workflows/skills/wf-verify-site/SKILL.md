---
name: wf-verify-site
description: Use when an agent-mux workflow verifies one transformed file in its worktree, the diff is minimal, tests pass, nothing else changed. Answers refuted true when the change must not be proposed.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-verify-site — check one change

## Inputs

`inputs.item` is the change report (`path`, `changed`, `summary`); you run in the worktree that holds it.

## Procedure

1. `git diff HEAD~1 --stat` and `git diff HEAD~1`: only the named file (and its own tests) may change.
2. Run the tests that cover the file. A failure, a deleted or weakened test, or an unrelated edit means `refuted = true`.
3. Otherwise `refuted = false` with a one-sentence reason.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

