---
name: wf-attempt
description: Use when an agent-mux workflow wants one independent attempt at a design task from a given angle, written as a proposal a judge can compare with others.
allowed-tools: Read, Grep, Glob
---

# wf-attempt — one approach

## Inputs

`args.task`; `args.angle` is a number: 0 optimizes for the smallest change, 1 for the lowest risk, 2 for the best user experience, 3 for the cleanest long-term structure.

## Procedure

1. Read what the task touches in the workspace.
2. Write a proposal from your angle only: the design, the files it changes, the trade-offs, the risks, how it is tested. Be specific enough that another session could implement it.
3. Do not compare with other angles; the judges do that.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Write the answer as plain markdown. There is no schema for this step: the whole final message is the result.
