---
name: wf-triage-bug
description: Use when an agent-mux workflow triages a bug report, reproduce or localize it in the workspace and report the suspect code, without fixing it.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-triage-bug — localize, do not fix

## Inputs

`args.request`.

## Procedure

1. Find the code path the report describes; run the existing test that comes closest, or a quick command that shows the behaviour.
2. Report: what you could reproduce, the suspect file and function, and the smallest fix you would try. Do not edit files.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Write the answer as plain markdown. There is no schema for this step: the whole final message is the result.
