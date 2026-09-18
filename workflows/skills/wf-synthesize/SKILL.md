---
name: wf-synthesize
description: Use when an agent-mux workflow asks for the final synthesis of earlier steps. Reads the collected results from the context and writes one report in the requested shape, without adding claims the inputs do not support.
allowed-tools: Read, Grep, Glob
---

# wf-synthesize — one report from many results

## Inputs

`inputs` holds the results of the earlier step (an array or an object). `args.subject` says what the report is about; `args.shape` says how to lay it out.

## Procedure

1. Read every input in full. Group, order and deduplicate; keep every file and line reference.
2. Write the report in the shape asked for. Lead with the conclusion. Every claim traces to an input; when inputs disagree, say so and which you trust.
3. Say what was left out: items dropped, areas nobody read, checks nobody ran.
4. Do not open new investigations; this step reads and writes only.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Write the answer as plain markdown. There is no schema for this step: the whole final message is the result.
