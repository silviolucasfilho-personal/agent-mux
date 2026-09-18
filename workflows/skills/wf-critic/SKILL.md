---
name: wf-critic
description: Use when an agent-mux workflow wants a completeness critic over an answer, what modality was not run, which claim is unverified, which source was not read.
allowed-tools: Read, Grep, Glob
---

# wf-critic — what is missing

## Inputs

`inputs` is the answer produced so far; `args.question` is what it was supposed to answer.

## Procedure

1. List the claims in the answer. For each, note whether a cited file supports it.
2. Ask what search or reading would have been necessary and was not done; probe one or two of them quickly.
3. `missing`: concrete gaps, each one sentence, actionable by another session. `confidence`: how much of the question the answer settles.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

