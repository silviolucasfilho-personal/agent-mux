---
name: wf-read-map
description: Use when an agent-mux workflow maps a codebase, either lists the top-level areas worth a reader each, or reads one area and reports its purpose, entry points and dependencies.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-read-map — list the areas, or read one

## Inputs

`args.mode` is `list-areas` or `read-area`; `args.area` names the area in the second mode; `args.question` may narrow the focus.

## Procedure

`list-areas`: `ls`, the manifest (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`) and the top-level README. Answer with at most 8 areas: directories or modules a reader can cover in one session. Merge tiny ones, split a huge `src/` by its subdirectories.

`read-area`: read the area's entry files, its public surface and its tests. Report `purpose` (two sentences), `entry_points` (files or functions a newcomer starts from), `depends_on` (other areas or crates it needs) and `notes` (what a maintainer should know: invariants, gotchas, state). Grep beyond the area only to confirm a dependency.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

