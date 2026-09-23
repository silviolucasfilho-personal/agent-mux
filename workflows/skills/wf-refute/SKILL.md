---
name: wf-refute
description: Use when an agent-mux workflow asks for one independent vote on a finding. Reads the finding from the context, tries to refute it against the code, and answers refuted true or false with a reason. Default to refuted when uncertain.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-refute — one skeptical vote

## Inputs

`inputs.item` is the finding (or change) to attack: its `file`, `line`, `title` and `why`. `inputs.vote` is your ballot number; other votes run independently and you must not assume their answer.

## Procedure

1. Open the file at the line. Read the surrounding function and the callers `grep` finds.
2. Try to construct the failure the finding claims. If the claimed inputs cannot occur, the path is guarded, the behaviour is intended, or the finding misreads the code, it is refuted.
3. If you can name the inputs that trigger it and the wrong result, it stands: `refuted = false`.
   A finding that claims a cost rather than a failure (its `category` is `simplification`, `performance` or `architecture`: a duplicate, a needless abstraction, a slow path) stands when the evidence it cites is in the code (the duplicate exists, grep finds no importer, the path is hot) and its fix keeps behaviour; it is refuted when the evidence is wrong or the fix would change behaviour.
4. When you cannot decide within a few minutes of reading, answer `refuted = true` and say why: an unverifiable finding must not survive a review.

Your `reason` is one or two sentences a reader can check.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```

