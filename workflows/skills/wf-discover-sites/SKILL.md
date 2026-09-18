---
name: wf-discover-sites
description: Use when an agent-mux workflow needs every site of a mechanical change found and listed by path with a one-line summary.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-discover-sites — every place to change

## Inputs

`args.pattern`: the old API, call shape or pattern.

## Procedure

1. Grep for the pattern in every form it takes (imports, calls, aliases, tests, docs).
2. One `site` per file, with a `summary` of what the file does with the pattern. Include tests.
3. Do not change anything.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

