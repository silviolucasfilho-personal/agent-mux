# TDD evidence — per-run loop pattern digest

Branch `feat/loop-pattern-digest`, off `5060a5e`. Date 2026-09-22.

## Source plan

No `*.plan.md`. The task came from a comparison of `../loopy` (Loop Library)
against agent-mux's own loop machinery. Loopy's run receipt
(`skills/loopy/references/run.md`) requires a run to pin its definition:
"the exact fetched/local/pasted definition, or SHA-256 plus exact prompt,
verification, and stopping content". agent-mux recorded no equivalent for a
loop run. Journeys were written during this run.

## User journeys

1. As a user auditing my loops, I want each run to record a fingerprint of
   the pattern text it actually ran, so that when I compare two runs of the
   same pattern I can tell whether the instructions changed underneath me.
2. As a maintainer debriefing a loop, I want to know that a behaviour change
   came from an edited pattern rather than from the codebase, so that I do
   not chase a phantom regression.

## The defect

`loop_runs.pattern` stores the pattern id. `~/.agent-mux/loops/registry.toml`
shadows the compiled-in `loops/registry.toml` by id, and the Configuration
view edits it in place, so `daily-triage` can resolve to materially different
text between two runs. Nothing in the row distinguished them, while
`workflow_runs.document_hash` has pinned workflow documents since v13.

## Task report

| Step | What happened | Command | Result |
|---|---|---|---|
| RED | Three tests written against the missing `Pattern::digest()` / `LoopRun::pattern_hash` | `cargo test --lib loops::` / `cargo test --test loop_runs a_run_records` | FAIL (compile), E0599 + E0609 only |
| GREEN (1st attempt) | Digest set on the scheduling-time row | `cargo test --test loop_runs a_run_records` | FAIL: `left: None`. That row is discarded unless the spawn fails |
| GREEN | Digest captured on `LiveLoopRun` at launch, applied at finish, as `readiness_score` already does | same | PASS |
| Schema | First `ALTER TABLE loop_runs ADD COLUMN` broke `v10_store_is_accepted_when_its_trace_schema_matches_v9`: `migration to v14: duplicate column name`. Replaced with a replay-safe `loop_run_patterns` side table | `cargo test` | PASS |
| Refactor | None needed; implementation follows existing conventions | `cargo clippy --all-targets` | clean |
| Docs | `docs/loops.md` section 7 records the digest | — | — |

RED excerpt:

```
error[E0599]: no method named `digest` found for struct `loops::Pattern`
error[E0609]: no field `pattern_hash` on type `loops::store::LoopRun`
```

GREEN excerpt:

```
test loops::tests::a_pattern_digest_is_stable_and_follows_every_field ... ok
test loops::store::tests::a_run_records_the_pattern_text_it_ran ... ok
test a_run_records_the_digest_of_the_pattern_it_ran ... ok
```

## Test specification

| # | What is guaranteed | Test | Type | Result | Evidence |
|---|---|---|---|---|---|
| 1 | A pattern's digest is stable across calls and is 64 hex chars | `src/loops/mod.rs:a_pattern_digest_is_stable_and_follows_every_field` | unit | PASS | `cargo test --lib a_pattern_digest` |
| 2 | Two different built-in patterns have different digests | same | unit | PASS | same |
| 3 | Editing any of goal, prompt, skills, verifier, human_gates, model, verifier_model, max_tokens_per_day or cost.early_exit_required changes the digest | same | unit | PASS | same |
| 4 | A run's digest survives the store round trip | `src/loops/store.rs:a_run_records_the_pattern_text_it_ran` | integration | PASS | `cargo test --lib a_run_records_the_pattern` |
| 5 | Two runs of the same pattern id with different text are distinguishable in the store | same | integration | PASS | same |
| 6 | Re-upserting a run updates its digest in place | same | integration | PASS | same |
| 7 | A row written without a digest reads back `None`, not an error | same | integration | PASS | same |
| 8 | A scheduled run's stored row equals the effective pattern's digest end to end | `tests/loop_runs.rs:a_run_records_the_digest_of_the_pattern_it_ran` | e2e | PASS | `cargo test --test loop_runs` |
| 9 | An edited pattern is not mistaken for the one that ran | same | e2e | PASS | same |

## Coverage and known gaps

No coverage tooling is configured in this repository (no `cargo-llvm-cov`
or `tarpaulin` in the tree or CI). Adding one was out of scope, so no
percentage is claimed. Every line added by this change is executed by at
least one of the nine guarantees above; the table is the coverage claim.

Full suite: `cargo test` → 479 passed, 1 failed.
The failure is `dossier_builds_are_byte_for_byte_identical`, which fails
identically on the unmodified branch point `5060a5e` under a full parallel
`cargo test` and passes there in isolation (verified in a detached
worktree, 4 runs of the binary and 5 of the test, all green on both sides).
Pre-existing flake, not a regression from this change.

Deliberate gaps:

- The digest covers the pattern entry only, not the bodies of the loop
  skills it names. A shadowed `~/.agent-mux/loops/skills/<name>/SKILL.md`
  changes a run's behaviour without changing the digest. Closing this means
  resolving each skill's effective text at launch; it is the natural
  follow-up.
- The digest is a fingerprint, not the text. `workflow_runs` stores both
  `document_hash` and `document`; loops store only the hash, so a reader
  learns that two runs differ but not how.
- Rows predating schema v14 carry no digest and are not backfilled: their
  text was never recorded, and inferring it from today's registry would
  assert something untrue.
- Nothing yet surfaces the digest — not the Loops view, `agent-mux loop
  runs`, or the MCP server. This change records it; reading it is separate.

## Merge evidence

Checkpoint commits on `feat/loop-pattern-digest`, all reachable from HEAD:

- `ac78e36` test: add reproducer for the missing loop pattern digest (RED)
- `908d5f7` fix: record the digest of the pattern text each loop run executed (GREEN)
- `a1bab51` docs: note the per-run pattern digest in the loops guide

If these are squashed, the RED/GREEN summary above must reach the PR body.
