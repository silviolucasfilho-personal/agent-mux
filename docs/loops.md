# Loop Engineering in agent-mux

A **loop** is a scheduled, bounded agent run against one workspace: it reads a state file, triages, at most proposes one fix in a worktree, updates the state file and stops. agent-mux is the loop's scheduler, observer and enforcer. The skills that run inside a loop are agent-mux's own (`loops/skills`), the facts a run reasons about are computed in Rust, and the files a workspace keeps follow the loop-engineering method's conventions so other tooling can read them. Nothing from another vendor is installed or executed.

Design: `docs/superpowers/specs/2026-09-15-loop-engineering-design.md`. Not to be confused with `trace loops`, the per-turn agentic-loop diagnostics.

---

## 1. Week one

1. Open agent-mux, `Tab` to the **Loops** section (between Agents and History), press `a`.
2. Pick the workspace with the same directory picker as the New session dialog (it starts on the directory of an open session, the current directory or a profile's `default_dir`; `↓` into the subfolder list, `→`/`←` to enter or go up, type in the list to search subfolders three levels deep), a pattern, a Claude Code or Codex profile, the cadence, `L1`, and leave **Scaffold** on. `Enter`.
3. agent-mux writes the missing contract files and skills into the workspace (never overwriting), registers the loop in `~/.agent-mux/loops.json`, and shows the readiness score.
4. Press `r` to run once now, or wait for the slot. The run appears in **Active** as an ordinary session (name `<pattern> ↻ <workspace>`); attach to watch it.
5. The preview card shows what the last run found, in the words the report uses (`needs you`, `reported`, `quiet`), with its summary line; `loop-run-log.md` in the workspace gains one line per completed run.

Keep L1 (report-only) for a week. Promote to L2 with `e` when the readiness audit allows it (score ≥ 58 and a triage skill) and you have read a week of state files.

## 2. The files a loop keeps in the workspace

| File | Who writes it | What it holds |
| --- | --- | --- |
| `STATE.md` or `<pattern>-state.md` | the skill, every run | `Last run: <RFC3339>`, `## High Priority (loop is acting or waiting on human)`, `## Watch List`, `## Recent Noise (ignored this run)`, footer `Run log:` |
| `LOOP.md` | scaffolder once, then you | Active Loops table, Human Gates, Budget, Worktrees, Safety |
| `loop-run-log.md` | agent-mux after every completed run | one JSON line per run after the marker; pruned after 30 days; tokens come from the store |
| `loop-budget.md` | scaffolder once, then you | the daily caps table and the kill switch (`loop-pause-all`) |
| `loop-constraints.md` | scaffolder once, then you | the binding rules the `loop-rules` skill loads |
| `gate.yaml` | scaffolder once, then you | `denylist` globs, `maxFiles`, `autoMergeAllowlist` (recorded, never acted on) |
| `loop-ledger.json` | agent-mux, fix patterns only | one attempt per run for the circuit breaker |
| `.loop-worktrees/` | agent-mux | L2+ worktrees and `manifest.json`. A fresh worktree only holds committed files, so agent-mux copies the (usually untracked) `loop-*` skills and the verifier into it before the run; those copies never count as a change. |
| `.claude/skills/loop-*/`, `.codex/skills/loop-*/`, `.claude/agents/<agent>.md`, `.codex/agents/<agent>.toml` (the verifier keeps the name `verifier.toml`) | scaffolder | the skills and the agents the pattern names (`agents` in the registry, the verifier alone by default), per harness. Every installed agent opens with the `## Baseline` section from `[agent] preamble` of `prompts.toml`. |

The literal `loop-pause-all` on a line of its own in the state file or `LOOP.md` pauses every loop of that workspace.

## 3. Which model runs a loop

A loop is one session: the triage skill runs, it may call `loop-fix`, and it
hands a change to the `loop-verifier` sub-agent. So a loop has two models to
choose, not one per step:

| Field | What it sets | Where |
| --- | --- | --- |
| `Model` | the run's own session (`--model`) | the add/edit dialog, `loop add --model`, `model` on the entry in `loops.json` |
| `Verifier` | the `loop-verifier` sub-agent, written into the agent file the scaffolder installs (`model:` in its frontmatter) | the dialog, `loop add --verifier-model`, `verifier_model` on the entry |

Blank leaves the profile's model for the run, and `inherit` for the
verifier, which is what the shipped agent file says. A pattern may suggest
both in `loops/registry.toml` (`model`, `verifier_model`); a loop copies
them when it is registered and can change them afterwards.

On Claude Code the verifier model is the `model:` line of the agent file.
On Codex it reaches the agent TOML as `model` only when it is not a Claude
alias (`sonnet`, `opus`, `haiku`, `inherit`), since Codex reads a config
overlay with `model` and `developer_instructions` (codex-cli 0.155.1; see
`src/loops/scaffold.rs`). The scaffolder never overwrites an existing agent file,
so a loop whose verifier model changed after scaffolding shows the
difference in the Loops view's **Files** tab, where the line reads what the
file declares against what the loop asks for. Edit the file with the
Configuration view (`C`) or delete it and re-scaffold.

`loop-fix` and the other skills run inside the same session on the same
model: a skill is a prompt, not a process, so it has no model of its own.

## 4. Patterns

| id | default cadence | week-one level | state file | skills | runs/day | tokens/day | breaker |
| --- | --- | --- | --- | --- | --- | --- | --- |
| daily-triage | 1d | L1 | `STATE.md` | loop-triage, loop-fix, loop-rules | 2 | 100k | no |
| pr-babysitter | 15m | L1 | `pr-babysitter-state.md` | loop-pr-triage, loop-fix, loop-rules | 288 | 2M | yes |
| ci-sweeper | 15m | L2 | `ci-sweeper-state.md` | loop-ci-triage, loop-fix, loop-rules, verifier | 96 | 1M | yes |
| post-merge-cleanup | 1d | L1 | `post-merge-state.md` | loop-post-merge, loop-fix, loop-rules | 1 | 200k | yes |
| dependency-sweeper | 1d | L2 | `dependency-sweeper-state.md` | loop-dependency-triage, loop-fix, loop-rules, verifier | 4 | 500k | yes |
| changelog-drafter | 1d | L1 | `changelog-drafter-state.md` | loop-changelog, loop-rules, verifier | 1 | 100k | no |
| issue-triage | 1d | L1 | `issue-triage-state.md` | loop-issue-triage, loop-rules, verifier | 12 | 80k | no |
| continuous-pr | 1h | L2 | `continuous-pr-state.md` | loop-issue-pick, loop-fix, loop-rules, verifier | 12 | 1.5M | yes |
| harness-audit | 1d | L1 | `harness-audit-state.md` | loop-harness-audit, loop-rules | 2 | 100k | no |

`continuous-pr` is the one pattern that starts at L2: it picks one issue a human labelled `ready`, hands it to `loop-fix` on the run's branch, asks every checker the pattern names, and leaves the branch in the Inbox for a human to open the pull request from; it never pushes, comments or opens the PR itself. `harness-audit` is always report-only whatever its level: it reads the loops' own runs (`agent-mux loop ls|runs|show`, or the MCP `agent_mux_get_loop_context` and trace tools) and writes at most three tuning proposals (an interval, a token cap, a level promotion or demotion, a denylist glob, a second checker) as High Priority items with the facts behind them; applying one is a human gate.

Registry: `loops/registry.toml` (embedded), merged by id with `~/.agent-mux/loops/registry.toml` when you add or replace patterns; the skills, the agents and the templates are overridden the same way under `~/.agent-mux/loops/` (the Configuration view `C`, `agent-mux config`, `docs/configuration.md`). A pattern may carry its own `prompt`, replacing `[loop] run` of `prompts.toml` for its runs, and an `agents` list naming the loop agents the scaffolder installs for it (`loops/agents/<name>.md`, built-in or library, including agents brought in with `agent-mux agent import`); an empty list means `loop-verifier` alone when `verifier = true` and no agent otherwise, and a pattern with `verifier = true` must keep `loop-verifier` in the list. Two checkers ship built in: `loop-verifier` runs the tests and reads the diff; `loop-reviewer` reads the diff only, for scope creep, risk areas and missing tests, and answers the same `## Verdict:` line. A pattern that lists `agents = ["loop-verifier", "loop-reviewer"]` requires both: agent-mux collects one verdict per checker sub-agent that answered (a name containing `verifier` or `reviewer`), and only unanimous APPROVEs propose the fix (section 6). Every skill reads `$AGENT_MUX_LOOP_CONTEXT` first, makes its one listing call and compares a fingerprint of it with the `Fingerprint:` line in the state file: unchanged means the run rewrites `Last run:` and ends as `no-op` under 5k tokens (the early exit the cost model assumes); otherwise it follows a bounded procedure, only edits the state file at L1, may call `loop-fix` for one item at L2, hands changes to `loop-verifier`, rewrites the state file, and ends with a fenced `loop-result` block (`outcome`, `items_found`, `actions_taken`, `escalations`, `summary`).

## 5. Levels and what enforces them

| Level | Meaning | Where | Enforced by |
| --- | --- | --- | --- |
| L1 | report-only | the workspace | the `PreToolUse` guard permits only the state file, the run log and `.loop-context/` |
| L2 | assisted: one fix, verifier, human decides | a worktree `.loop-worktrees/<run>` on branch `loop/<run>`; the state file, run log and ledger stay in the workspace (`$AGENT_MUX_LOOP_STATE`, absolute paths in the context) | guard: denylist, `maxFiles`, no push or merge; post-run gate re-check; inbox |
| L3 | unattended | worktree | same guard; readiness ≥ 78 with verifier, cost observability and fresh activity |

A loop run passes `--dangerously-skip-permissions` (Claude Code) or `--yolo` (Codex) whatever the profile says: print mode cannot answer an approval prompt, and the guard, the gate and the worktree are the controls instead.

Per-harness ceiling: Claude Code L3 (per-launch guard with `--loop`, fail-closed for write tools); Codex L3 with `agent-mux trace hooks install codex`, else L1; Antigravity not supported (section 8).

The effective level of a run can be lower than the configured one: tokens today at 80 % or more of the cap, a circuit breaker one attempt short of tripping (two identical errors where three trip it, nine attempts where ten are the cap), a stale state file (`Last run` older than 14 days), a readiness gate that no longer holds, or a missing guard cap the run at L1; the reason travels in the context (`run.level_reason`, `breaker.near_trip`) and the preview, so the loop reports instead of spending its last attempt.

## 6. Pre-flight, in order

Kill switch (`K`, or the literal in the files) → workspace exists (git repository for L2+) → runs today below the cap → tokens today below 100 % of the cap (80 % forces report-only) → circuit breaker (3× same error, 3 similar errors, 5 consecutive failures, 10 iterations; one attempt short of any of these caps the run at L1 instead) → readiness → harness resolves → the pattern's triage skill is installed in the workspace for the harness → concurrency (`max_concurrent`, one run per workspace). A blocked run is stored in `loop_runs` with its reason and never written to `loop-run-log.md`; a breaker trip pauses the loop.

A run's session appears in **Active** under its loop's header — `▾ ⟳ <loop id> <running>/<total>▶ <pattern>` — so successive runs of one loop stack together instead of scattering through the list; `space` folds the loop away, and the trace browser (`T`) groups the same runs the same way.

## 7. After a run

agent-mux reads tokens and cost from the trace store, which checker sub-agents ran (a name containing `verifier` or `reviewer`) and what each answered (`## Verdict: APPROVE | REJECT | ESCALATE_HUMAN`), the files the run's write tools touched, and the worktree's changes. The verdicts add up to one word: any `ESCALATE_HUMAN` wins, then any `REJECT`, and only unanimous `APPROVE`s approve; the run row keeps the list, the word and a label such as `APPROVE (2/2)` or `REJECT (1/2)` that the Loops view shows. The outcome is the `loop-result` block when present and consistent, else derived, with the checkers' rule first: a changed worktree (or a block claiming a fix) against a `REJECT` or `ESCALATE_HUMAN` → `escalated`, the branch stays for a human and nothing is proposed; otherwise worktree changed → `fix-proposed`; a checker said `ESCALATE_HUMAN` or the High Priority section grew → `escalated`; state file changed → `report-only`; nothing → `no-op`; non-zero exit or timeout → `failed`. A fix without a checker observation is flagged `verifier_missing`. A touched path on the denylist forces `escalated` and pauses the loop.

Then: the `loop_runs` row, the run-log line, the ledger attempt (fix patterns), a copy of the state file under `<runtime>/loops/state/<loop id>/<run id>.md` with the difference against the previous run (`detail.delta`, or `detail.quiet` when nothing moved), the registry (`next_run_at`, `last_run_id`, auto-pause on failure), and the worktree: removed when nothing changed, kept and listed in the **Inbox** otherwise.

## 8. Reading a run

`E` opens the Loops view on the **Report** tab: what the selected run found
and who has to act. It is the state file the run wrote, parsed into the
shape every loop skill already keeps.

```text
Sep 17 17:36   NEEDS YOU   L1
8 found · 2 for you · 417k tokens · $1.18 · 1m 43s
#2238 conflicts and #1919 is blocked on a missing check
Since last run  #2238 CLEAN → CONFLICTING after a push
────────────────────────────────────────
Needs you (2)
  #2238 chore(spec-wave): atualiza arquivos para v0.34.2
    conflicts (CONFLICTING, DIRTY); touches `.github/workflows/**` (denylist)
    Decide   rebase spec-wave/update-v0.34.2 on develop, or regenerate and close
    Loop did reported only; conflicts and workflow files are human gates
Watching (6)
Ignored (3)
What the run said
  The open PR queue has two items that need a human. …
```

- **Needs you** is `## High Priority`, one card per item: the id and title,
  the status fragment, then the `Human decision:` and `Loop action:` lines.
- **Since last run** is the difference against the previous run's report:
  items that are new, gone, moved bucket or changed status.
- **What the run said** is the run's final message.

`↑` `↓` step back through the runs; the report follows the selection, so an
older run shows the report *it* wrote. agent-mux keeps one copy of the state
file per run under `<runtime>/loops/state/<loop id>/`, pruned after 30 days,
because the workspace's file is rewritten in place every run.

The **Runs** tab (`2`) is the timeline those reports sit in. A run that
changed nothing folds away, so a fifteen-minute loop still reads as a
changelog:

```text
Sep 17 17:36   NEEDS YOU   L1   8 found · 2 for you · 417k tokens · $1.18 · 1m 43s
  #2238 conflicts and #1919 is blocked on a missing check
Sep 17 14:06 … 17:21   quiet ×13   1.1M tokens · $2.60
Sep 16 16:03 … 17:33   skipped · tokens today 2.2M at the cap of 2.0M ×7
```

The selected run opens with why it ended that way, the verifier's verdict,
the files it touched, the first lines of what it said, its launch id, exit
code and readiness. Outcome words are for the reader (`needs you`,
`fix ready`, `reported`, `quiet`, `skipped`, `failed`); the values stored in
`loop_runs.outcome` do not change.

A run whose harness has no prices in `pricing.toml` shows `unpriced
(<harness>)`, never `$0.00`.

## 9. The inbox

The **Inbox** tab lists runs waiting on a decision across every loop, with the branch, the worktree path, the files, the verifier verdict and `git diff --stat`. `a` marks the run **applied**: the worktree is removed, the branch stays for you to merge (`git merge loop/<run>`); agent-mux never merges. `x` marks it **rejected**: worktree and branch are removed.

## 10. Antigravity

Not supported for loops in this version. agy 1.2.3 requires a `decision` in every `PreToolUse` reply and each value changes permission behaviour, so no selective path guard can be registered; its hooks and MCP entry are global installs, its skills and agents are user-level only, and sub-agent spawning is unverified. The spec's section 16 lists the probes and the two deliveries that bring it in.

## 11. When something trips

| Symptom | What to do |
| --- | --- |
| Loop shows `‖` with a reason | `p` resumes; a breaker reason also resets the trailing failures in the ledger |
| `!` with an inbox count | `E` → Inbox, decide with `a` or `x` |
| Run blocked: tokens at the cap | raise the cap with `e`, or wait for UTC midnight |
| Run capped at L1: readiness | `E` → Readiness tab lists the missing signals |
| Run capped at L1: no path guard | Codex: `agent-mux trace hooks install codex`; Claude: the binary must run from an absolute path |
| `verifier_missing` on a fix | the skill did not hand the change to `loop-verifier`; treat the fix as unverified |
| Run failed: "the harness did not find /loop-…" | the skill file is missing from the run's directory; edit the loop with Scaffold on or run `agent-mux loop init`, and commit `.claude/skills` if you want it in every checkout |
| An edited loop skill or agent does not reach a workspace | the scaffolder never overwrites; press `u` in the Configuration view or run `agent-mux config push` |
| Run finished but did nothing | look at the session's scrollback (attach to it in Active); a loop run always bypasses the harness's own approval prompts because print mode has nobody to answer them, and the guard is the control |
| Nothing runs | `[loops] enabled = false`, the kill switch, or the loop is paused; `agent-mux trace doctor` has a `loops` section |

## 12. Command line

`agent-mux loop ls|add|rm|run|pause|resume|init|audit|status|report|runs|show|cost|inbox|decide` mirror the sidebar; `add` takes `--model` and `--verifier-model`; `report <id>` prints what a run found and who has to act (`--run <run_id>` for an older one), `runs <id>` the folded timeline (`--all` unfolds it) and `show <run_id>` one run in full; `loop run <id> --now` performs one scheduler pass headlessly (for cron) and exits 0 for report-only or no-op, 3 fix-proposed, 4 escalated, 1 blocked, 2 failed. The MCP tool `agent_mux_get_loop_context` gives a running loop its context recomputed now.

Configuration (`profiles.toml`):

```toml
[loops]
enabled = true            # the in-TUI scheduler
max_concurrent = 1        # loop runs at a time, across workspaces
catch_up = "once"         # "once" | "skip": a slot missed while agent-mux was closed
run_timeout_s = 900       # a run past this is killed and recorded as failed
worktrees_dir = ".loop-worktrees"
```
