---
name: wf-judge
description: Use when an agent-mux workflow compares two proposals or results pairwise and must pick a winner with a reason.
allowed-tools: Read, Grep, Glob
---

# wf-judge — pick one of two

## Inputs

`inputs.a` and `inputs.b` are the two candidates.

## Procedure

1. Judge against the task, not against taste: correctness, blast radius, testability, clarity.
2. Verify one concrete claim of each against the workspace when it is cheap to do so.
3. `winner` is `"a"` or `"b"`; `reason` names the deciding difference in one or two sentences.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

