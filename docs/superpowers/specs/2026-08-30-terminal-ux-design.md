# agent-mux terminal UX — scrollback, mouse, selection, search

**Date:** 2026-08-30
**Status:** Approved design, pre-implementation
**Builds on:** v1 (`2026-08-29-agent-mux-design.md`), branch `worktree-agent-mux-v1`

## Purpose

Make the embedded terminal pane feel like a real terminal emulator
(Ghostty / iTerm2): scrollable history, mouse support forwarded to agents
that want it, click-drag selection with clipboard copy/paste, and search
over scrollback. No change to session management, status detection, or the
agent-verbatim rendering principle.

## Requirements

- Scroll the selected session's history with the mouse wheel and keyboard,
  in both Attached mode and the Control-mode preview.
- Wheel routing like a real emulator: forwarded to the agent when it
  enabled mouse reporting; arrow keys when it is in the alternate screen;
  local scrollback otherwise. Shift+Wheel always scrolls locally.
- While scrolled, new output must not move the view (content-anchored);
  a visible indicator shows the scrolled state; typing snaps to live.
- Click-drag selection with highlight; release copies to the system
  clipboard (iTerm2 copy-on-select); Ctrl+Shift+C explicit copy;
  Ctrl+Shift+V pastes into the attached agent with bracketed paste when
  the agent enabled it.
- Ctrl+Shift+F incremental search over screen + scrollback with match
  highlighting and next/previous navigation (plain Ctrl+F also opens it
  in Control mode, where no keys are forwarded).
- Plain keys and plain Ctrl+letters that v1 forwards keep reaching the
  agent unchanged. The only chords agent-mux now intercepts in Attached
  mode are the `Ctrl+Shift+C/V/F` family and `Shift`+navigation keys —
  the same set every modern terminal (Ghostty, Windows Terminal)
  reserves for itself — plus mouse events. (In v1, crossterm's SHIFT
  modifier was ignored, so these chords used to reach the agent as
  their plain control bytes; that duplication is what gets repurposed.)
- Cross-platform via existing crates; Windows 11 ConPTY remains the
  primary tested target.

## Stack additions

| Concern | Choice | Why |
|---|---|---|
| Clipboard | `arboard` | Cross-platform (Windows/mac/X11/Wayland), no service dependency, maintained. |
| Everything else | existing vt100 / tui-term / crossterm / ratatui | vt100 already stores 1000 scrollback lines per session and exposes `set_scrollback`, `alternate_screen`, `mouse_protocol_mode/encoding`, `bracketed_paste`; crossterm captures mouse events; highlights are a ratatui buffer post-pass. |

## Architecture

### New components

- `selection.rs` — pure selection model. Coordinates are **grid-absolute**
  (`abs_row = scrollback_len - scroll_offset + visual_row`), so a
  selection survives scrolling. Anchor/head, normalization
  (start ≤ end regardless of drag direction), containment test used by
  the render pass, and text extraction from `vt100::Screen` cells
  (wide-char aware; per-row trailing whitespace trimmed; rows joined
  with `\n`).
- `search.rs` — search state machine: query string, match list
  (grid-absolute row + column range), current index. Matching is
  case-insensitive substring over each row's text, screen + full
  scrollback. Extraction walks history via `set_scrollback` through a
  helper that always restores the previous offset. Incremental: matches
  recomputed on every query edit; recomputed on new output only while
  the bar is open.
- `mouse.rs` — encodes crossterm mouse events (press/drag/release/wheel,
  with position translated to pane-local 1-based coordinates) into the
  child's negotiated protocol per vt100's reported
  `mouse_protocol_mode()` and `mouse_protocol_encoding()` (SGR 1006
  preferred; X10/UTF-8 encodings per report). Pure function:
  `encode_mouse(event, mode, encoding) -> Option<Vec<u8>>`; `None` when
  the child never asked for mouse events.

### Modified components

- `session.rs` — scroll anchoring: `process_output` records
  `scrollback_len` before/after `parser.process`; if the view is
  scrolled (`offset > 0`), the offset grows by the delta (capped at
  `scrollback_len`), keeping the view content-anchored.
  New helpers: `scroll_by(delta: i32)`, `scroll_to_bottom()`,
  `scroll_to_top()`, `scrolled() -> usize` (all thin wrappers over
  `screen_mut().set_scrollback`).
