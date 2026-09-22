# TDD evidence: turn summary in the Trace browser

**Source plan:** none; the journey was written during this TDD run from the request
"the Trace screen must show a complete summary for a turn: total tokens
(input/output/cache), cost, and the number of each tool called".

Branch: `feat/turn-summary` (from `perf/trace-browser-navigation` @ `5bb74ae`).

## User journey

As a user reviewing a turn in the Trace browser (`T`), I want one view that
tells me what the turn cost: tokens by kind, dollars, and which tools ran how
often, so I can see where a turn's budget went without adding up rows.

## Design decisions

- A fifth Detail view, **Summary**, reached with `v` after Loop.
- Tokens and cost come from the turn's `trace_stats` row, the same numbers as
  the Turns pane. They are not re-summed from observations.
- Tool calls are `tool` rows plus subagent launches (`agent: …`). A subagent's
  transcript row is its work, not a call. So the total can be lower than the
  Turns pane `🔧` count, which counts every `agent` row.
- Tools are ordered most called first, ties by name, so the order holds across
  live redraws.
- Unknown usage stays `-`; the summary never invents zeros.

## Task report

| Stage | Commit | Command | Result |
|---|---|---|---|
| RED (compile-time) | `234e37b` | `cargo test --lib` | 6 errors, all intended: `turn_summary` not found (×4 in `view.rs`), `DetailView::Summary` not found (`app.rs`, `ui.rs`) |
| GREEN | `d2e3ddd` | `cargo test --lib -- tracing::view v_cycles tree_and_timeline` | `12 passed; 0 failed` |
| GREEN (whole lib) | `d2e3ddd` | `cargo test --lib` | `463 passed; 0 failed` |
| Lint | `d2e3ddd` | `cargo clippy --all-targets`, `cargo fmt --check` | no warnings; fmt clean |

## Test specification

| # | What is guaranteed | Test | Type | Result |
|---|---|---|---|---|
| 1 | Each tool is counted by name, most called first, ties by name; generations are not calls | `tracing::view::tests::the_summary_counts_each_tool_by_name_most_called_first` | unit | PASS |
| 2 | Subagent launches count as calls, their transcript rows do not, nested tools do | `tracing::view::tests::subagent_launches_are_tool_calls_but_their_transcripts_are_not` | unit | PASS |
| 3 | Input/output/cache read/cache write/total tokens, cost and unpriced count come from the turn totals | `tracing::view::tests::the_summary_takes_tokens_and_cost_from_the_turn_totals` | unit | PASS |
| 4 | An empty turn has no tools and unknown (not zero) tokens and cost | `tracing::view::tests::an_empty_turn_summarises_to_nothing_without_inventing_zeros` | unit | PASS |
| 5 | `v` cycles List → Tree → Timeline → Loop → Summary → List; Summary hides no rows | `app::history_tests::v_cycles_the_detail_view_and_space_folds_only_in_the_tree` | unit | PASS |
| 6 | The rendered pane names the view and shows each token row with its value, `$0.42`, `5 calls · 4 distinct`, `Grep ×2` before `Bash`, and the agent launch | `ui::tests::tree_and_timeline_views_render_their_own_shapes` | render (TestBackend) | PASS |

## Follow-up cycle: per-tool errors and a one-screen summary

Request: "add the per-tool error counts to the Summary view and keep the
summary in one screen only".

### User journey

As a user reviewing a turn's Summary, I want to see which tools failed and
how often, and to see the whole summary without scrolling, so I can spot a
failing tool in a busy turn at a glance.

### Design decisions

- A call is an error when its row's level is `ERROR`. Subagent launches
  count, generations don't. The Loop view counts only `tool` rows, so its
  error total can be lower.
- Each tool row shows `×calls` and, only when some failed, `✗errors` (red).
  The tools heading reads `N calls · M distinct · E errors`; the errors
  part is left out at zero.
