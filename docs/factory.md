# Software factory

The factory takes one **work item**, a GitHub issue or text you type, through a fixed line of stations and ends with a **draft pull request**. Each station is a headless session of one of your harnesses with one job. agent-mux, not a session, pushes the branch and opens the pull request. Marking it ready and merging stay with you.

This is phase 1: the line, the delivery, the brain and the `agent-mux factory` commands. The Factory screen, label intake and issue comments are phase 2 (design: `docs/superpowers/specs/2026-11-15-software-factory-design.md`).

## 1. The line

```
classify ─┬─ clarify ──▶ needs-answer (questions for the requester)
          ├─ question ─▶ answer ──────▶ answered
          └─ build ───▶ research? ─▶ analyze ─▶ implement ⇄ review ─▶ draft pull request
                                               (sent back at most twice)
```

| Station | Agent | What it does | Where it runs |
| --- | --- | --- | --- |
| classify | `classifier` | Kind, priority, complexity, and whether the item can be built, answered, or needs clarification | the workspace, read-only |
| research | `researcher` | One session per fact the plan turns on and the repository cannot hold; none for most items | the workspace, with web |
| analyze | `analyst` | The plan and the acceptance criteria the reviewer will check verbatim | the workspace, read-only |
| implement | `implementer` | Executes the plan, runs the repository's checks, commits | its own worktree |
| review | `reviewer` | Reads the committed diff, judges every criterion with evidence, answers `approve`, `request_changes` or `reject` | the same worktree, on another harness |

The line is the built-in workflow `factory` (`workflows/factory.toml`), so the order of the stations is the interpreter's, not a prompt's. It is an ordinary flow: `agent-mux workflow show factory`, and you can edit it, its step skills (`wf-factory-*`) and its agents in the Configuration view (`C`).

What the code holds to:

- **A station sees only its context**: the item, the earlier stations' typed answers, the brain. Never another session's transcript.
- **The implementer never judges its own work.** The reviewer is a separate session, on a harness other than the implementer's when you have a profile for one (Antigravity last; with a single harness the run notes that the review is not independent).
- **Work is what was committed.** A reviewer starts only when the worktree has a commit past its base and nothing uncommitted. Otherwise the implementer is asked again, once.
- **An approval is checked.** `approve` with a failing criterion, or with no criterion judged, ends `unresolved` and opens nothing.
- **Stations do not deliver.** Where a harness runs agent-mux's `PreToolUse` guard, a build session is refused `git push`, `git merge`, `git rebase` and `gh pr create|ready|merge`.
- **The item's text is data.** Every station is told so; the classifier cannot grant anything.

## 2. Running an item

```sh
agent-mux factory run "the password reset mail is sent twice; fix it"
agent-mux factory run --issue 214            # needs gh and a GitHub origin
#   --workspace DIR (default: here) · --harness claude|codex|… (default claude) · --profile NAME
#   --budget TOKENS · --step build.model=… / analyze.harness=codex (as `workflow run --step`) · --json
```

The command runs the line to its end in this terminal and prints what became of the item. It exits 0 when a pull request was opened or a question answered, 4 when the item needs you, 1 when it failed, 2 when cancelled.

The work forks from the tip of `origin`'s default branch (fetched first), whatever your checkout is on. Your checkout is never touched. Without an `origin`, the line still runs and the reviewed branch stays in its worktree.

| The item ends | Meaning | Next |
| --- | --- | --- |
| `delivered` | The draft pull request is open | Review it, mark it ready, merge it |
| `answered` | It was a question; the answer was printed | — |
| `needs-answer` | Triage asked questions and nothing was built | `agent-mux factory answer <item> "<answer>"` runs it again with your answer |
| `unresolved` | The review still did not pass after two revisions | Read the findings; fix in the worktree yourself, or `retry` |
| `rejected` | The reviewer says the approach is wrong | Rewrite the request, or `retry` |
| `failed` | A station gave no answer, the run hit its budget, or the delivery failed | `agent-mux factory retry <item>` |

```sh
agent-mux factory ls [--state S] [--all] [--json]    # this workspace's items; --all: every workspace
agent-mux factory show <item> [--json]               # the item, its text and its history
agent-mux factory retry <item> [--force]
agent-mux factory cancel <item> [--force]
```

An item is named by its id (`wi_…`), a unique prefix of it, or `#<issue>`. An issue becomes an item once: a second `run --issue 214` says which item it already is.

`retry` starts a fresh run. After a failed **delivery** (the review had approved) it only delivers again. A run is stopped with Ctrl-C in its terminal; if that process is gone and the item still reads `running`, `retry --force` or `cancel --force` takes it over.

## 3. Delivery

When the review approves, agent-mux:

1. Names the branch `<prefix><kind>-<title words>` (`factory/bug-reset-mail-sent-twice`), adding `-2`, `-3` when the remote has it.
2. Pushes the worktree's `HEAD` to that one branch: no `--force`, no tags, hooks off. It refuses a destination outside the prefix, the repository's default branch, a worktree with uncommitted paths, and a branch with no commit.
3. Opens a **draft** pull request against the default branch with `gh pr create --draft`. The description is written from the stations' answers: the problem, the approach, each acceptance criterion with the reviewer's verdict and evidence, the commands that were run, deviations from the plan, and `Closes #N` for an issue.

Nothing in agent-mux marks a pull request ready or merges it. Keep branch protection on the default branch as a second line.

## 4. The brain

One short Markdown file per repository with the durable facts earlier runs learned: a build quirk, a check that is not obvious, a convention a station missed.

- It lives in the library, `~/.agent-mux/factory/<key>/brain.md`, keyed by the `origin` remote (the SSH and HTTPS forms of one remote are one brain), not in the repository.
- agent-mux reads it when a run starts and puts it in every station's context. No station opens the file, so it works the same on every harness and needs no memory tool.
- Only you write it. The implementer and the reviewer may **suggest** a note; a run prints the suggestions and adds nothing.
- It is bounded (40,000 characters by default). An addition past the bound is refused: edit the page down.

```sh
agent-mux factory brain show | path | edit
agent-mux factory brain add "integration tests need --no-fail-fast; the full run takes two minutes"
```

The factory does not use ai-memory.

## 5. Settings

```toml
[factory]
branch_prefix = "factory/"   # every pushed branch starts with it
budget_tokens = 0            # a token ceiling per item; 0 = none
brain_max_chars = 40000
setup_command = ""           # the implementer runs it once in its worktree, e.g. "pnpm install"
```

How often work may be sent back is the flow's own `max_cycles` (2) on its `build` step. Each station's harness and model are the flow's and the agents': set them there, or for one run with `--step`.

## 6. What is recorded

- `work_items` and `work_item_events` in the trace store (schema v28): the item, its state, its latest run, its branch and pull request, and its history. GitHub stays the system of record for issues and pull requests.
- The run itself is a workflow run: `agent-mux workflow status <run>`, the Run screen, `<runtime>/workflows/<run>/` with the journal, each session's output, `artifacts/` (longer documents a station left for later ones) and `worktrees.json` (the build step's worktree).

## 7. Limits of phase 1

- No Factory screen, no label intake, no comments on the issue: start items from the command line.
- One item per `factory run` process; it does not queue.
- A reviewer on Antigravity cannot run commands (it runs custom agents read-only), so it judges from the files alone; the choice of reviewer avoids it when another harness is there.
- The no-push rule is enforced by the guard only on harnesses where agent-mux's hooks run for the session; elsewhere it rests on the station's instructions and on delivery being the only code that pushes.
- A station's artifact rules (one file, never overwritten, 256 KiB) are instructions, not enforced.
