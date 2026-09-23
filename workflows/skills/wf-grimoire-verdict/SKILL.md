---
name: wf-grimoire-verdict
description: Use when an agent-mux workflow asks for Grimoire's Oracle verdict on reviewed findings. Reads the findings that survived refutation, applies the verdict rule (APPROVED, NEEDS_CHANGES or NEEDS_HUMAN) and writes the verdict report. Does not review the code again.
allowed-tools: Read, Grep, Glob
---

# wf-grimoire-verdict — the Oracle's verdict

## Inputs

`inputs` is the array of findings that survived three independent refuters; each has `file`, `line`, `title`, `why`, `severity`, `category`, `lens` and `fix`. It may be empty. `args.scope` is what was reviewed; `args.language` is the language to write in.

## Procedure

1. Read every finding. Sort by severity (`critical`, `high`, `medium`, then `low`) and keep at most 10.
2. Apply the rule, in this order, and do not soften it:
   - **NEEDS_CHANGES** when any finding is `critical` or `high`;
   - otherwise **NEEDS_HUMAN** when any finding's category is `architecture`;
   - otherwise **APPROVED**.
3. Write the report. Every claim traces to a finding; do not open new investigations or add findings of your own.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Write plain markdown in `args.language`. The first three lines are fixed, in English, so a script can read them:

```text
ORACLE_VERDICT: APPROVED | NEEDS_CHANGES | NEEDS_HUMAN
BLOCKING_COUNT: <number of critical and high findings>
HUMAN_REVIEW_REQUIRED: true | false
```

Then one sentence saying why, then `## Blocking findings` (the `critical` and `high` ones: file and line, why, fix), then `## Other findings` (the `medium` and `low` ones, and every `architecture` one, to note in the pull request body). Leave out an empty section. Close with a line saying that low findings were not refuted and so are not listed, and that the run's notes name any reviewer that did not answer. There is no schema for this step: the whole final message is the result.
