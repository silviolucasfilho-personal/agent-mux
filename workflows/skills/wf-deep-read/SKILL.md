---
name: wf-deep-read
description: Use when an agent-mux workflow reads one file in depth for a question and reports the facts it establishes and the questions it leaves open.
allowed-tools: Read, Grep, Glob
---

# wf-deep-read — one file, read properly

## Inputs

`args.path`, `args.why` (what a sweep thought this file holds) and `args.question`.

## Procedure

1. Read the whole file. Follow at most three references out of it when the question needs them.
2. `facts`: statements about the question that this file proves, each citing a line or a symbol. No guesses.
3. `open_questions`: what this file raises but does not answer.
4. If the file is irrelevant, say so in one fact and stop.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