- One screen: the tokens and cost headings merged into
  `── tokens · N generations ──`, the cells go two to a row (one to a row
  below 44 columns) and the blank lines are gone. The fixed part is 5 rows
  (8 when narrow).
- `trace_view::fit_tools(tools, rows)` keeps every tool that fits.
  Otherwise it shows `rows - 1` tools and folds the rest into
  `… +N more · C calls · E errors`, so the shown and folded calls always
  add up to the heading's total.

### Task report

| Stage | Commit | Command | Result |
|---|---|---|---|
| RED (compile-time) | `2fa67bc` | `cargo test --lib` | 11 errors, all intended: `Folded` (×2), `fit_tools` (×5), `ToolCount::errors` (×2), `TurnSummary::tool_errors` (×2) |
| GREEN | `d41dc1f` | `cargo test --lib -- tracing::view v_cycles tree_and_timeline` | `16 passed; 0 failed` |
| GREEN (whole lib) | `d41dc1f` | `cargo test --all-targets --no-fail-fast` (lib target) | `467 passed; 0 failed` |
| Integration suites | `d41dc1f` | `cargo test --all-targets --no-fail-fast`, then the suites after `skill_hydrate` (the first run was cut off there) | all pass except two failures from timing under parallel load: `heimdall_dossier::dossier_builds_are_byte_for_byte_identical` (known flake) and `trace_langfuse::launches_route_rows_by_backend_and_replay_reaches_langfuse` ("first fake claude never exited"). Rerun alone: `heimdall_dossier` 6 passed, `trace_langfuse` 8 passed |
| Lint | `d41dc1f` | `cargo clippy --all-targets`, `cargo fmt --check` | no warnings; fmt clean |

The UI assertions' RED is part of the compile-time RED above: the test
crate didn't build without `fit_tools`, so they never ran against the old
layout. I checked the new render by eye once at 180×18 (header, 9 tools,
`… +15 more · 15 calls · 1 error` on the pane's last row).

### Test specification

| # | What is guaranteed | Test | Type | Result |
|---|---|---|---|---|
| 7 | Each tool counts its `ERROR` calls; subagent launches count, generations don't; the total is their sum | `tracing::view::tests::the_summary_counts_each_tools_errors_and_their_total` | unit | PASS |
| 8 | An empty turn has zero tool errors | `tracing::view::tests::an_empty_turn_summarises_to_nothing_without_inventing_zeros` | unit | PASS |
| 9 | Every tool is shown when the rows are enough (exactly or more) | `tracing::view::tests::every_tool_is_shown_when_the_rows_are_enough` | unit | PASS |
| 10 | Tools that don't fit fold into the last row, carrying their tool, call and error counts | `tracing::view::tests::the_tools_that_do_not_fit_fold_into_one_last_row` | unit | PASS |
| 11 | With 0 or 1 rows every tool folds; no tools means no fold | `tracing::view::tests::with_one_row_or_none_every_tool_folds` | unit | PASS |
| 12 | The pane shows each value right after its label (tokens and cost), `5 calls · 4 distinct · 1 error`, `✗1` on Grep and none on Read | `ui::tests::tree_and_timeline_views_render_their_own_shapes` | render (TestBackend) | PASS |
| 13 | At 180×18 with 24 tools: totals and token/cost rows stay on screen, the least called tool is folded, the fold row's counts match what is hidden and it sits on the pane's last row | `ui::tests::tree_and_timeline_views_render_their_own_shapes` | render (TestBackend) | PASS |

## Coverage and known gaps

- Coverage wasn't measured: `cargo llvm-cov` isn't installed here.
- If the pane is shorter than the fixed header (5 rows, 8 when narrow), the
  header itself is clipped and the tools heading may not show. The
  cramped-terminal draws (40×10) only check that nothing panics.
- No refactor commit: the renderer keeps the file's inline `head`/`cell`
  closure style.
