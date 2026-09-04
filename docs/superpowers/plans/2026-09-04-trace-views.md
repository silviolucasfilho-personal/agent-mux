# Timeline and tree views: implementation plan

Spec: `../specs/2026-09-04-trace-views-design.md`. Each task ends green (`cargo clippy --all-targets`, `cargo test --no-fail-fast`).

### Task 1: View logic (`src/tracing/view.rs`, `src/tracing/mod.rs`)

**Interfaces:** `Rollup { count, tokens, cost }`; `TreeRow<'a> { obs, prefix, depth, has_children, collapsed, hidden, subtree }`; `tree_rows(&'a [ObservationView], &HashSet<String>) -> Vec<TreeRow<'a>>`; `Window { start_ns, end_ns }` with `window(turn_start, turn_end, obs, now_ns)` and `span_ns()`; `Bar { offset, width, running, instant }` with `bar(start_ns, end_ns, &Window, cols)`; `axis(&Window, cols) -> String`.

- [x] **Step 1:** Tests: connector prefixes over a mixed-depth run; collapse hides exactly one subtree; rollups sum descendants only; leaves are not collapsible; bar geometry (proportional, clamped, instant, running); degenerate windows (zero span, zero cols).
- [x] **Step 2:** Implement. One rule tightened while testing: only an *open* turn's window reaches `now`, so a closed turn holding an observation that never reported an end is not stretched across the days since it ran.

### Task 2: Browser wiring (`src/app.rs`)

**Interfaces:** `DetailView { List, Tree, Timeline }` with `next()`/`label()`; `TraceBrowserState.detail_view`, `.collapsed: HashSet<String>`; `visible_rows()` for selection bounds; `v` cycles, `space` toggles collapse (tree only); `load_observations` clears the collapse set.

- [x] **Step 1:** Tests: `v` cycles all three; `space` toggles only in tree and only on a parent; selection stays in range when a collapse hides rows; changing turn resets view state.
- [x] **Step 2:** Implement.

### Task 3: Rendering (`src/ui.rs`)

- [x] **Step 1:** Tests: each view renders into a small `TestBackend` without panic and shows its marks (connector glyphs, bar cells, axis).
- [x] **Step 2:** Implement; Detail title names the view, footer gains `[v] view` and `[space] fold`. Tree names are padded so the duration/token/cost columns line up down the pane.

### Task 4: CLI (`src/tracing/cli.rs`)

- [x] **Step 1:** Tests: `--tree` and `--timeline` shapes; both flags together errors.
- [x] **Step 2:** Implement; USAGE updated; spec status. `tree_lines` / `timeline_lines` return their rows so the shapes are unit-tested without capturing stdout.
