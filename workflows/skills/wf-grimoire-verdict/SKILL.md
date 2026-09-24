---
name: wf-grimoire-verdict
description: Use when an agent-mux workflow asks for Grimoire's Oracle verdict on reviewed findings. Reads the brief, what each reviewer raised and what survived refutation, applies the verdict rule (APPROVED, NEEDS_CHANGES or NEEDS_HUMAN) and writes the verdict report with the next step. Does not review the code again.
allowed-tools: Read, Grep, Glob
---

# wf-grimoire-verdict — the Oracle's verdict

## Inputs

- `inputs`: the findings that survived three independent refuters (every `architecture` finding reaches here, whatever its severity). Each has `file`, `line`, `title`, `why`, `severity`, `category`, `lens` and `fix`. It may be empty.
- `args.brief`: the Sage's brief. Its first line is `SCOPE:` when the scope was read, `SCOPE_ERROR:` or `NOTHING_TO_REVIEW:` when it was not.
- `args.reviews`: a JSON array of the reviewers' answers, each `{ "lens": "paladin" | "cleric" | "ranger", "findings": [...] }`. A reviewer that did not answer is simply absent: check all three lenses are there.
- `args.scope`: what was reviewed. `args.language`: the language to write in.

## Procedure

1. Apply the rule, first match wins, and do not soften it:
   1. **NEEDS_HUMAN** when the brief is empty or starts with `SCOPE_ERROR:` or `NOTHING_TO_REVIEW:`. Nothing was reviewed; say why, quoting the brief's line.
   2. **NEEDS_CHANGES** when any finding in `inputs` is `critical` or `high`.
   3. **NEEDS_HUMAN** when a lens is absent from `args.reviews`: the change was not reviewed for it. Name it (security for the Paladin, bugs and edge cases for the Cleric, simplification and design for the Ranger).
   4. **NEEDS_HUMAN** when any finding's category is `architecture`.
   5. Otherwise **APPROVED**.
2. Count, from the data only:
   - `BLOCKING_COUNT`: every `critical` and `high` finding in `inputs`.
   - `HUMAN_REVIEW_REQUIRED`: `true` exactly when the verdict is NEEDS_HUMAN or any finding in `inputs` is `architecture`; `false` otherwise.
   - Raised: the findings in each answer of `args.reviews`, per reviewer (`no answer` for an absent lens). Survived: the length of `inputs`.
3. Write the report. List every blocking finding; list at most 10 of the others, most severe first. Every claim traces to a finding or the brief; do not open new investigations or add findings of your own.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

Plain markdown. The first three lines are fixed, in English, so a script and agent-mux can read them:

```text
ORACLE_VERDICT: APPROVED | NEEDS_CHANGES | NEEDS_HUMAN
BLOCKING_COUNT: <n>
HUMAN_REVIEW_REQUIRED: true | false
```

Then, in `args.language`:

1. One sentence saying why (this line becomes the run's headline).
2. One line of counts: raised per reviewer, how many survived the refuters. Findings below `medium` were set aside without a vote, and duplicates across reviewers were merged; the run's notes give those numbers, so do not invent them.
3. `## Blocking findings`: the `critical` and `high` ones, each with file and line, why, and the fix.
4. `## Other findings`: the rest, noting that they belong in the pull request body.
5. `## Next step`, one line for the verdict:
   - APPROVED: the change can merge; nothing was posted to the pull request, so post the verdict yourself if the team expects it.
   - NEEDS_CHANGES: fix the blocking findings, then run the same review again: `agent-mux workflow run grimoire-review --arg scope=<args.scope>`.
   - NEEDS_HUMAN: say what a human must decide (the scope to fix, the lens to run again, or the design question), and who is best placed to decide it.

Leave out an empty section. There is no schema for this step: the whole final message is the result.
