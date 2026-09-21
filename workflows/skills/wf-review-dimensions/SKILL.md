---
name: wf-review-dimensions
description: Use when an agent-mux workflow must decide which review lenses a change deserves before fanning out reviewers. Reads the scope, lists the languages in the diff and whether anything security-sensitive changed, and answers with a dimensions array.
allowed-tools: Read, Grep, Glob, Bash
---

# wf-review-dimensions — which lenses this change needs

## Inputs

`args.scope` (a path, a ref range, or empty for the uncommitted working tree).

## Procedure

1. Establish the diff: `git diff` (plus `git diff --cached`) for an empty scope, `git diff <range>` for a range, or read the path. Nothing in scope: answer with the three fixed dimensions only.
2. List the changed files (`--name-only`) and read the hunks once.
3. Build `dimensions`, in this order:
   - always `correctness`, `concurrency and resources`, `tests and coverage`;
   - one `<language> conventions and idioms` entry per language present in the changed files, at most four, most changed files first. Languages you may name: Rust (`.rs`), TypeScript (`.ts`, `.tsx`, `.js`, `.jsx`), Python (`.py`), Go (`.go`), Java (`.java`), Kotlin (`.kt`, `.kts`), C/C++ (`.c`, `.h`, `.cc`, `.cpp`, `.hpp`), shell (`.sh`, `.bash`, `.zsh`), SQL (`.sql`). Markdown, TOML, YAML and JSON are not languages here;
   - `security` when any changed path or hunk touches one of these triggers: user or external input handling (parsers, request bodies, CLI arguments, environment), shell or process execution, SQL or query building, file paths and file system writes, authentication or authorization, secrets and credentials, network calls, or CI and workflow files (`.github/**`, `Dockerfile`, deploy scripts). Name the trigger in `reason`.
4. Do not judge the code. This step only chooses the lenses; the finders that run next do the review.

## Rules

You are one session of an agent-mux workflow. Read the file named by `$AGENT_MUX_WORKFLOW_CONTEXT` before running any command; if the variable is unset or the file is missing, print `no workflow context` and stop. Take `args`, `item`, `inputs`, `result_schema` and `budget` from it. Stay inside the workspace you were started in. Do not ask questions: there is nobody to answer. Do not edit files unless this skill says so.

## Answer

End with exactly one fenced block whose JSON matches `result_schema` from the context, and nothing after it:

```workflow-result
{{ ... }}
```
