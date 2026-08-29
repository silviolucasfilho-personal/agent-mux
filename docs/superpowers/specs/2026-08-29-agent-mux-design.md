# agent-mux — Rust TUI fleet manager for Claude Code & Codex

**Date:** 2026-08-29
**Status:** Approved design, pre-implementation

## Purpose

A tmux-like terminal multiplexer purpose-built for coding agents. It runs
multiple concurrent `claude` and `codex` CLI sessions inside embedded
pseudo-terminals, shows at a glance which sessions are working, waiting for
input, or finished, and lets the user attach to any session with the agent's
full native UI intact.

The agents' own TUIs are rendered verbatim — this tool adds session
management around them, it does not reimplement their chat UI.

## Requirements

- Run 3–10 concurrent agent sessions, each in its own PTY.
- Sidebar listing all sessions with a live status badge per session.
- Attach to a session: its virtual screen fills the main pane and keyboard
  input is forwarded raw to the agent. Detach back to the sidebar with a
  prefix key.
- Background (non-focused) sessions keep consuming PTY output into an
  up-to-date virtual screen, so attaching is instant and lossless.
- New-session flow: pick an agent profile and a working directory.
- Windows 11 (ConPTY) is the primary tested target; all chosen crates are
  cross-platform so Linux/macOS support requires no redesign.
- Sessions run in plain directories chosen by the user. No git or worktree
  automation in v1.

## Stack

| Concern | Choice | Why |
|---|---|---|
| TUI framework | `ratatui` + `crossterm` | Standard Rust pair; immediate-mode rendering; solid Windows support. |
| PTY | `portable-pty` (WezTerm) | ConPTY on Windows, openpty on Unix, one API. Agents believe they are in a real terminal. |
| Terminal emulation | `tui-term` + `vt100` | `tui-term` renders a `vt100::Parser` screen as a ratatui widget. One parser per session = per-session virtual screen. |
| Async / events | `tokio` + `mpsc` | One blocking reader thread per PTY feeding a channel; a single event loop merges PTY output, keyboard input, and a tick timer. |
| Config | `serde` + `toml` | Small profile file, no ceremony. |

## Architecture

### Components

- `main.rs` — terminal setup/teardown (raw mode, alternate screen), runs the
  event loop, guarantees terminal restore on panic.
- `app.rs` — `App` state: `Vec<Session>`, focused index, `Mode`
  (`Control` | `Attached` | `NewSession` dialog), action dispatch.
- `session.rs` — `Session`: child process handle, PTY master writer, reader
  thread handle, `vt100::Parser` (screen + scrollback), `Status`, profile
  name, working directory.
- `status.rs` — status heuristic (see below), updated on every PTY output
  event and tick.
- `ui.rs` — pure draw functions: sidebar, main pane (`tui-term`
  `PseudoTerminal` widget over the focused session's screen), status bar,
  new-session dialog.
- `config.rs` — loads `profiles.toml`; profile = display name + command +
  args + optional default directory.

### Data flow

```
keyboard ──crossterm──►┐
PTY #1 out ──thread──► │  mpsc ──► event loop ──► App::handle ──► draw
PTY #n out ──thread──► │
tick (250ms) ─────────►┘
```

- Each session spawns one blocking reader thread that forwards raw bytes to
  the shared channel tagged with the session id. The event loop feeds bytes
  into that session's `vt100::Parser`.
- In `Attached` mode, key events are encoded to bytes and written to the
  focused session's PTY writer. `Ctrl+Q` switches back to `Control` mode
  (chosen because neither CLI binds it; a double `Ctrl+Q` sends a literal
  one through).
- Pane resize (terminal resize or layout change) calls `pty.resize()` with
  the new dimensions and resizes the parser; the child CLI reflows itself.

### Session lifecycle

1. `NewSession` dialog: choose profile, choose working directory (free-text
   path with the profile's default pre-filled).
2. Spawn via `portable_pty::CommandBuilder` with `cwd` set. On Windows,
   npm-installed CLIs are `.cmd` shims: if the command does not resolve to a
   native `.exe`, spawn through `cmd /c <command>`.
3. Running: reader thread pumps output; status heuristic updates.
4. Child exit: session marked `Exited(code)`; screen remains viewable;
   `r` respawns with the same profile/directory, `x` removes the session.
5. App quit: all children are killed (confirm dialog if any session is
   `Working`). No detach/persist in v1.

## Status detection

A PTY stream has no structured events, so v1 uses two cheap signals:

- **Activity:** output within the last ~2 s → `Working` (spinners repaint
  constantly). Quiet beyond that → `Idle`.
- **BEL (0x07):** both CLIs ring the terminal bell on completion or when
  approval is needed. A BEL on a non-focused session sets a `NeedsAttention`
  badge that clears when the session is next attached.

Statuses: `Working` | `Idle` | `NeedsAttention` | `Exited(code)`.
Thresholds live in one place (`status.rs`) and are tunable via config later
if the defaults misbehave.

## Keybindings

| Mode | Key | Action |
|---|---|---|
| Control | `j`/`k` or arrows | Move selection in sidebar |
| Control | `Enter` | Attach to selected session |
| Control | `n` | New session dialog |
| Control | `x` | Kill selected running session (confirm); remove if `Exited` |
| Control | `r` | Respawn exited session |
| Control | `q` | Quit (confirm if any `Working`) |
| Attached | `Ctrl+Q` | Detach to Control mode |
| Attached | `Ctrl+Q Ctrl+Q` | Send literal `Ctrl+Q` to the agent |
| Attached | anything else | Forwarded raw to the PTY |

## Error handling

- Reader thread error / EOF → session degrades to `Exited`; never panics the
  app.
- PTY write failure while attached → status-bar error, session marked
  `Exited`.
- Spawn failure (bad command, bad directory) → error surfaced in the
  new-session dialog; no session created.
- Terminal is always restored (raw mode off, alternate screen left) via a
  panic hook + `Drop` guard.

## Testing

- Unit: status heuristic (feed synthetic output/BEL timelines), keybinding
  dispatch, config parsing.
- Integration: spawn `cmd.exe` / `pwsh` (not the real agents) under
  `portable-pty`; drive spawn → output → parser screen assertions → input
  → resize → exit. Runs headless; no ratatui terminal needed (assert on
  `vt100` screen contents).
- Real-agent smoke test stays manual: launch `claude` and `codex` profiles
  and verify rendering, input, resize, bell.

## Out of scope (v2 candidates)

- Git worktree/branch per session (claude-squad style).
- Detach/persist across app restarts — no tmux-style daemon on Windows;
  pragmatic bridge is a `claude --resume` profile.
- Desktop notifications on `NeedsAttention`.
- A custom unified chat UI over `claude -p --output-format stream-json` /
  `codex exec --json` — deliberately a different tool; this one embeds the
  native UIs.
