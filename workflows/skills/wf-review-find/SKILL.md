---
name: wf-review-find
description: Use when an agent-mux workflow runs a review step for one dimension (correctness, security, concurrency, tests). Reads the workflow context, inspects the scope for that dimension only, and answers with a findings block.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-review-find — one dimension of a code review

## Inputs

`args.dimension` (the lens), `args.scope` (a path, a ref range, or empty for the uncommitted working tree).

## Procedure

1. Establish the diff: `git diff` (plus `git diff --cached`) for an empty scope, `git diff <range>` for a range, or read the path. Nothing in scope: answer with an empty `findings` array.
2. Read every changed hunk with enough surrounding code to judge it. For `security`, also grep the touched files for input handling, shell, SQL, file paths and secrets. For `concurrency and resources`, look for shared state, locks, handles, timeouts. For `tests and coverage`, compare changed behaviour with the tests that cover it.
3. Report only defects you can state as a concrete failure: inputs or state, then the wrong output or crash. Style, naming and speculation are not findings. At most 8 findings; the most severe first.
4. Every finding names the file and, when you can, the line.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

