# Timeline and tree views for the local trace browser

**Status:** Implemented (plan: `../plans/2026-09-04-trace-views.md`, all tasks landed).
**Request (2026-09-04):** "can we add two new views to the local sqlite tracing, one a timeline and other an hierarchical tree view?"
**Builds on:** `2026-09-03-sqlite-trace-store-design.md` (the browser, `list_observations`) and `2026-09-03-hook-channel-design.md` (subagent nesting, hook-pinned timings).

## Purpose

A turn's observations are currently one flat list, indented a little where subagents nest. Two questions it answers badly: *what is the shape of this turn* (which tool calls belong to which subagent) and *where did the time go* (what ran long, what ran in parallel, where the model was thinking between calls). A tree view answers the first, a timeline the second.

## Decisions

1. **Two more views of the same pane, not two more panes.** The browser's Detail pane gains a view mode — `list` (today's), `tree`, `timeline` — cycled with `v`. Sessions and Turns are untouched, and every view renders the same `observations` vector the pane already loads, so no new queries and no new state to keep in sync.
2. **The rendering is pure and shared.** `src/tracing/view.rs` turns `&[ObservationView]` into `TreeRow`s and `TimelineRow`s: box-drawing prefixes, collapse filtering, subtree rollups, and bar geometry are ordinary functions with unit tests, and the TUI and `trace show` both call them. No terminal needed to test the interesting logic.
3. **Tree rows collapse.** A row with children is collapsible with `space`; a collapsed row hides its whole subtree and reports what it hid (`+7 · 12k tok · $0.04`), which is what makes a subagent with twenty tool calls readable. Collapse state is per-turn and resets when the turn changes.
4. **The timeline is proportional to the turn, not to the rows.** The window is the turn's own start and end, so gaps between bars are real thinking and model latency, and a turn that took a minute does not look like one that took a second. Only an *open* turn extends to now: a closed turn holding a tool that never reported an end would otherwise be stretched across every day since it ran, squashing every real bar to one cell. Rows keep their tree indentation in the label column, so nesting stays visible while time is being read.
5. **Hook-pinned timings matter here.** A tool row whose start and end came from `PreToolUse`/`PostToolUse` is drawn from those, and rows still running are drawn to the right edge with a distinct cap. This is where the hook channel's precision becomes visible.
6. **The CLI gets the same two views.** `trace show <trace> --tree` and `--timeline` print the same shapes, so a turn can be inspected without the TUI and pasted into a report.

## Views

**Tree** — one row per observation, box-drawing connectors from the depth sequence:

```
💬 assistant                      1.2s   4.1k   $0.02
🤖 agent: Explore            [+7] 8.4s   31k    $0.11
├─ 🔧 Grep                        0.2s
└─ 🔧 Read                        0.1s
🔧 Bash                           2.4s
```

A collapsed parent shows `[+n]` and its subtree's tokens and cost; an expanded one shows its children beneath it.

**Timeline** — a scale header, then one bar per observation across the turn's window:

```
 0ms         1.2s         2.4s        3.6s
 assistant   ████
 agent:…     ░░░░████████████████
 ├─ Grep     ░░░░░░░░███
 └─ Read     ░░░░░░░░░░░░░██
 Bash        ░░░░░░░░░░░░░░░░░░░░░░████████▶
```

Leading `░` is the offset from the turn's start (drawn dim), `█` is the observation's duration, `▶` caps a row that is still running, and an instant event is a single `▏`. Errors draw red, generations blue, agents cyan.

## Keys

| Key | Effect |
|---|---|
| `v` | cycle the Detail pane: list → tree → timeline → list |
| `space` | tree: collapse or expand the selected row's subtree |
| everything else | unchanged (`Tab`, `↑/↓`, `Enter`, `/`, `a`, `r`, `Esc`) |

`Enter` still opens the selected observation's body in any view, so the views are ways to *find* a row, not dead ends.

## CLI

`agent-mux trace show <trace> [--tree | --timeline] [--full]`, mutually exclusive with each other, `--full` still printing bodies underneath. Without either flag the existing table is printed unchanged.

## Testing

- **Tree**: connector prefixes for a mixed-depth run (last child, middle child, deep nesting), collapse hides exactly the subtree and nothing else, rollups sum only descendants, a row with no children is not collapsible.
- **Timeline**: bar offset and width are proportional and clamped to the pane, a zero-length observation still draws one cell, a running row reaches the edge, an observation outside the window clamps rather than panics, a zero-width or zero-duration window degrades safely.
- **TUI**: `v` cycles, `space` toggles only in tree view, changing turn resets collapse, the renderer draws each view without panicking at small terminal sizes.
- **CLI**: `--tree` and `--timeline` print their shapes; both together is an error.

## Out of scope

A session-level timeline across turns (turn bars over a session's lifetime), horizontal zoom and pan in the timeline, and collapse-all/expand-all. Each is a small, separate addition on top of these functions.

## Files

| File | Change |
|---|---|
| `src/tracing/view.rs` (new) | `TreeRow`, `TimelineRow`, `Window`, `Rollup`, geometry, tests. |
| `src/tracing/mod.rs` | `pub mod view;` |
| `src/app.rs` | `DetailView`, collapse set, `v` and `space` keys, reset on turn change. |
| `src/ui.rs` | render tree and timeline in the Detail pane; footer hint. |
| `src/tracing/cli.rs` | `show --tree` / `--timeline`. |
