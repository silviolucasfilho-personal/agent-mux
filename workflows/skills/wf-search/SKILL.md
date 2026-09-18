---
name: wf-search
description: Use when an agent-mux workflow sweeps a workspace for leads on a question through one search mode (names, symbols, history, docs). Returns at most 6 files with why each matters.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-search — one search mode

## Inputs

`args.mode` is one of: `by file and directory names`, `by symbol and identifier`, `by git history`, `by documentation and comments`. `args.question` is what we want to know.

## Procedure

Use only your mode; the other modes run in parallel and are blind to yours.

- names: `find`/`ls` for files and directories whose names relate to the question.
- symbols: `grep -rn` for identifiers, types and functions the question implies; follow one level of references.
- history: `git log --oneline -S<term>` and `git log --oneline -- <path>` for where the topic changed and why.
- docs: README, `docs/`, doc comments and inline comments that mention the topic.

Answer with at most 6 `leads`, each a `path` and a one-sentence `why`. An empty array is a valid answer.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

