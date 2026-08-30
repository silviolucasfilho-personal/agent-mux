# agent-mux Historical Session Log Viewer

**Date:** 2026-08-30
**Status:** Approved design, pre-implementation
**Builds on:** v1 (`2026-08-29-agent-mux-design.md`) and terminal UX (`2026-08-30-terminal-ux-design.md`)

## Purpose

Allow users to browse, search, and view detailed transcripts and logs from past Claude Code sessions directly inside `agent-mux` without leaving the TUI, with the ability to instantly resume any past session.

## Requirements

1. **Discovery**: Automatically discover past Claude Code session transcripts stored on disk under `~/.claude/projects/<project-slug>/<session-id>.jsonl`.
2. **Filtering**: Default to showing sessions for the current working directory / selected profile directory, with a quick toggle (`a`) to view all past sessions across all projects.
3. **Interactive Side-by-Side UX**:
   - Left pane: Past Sessions list showing timestamp, AI session title (or first prompt preview), turn count, and session ID.
   - Right pane: Structured log viewer showing the full conversation timeline (User turns, Assistant responses/thoughts, Tool calls with inputs & execution outputs, errors, token stats).
4. **Resumption**: Pressing `r` (or `Enter` from the session list) immediately launches and attaches to `claude --resume <session-id>` in a new session tab.
5. **Navigation & Scrolling**:
   - `Tab` / `BackTab` to toggle focus between the session list and the log pane.
   - `j`/`k` or `↑`/`↓` to select sessions or scroll logs.
   - `PageUp`/`PageDown`, `Home`/`End`, and mouse wheel for smooth scrolling through long session transcripts.
   - `Esc` / `q` closes the viewer and returns to `Control` mode.
6. **Status Bar & Keybinding**:
   - `[l]` in `Control` mode opens the Session History modal.
   - Status bar updated in `Control` mode to include `[l] logs`.

## Stack & Architecture

### Components

- `src/history.rs`:
  - `project_slug(path: &Path) -> String`: Converts a filesystem path into Claude Code's project directory slug format (e.g. `/home/user/workspace/app` -> `-home-user-workspace-app`).
  - `discover_sessions(claude_dir: Option<&Path>, current_dir: Option<&Path>, all_projects: bool) -> Vec<SessionSummary>`: Scans `~/.claude/projects/` for `.jsonl` files and extracts high-level metadata (id, title, timestamp, modified time, turn count, tokens).
  - `load_session_log(path: &Path) -> Result<Vec<LogEntry>>`: Parses JSONL session events into structured log entries.
  - `render_log_lines(entries: &[LogEntry], width: usize) -> Vec<LogLine>`: Formats log entries into wrapped, styled Ratatui lines with visual hierarchy and tool output blocks.
- `src/app.rs`:
  - `Mode::SessionHistory(HistoryState)` added to `Mode`.
  - `HistoryState` maintains the list of summaries, selected index, loaded log lines, scroll offset, and focused pane.
  - Key dispatch: `l` in Control mode triggers `Mode::SessionHistory`. Handles modal navigation, scrolling, and resume action.
- `src/ui.rs`:
  - `draw_session_history(f: &mut Frame, state: &HistoryState, app: &App)`: Renders the split modal dialog.
  - Updates `draw_status_bar` to display `[l] logs`.

## Keybindings (Session History Mode)

| Context | Key | Action |
|---|---|---|
| Control mode | `l` | Open Session History / Log Viewer |
| History Modal | `Tab` / `BackTab` | Switch focus between Session List and Log Detail |
| Session List | `j`/`k` or `↑`/`↓` | Select previous / next past session |
| Session List | `r` or `Enter` | Resume selected session (`claude --resume <id>`) |
| Session List | `a` | Toggle current project vs. all projects |
| Log Detail | `j`/`k` or `↑`/`↓` | Scroll log by 1 line |
| Log Detail | `PgUp` / `PgDn` | Scroll log by page |
| Log Detail | `Home` / `End` | Jump to start / end of log |
| Log Detail | Mouse Wheel | Scroll log |
| History Modal | `Esc` / `q` | Close History Viewer |