- `app.rs` — new input routing:
  - `AppEvent::Mouse` handling: wheel routing rule, selection
    lifecycle (down → anchor, drag → head, release → copy-on-select,
    plain click → clear), Shift+Wheel override.
  - New chords: Shift+PgUp/PgDn (page), Shift+Home/End (top/bottom),
    Ctrl+Shift+C (copy), Ctrl+Shift+V (paste), Ctrl+Shift+F (search
    bar; plain Ctrl+F additionally opens it in Control mode only).
    Handled BEFORE attached-mode forwarding, so they are never sent to
    the agent — while plain Ctrl+F/Ctrl+C/Ctrl+V without Shift still
    forward exactly as in v1. Any other forwarded key while scrolled calls
    `scroll_to_bottom()` first, then forwards.
  - Search submode: while the bar is open, printable keys/Backspace edit
    the query, Enter → next match, Shift+Enter → previous, Esc closes
    and snaps to live. Navigating scrolls the view so the current match
    is visible.
  - Clipboard via `arboard::Clipboard`, created lazily per use; failure
    → status-bar error, never a panic.
  - Paste: clipboard text written to the attached session; wrapped in
    `ESC[200~ … ESC[201~` iff `screen.bracketed_paste()`; CR/LF
    normalized to CR when not bracketed.
- `ui.rs` — post-render passes over the ratatui buffer, in order:
  selection highlight (REVERSED) on cells inside the selection,
  search-match highlight (current match visually distinct from other
  matches), scroll indicator appended to the pane title
  (`[SCROLL ↑ N]`), search bar drawn over the status-bar line when open.
  A shared helper maps grid-absolute coordinates ↔ visible buffer cells
  given the current scroll offset and pane origin.
- `events.rs` — add `Mouse(crossterm::event::MouseEvent)`.
- `main.rs` — `EnableMouseCapture` on entry; `DisableMouseCapture` in
  the guard's restore path and the panic hook; forward mouse events into
  the channel. Mouse events outside the main pane are dropped (v1
  sidebar stays keyboard-driven).

### Wheel routing rule (normative)

For a wheel event over the main pane, in order:
1. Shift held → scroll local scrollback (3 lines per tick).
2. Child's `mouse_protocol_mode()` reports wheel/press interest →
   encode via `mouse.rs`, forward to PTY.
3. Child in `alternate_screen()` → send 3× arrow up/down key sequences.
4. Otherwise → scroll local scrollback (3 lines per tick).

Rules 2–3 apply only in Attached mode; in Control-mode preview, wheel
always scrolls locally.

## Keybindings (additions)

| Context | Input | Action |
|---|---|---|
| Main pane (Attached + Control) | Wheel | Routed per the wheel rule above |
| Main pane (Attached + Control) | Shift+Wheel | Scroll local scrollback |
| Attached + Control | Shift+PgUp / Shift+PgDn | Page up / down |
| Attached + Control | Shift+Home / Shift+End | Jump to top / live bottom |
| Attached (scrolled) | any forwarded key | Snap to live, then forward |
| Main pane | Mouse down / drag / release | Select; release copies to clipboard |
| Main pane | Plain click | Clear selection |
| Attached + Control | Ctrl+Shift+C | Copy selection |
| Attached | Ctrl+Shift+V | Paste clipboard (bracketed when supported) |
| Attached + Control | Ctrl+Shift+F | Open search bar |
| Control only | Ctrl+F | Open search bar (Attached forwards plain Ctrl+F to the agent) |
| Search bar | printable / Backspace | Edit query (incremental) |
| Search bar | Enter / Shift+Enter | Next / previous match |
| Search bar | Esc | Close search, snap to live |

All v1 bindings are unchanged. Mouse events are forwarded to agents only
under rule 2, so agents that use the mouse (if any) keep working.

## Error handling

- Clipboard init/read/write failure → status-bar error; selection stays.
- `encode_mouse` returning `None` → event silently dropped (agent didn't
  ask for mouse).
- Search over an empty scrollback → "no matches" indicator in the bar.
- Mouse capture is always released on exit via the existing Drop guard +
  panic hook (`DisableMouseCapture` added to `restore_terminal`).

## Testing

- Unit: wheel-routing decision function (all 4 rules × modes), selection
  model (normalization, containment, extraction incl. wide chars and
  drag-upward), search matching + navigation order, scroll anchoring math,
  `encode_mouse` for SGR encoding and `None` cases, paste bracketing.
- TestBackend: selection highlight cells, search bar + match highlight,
  scroll indicator in the title.
- Integration (real `cmd.exe`/`sh`, headless): produce >2 screens of
  output; scroll up and assert old content visible; keep producing output
  and assert the view stays anchored; snap-to-bottom on a forwarded key;
  search finds a known line and scrolls to it.
- Clipboard round-trip test `#[ignore]`d (mutates the real system
  clipboard); selection *extraction* is covered without the clipboard.
- Manual smoke: wheel + selection + search against real `claude`/`codex`
  profiles, including their full-screen alternate-screen UIs.

## Out of scope

- Sidebar mouse interaction (click-to-select sessions) — v-next.
- Regex or case-sensitive search modes.
- Selection modes beyond linear (no block/rectangular selection).
- Configurable keybindings and scroll speed.
- URL detection / clickable links.
