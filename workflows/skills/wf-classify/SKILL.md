---
name: wf-classify
description: Use when an agent-mux workflow must classify a request as a bug, a feature or a question before routing it.
allowed-tools: Read, Grep, Glob
---

# wf-classify — one label

## Inputs

`args.request`.

## Procedure

Read the request; look at the workspace only when the label depends on whether something already exists. `label` is `bug` (something behaves wrongly), `feature` (something should exist) or `question` (the author wants to know something). `reason` is one sentence.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

