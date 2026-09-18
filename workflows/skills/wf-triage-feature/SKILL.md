---
name: wf-triage-feature
description: Use when an agent-mux workflow triages a feature request, locate where it would live in the workspace and outline the design and its cost.
allowed-tools: Read, Grep, Glob
---

# wf-triage-feature — where and how

## Inputs

`args.request`.

## Procedure

1. Find the modules the feature touches and the closest existing feature to mirror.
2. Report: the shape of the change (files, types, commands), the risks, the tests it needs, and a size estimate in files touched.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Write the answer as plain markdown. There is no schema for this step: the whole final message is the result.
