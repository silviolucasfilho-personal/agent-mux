---
name: wf-find
description: Use when an agent-mux workflow runs one round of discovery for a topic (injection, unchecked errors, races, leaks) and must report findings not already seen in earlier rounds.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-find — one round of discovery

## Inputs

`args.topic`; `previous_seen` lists what earlier rounds found (file, line, title). Every finding you repeat is discarded, so search where they did not.

## Procedure

1. Read `previous_seen` and decide where it is thin: directories, file types, call patterns nobody reported.
2. Search there with greps and reads that fit the topic. Vary your angle from the obvious one.
3. Report only concrete failures (inputs, then wrong result), at most 8, with file and line. An empty array is a valid answer and is how the loop learns it is done.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

