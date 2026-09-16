# Loop Engineering in agent-mux

A **loop** is a scheduled, bounded agent run against one workspace: it reads a state file, triages, at most proposes one fix in a worktree, updates the state file and stops. agent-mux is the loop's scheduler, observer and enforcer. The skills that run inside a loop are agent-mux's own (`loops/skills`), the facts a run reasons about are computed in Rust, and the files a workspace keeps follow the loop-engineering method's conventions so other tooling can read them. Nothing from another vendor is installed or executed.

Design: `docs/superpowers/specs/2026-09-15-loop-engineering-design.md`. Not to be confused with `trace loops`, the per-turn agentic-loop diagnostics.

---

## 1. Week one

1. Open agent-mux, `Tab` to the **Loops** section (between Agents and History), press `a`.
2. Pick the workspace (a directory of an open session, a profile's `default_dir`, or a typed path), a pattern, a Claude Code or Codex profile, the cadence, `L1`, and leave **Scaffold** on. `Enter`.
3. agent-mux writes the missing contract files and skills into the workspace (never overwriting), registers the loop in `~/.agent-mux/loops.json`, and shows the readiness score.
4. Press `r` to run once now, or wait for the slot. The run appears in **Active** as an ordinary session (name `<pattern> ↻ <workspace>`); attach to watch it.
5. The preview card shows the outcome, tokens, cost and duration of the last run; `loop-run-log.md` in the workspace gains one line per completed run.

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
| `.claude/skills/loop-*/`, `.codex/skills/loop-*/`, `.claude/agents/loop-verifier.md`, `.codex/agents/verifier.toml` | scaffolder | the skills and the verifier, per harness |

The literal `loop-pause-all` on a line of its own in the state file or `LOOP.md` pauses every loop of that workspace.

## 3. Patterns

| id | default cadence | week-one level | state file | skills | runs/day | tokens/day | breaker |
| --- | --- | --- | --- | --- | --- | --- | --- |
| daily-triage | 1d | L1 | `STATE.md` | loop-triage, loop-fix, loop-rules | 2 | 100k | no |
| pr-babysitter | 15m | L1 | `pr-babysitter-state.md` | loop-pr-triage, loop-fix, loop-rules | 288 | 2M | yes |
| ci-sweeper | 15m | L2 | `ci-sweeper-state.md` | loop-ci-triage, loop-fix, loop-rules, verifier | 96 | 1M | yes |
| post-merge-cleanup | 1d | L1 | `post-merge-state.md` | loop-post-merge, loop-fix, loop-rules | 1 | 200k | yes |
| dependency-sweeper | 1d | L2 | `dependency-sweeper-state.md` | loop-dependency-triage, loop-fix, loop-rules, verifier | 4 | 500k | yes |
| changelog-drafter | 1d | L1 | `changelog-drafter-state.md` | loop-changelog, loop-rules, verifier | 1 | 100k | no |
| issue-triage | 1d | L1 | `issue-triage-state.md` | loop-issue-triage, loop-rules, verifier | 12 | 80k | no |

Registry: `loops/registry.toml` (embedded). Every skill reads `$AGENT_MUX_LOOP_CONTEXT` first, follows a bounded procedure, only edits the state file at L1, may call `loop-fix` for one item at L2, hands changes to `loop-verifier`, rewrites the state file, and ends with a fenced `loop-result` block (`outcome`, `items_found`, `actions_taken`, `escalations`, `summary`).

## 4. Levels and what enforces them

| Level | Meaning | Where | Enforced by |
| --- | --- | --- | --- |
| L1 | report-only | the workspace | the `PreToolUse` guard permits only the state file, the run log and `.loop-context/` |
| L2 | assisted: one fix, verifier, human decides | a worktree `.loop-worktrees/<run>` on branch `loop/<run>`; the state file, run log and ledger stay in the workspace (`$AGENT_MUX_LOOP_STATE`, absolute paths in the context) | guard: denylist, `maxFiles`, no push or merge; post-run gate re-check; inbox |
| L3 | unattended | worktree | same guard; readiness ≥ 78 with verifier, cost observability and fresh activity |

Per-harness ceiling: Claude Code L3 (per-launch guard with `--loop`, fail-closed for write tools); Codex L3 with `agent-mux trace hooks install codex`, else L1; Antigravity not supported (section 8).

The effective level of a run can be lower than the configured one: tokens today at 80 % or more of the cap, a stale state file (`Last run` older than 14 days), a readiness gate that no longer holds, or a missing guard cap the run at L1; the reason travels in the context and the preview.

## 5. Pre-flight, in order

Kill switch (`K`, or the literal in the files) → workspace exists (git repository for L2+) → runs today below the cap → tokens today below 100 % of the cap (80 % forces report-only) → circuit breaker (3× same error, 3 similar errors, 5 consecutive failures, 10 iterations) → readiness → harness resolves → the pattern's triage skill is installed in the workspace for the harness → concurrency (`max_concurrent`, one run per workspace). A blocked run is stored in `loop_runs` with its reason and never written to `loop-run-log.md`; a breaker trip pauses the loop.

## 6. After a run

agent-mux reads tokens and cost from the trace store, whether a sub-agent whose name contains `verifier` ran and what it answered (`## Verdict: APPROVE | REJECT | ESCALATE_HUMAN`), the files the run's write tools touched, and the worktree's changes. The outcome is the `loop-result` block when present and consistent, else derived: worktree changed → `fix-proposed`; verifier said `ESCALATE_HUMAN` or the High Priority section grew → `escalated`; state file changed → `report-only`; nothing → `no-op`; non-zero exit or timeout → `failed`. A fix without a verifier observation is flagged `verifier_missing`. A touched path on the denylist forces `escalated` and pauses the loop.

Then: the `loop_runs` row, the run-log line, the ledger attempt (fix patterns), the registry (`next_run_at`, `last_run_id`, auto-pause on failure), and the worktree: removed when nothing changed, kept and listed in the **Inbox** otherwise.

## 7. The inbox

`E` opens the Loops view; the **Inbox** tab lists runs waiting on a decision across every loop, with the branch, the worktree path, the files, the verifier verdict and `git diff --stat`. `a` marks the run **applied**: the worktree is removed, the branch stays for you to merge (`git merge loop/<run>`); agent-mux never merges. `x` marks it **rejected**: worktree and branch are removed.

## 8. Antigravity

Not supported for loops in this version. agy 1.2.3 requires a `decision` in every `PreToolUse` reply and each value changes permission behaviour, so no selective path guard can be registered; its hooks and MCP entry are global installs, its skills and agents are user-level only, and sub-agent spawning is unverified. The spec's section 16 lists the probes and the two deliveries that bring it in.

## 9. When something trips

| Symptom | What to do |
| --- | --- |
| Loop shows `‖` with a reason | `p` resumes; a breaker reason also resets the trailing failures in the ledger |
| `!` with an inbox count | `E` → Inbox, decide with `a` or `x` |
| Run blocked: tokens at the cap | raise the cap with `e`, or wait for UTC midnight |
| Run capped at L1: readiness | `E` → Readiness tab lists the missing signals |
| Run capped at L1: no path guard | Codex: `agent-mux trace hooks install codex`; Claude: the binary must run from an absolute path |
| `verifier_missing` on a fix | the skill did not hand the change to `loop-verifier`; treat the fix as unverified |
| Run failed: "the harness did not find /loop-…" | the skill file is missing from the run's directory; edit the loop with Scaffold on or run `agent-mux loop init`, and commit `.claude/skills` if you want it in every checkout |
| Run finished but did nothing | print mode cannot answer permission prompts: set `bypass_approvals = true` on the loop's profile |
| Nothing runs | `[loops] enabled = false`, the kill switch, or the loop is paused; `agent-mux trace doctor` has a `loops` section |

## 10. Command line

`agent-mux loop ls|add|rm|run|pause|resume|init|audit|status|cost|inbox|decide` mirror the sidebar; `loop run <id> --now` performs one scheduler pass headlessly (for cron) and exits 0 for report-only or no-op, 3 fix-proposed, 4 escalated, 1 blocked, 2 failed. The MCP tool `agent_mux_get_loop_context` gives a running loop its context recomputed now.

Configuration (`profiles.toml`):

```toml
[loops]
enabled = true            # the in-TUI scheduler
max_concurrent = 1        # loop runs at a time, across workspaces
catch_up = "once"         # "once" | "skip": a slot missed while agent-mux was closed
run_timeout_s = 900       # a run past this is killed and recorded as failed
worktrees_dir = ".loop-worktrees"
```
