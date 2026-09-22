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

## Coverage and known gaps

- Coverage wasn't measured: `cargo llvm-cov` isn't installed here.
- The full `cargo test --all-targets` run was not executed in this session (the
  user stopped it); only the library target was run.
- The Summary view doesn't show per-tool error counts; the Loop view already
  shows the turn's tool errors.
- No refactor commit: the view reuses the Loop view's `head`/`kv` line shape
  inline, matching the file's existing style.
