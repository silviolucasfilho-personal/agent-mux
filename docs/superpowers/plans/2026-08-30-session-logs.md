# agent-mux Session Logs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Historical session log viewer for Claude Code sessions with interactive split-pane UI, timeline formatting (user, assistant, tool use/output), search/scrolling, and instant session resumption (`claude --resume <id>`).

**Architecture:** A new `history.rs` module discovers and parses `.jsonl` session files from `~/.claude/projects/`. In `app.rs`, `Mode::SessionHistory(HistoryState)` manages the state machine (active sessions list, selected session, loaded log lines, scroll position, focus). In `ui.rs`, `draw_session_history` renders a split modal (session list on left, formatted conversation on right).

**Tech Stack:** `ratatui 0.30`, `serde_json 1.0`, `crossterm 0.29`.

**Spec:** `docs/superpowers/specs/2026-08-30-session-logs-design.md`

---

### Task 1: History Discovery and Parser Module (`src/history.rs`)

**Files:**
- Create: `src/history.rs`
- Modify: `src/lib.rs`
- Test: Unit tests in `src/history.rs`

**Interfaces:**
- `project_slug(path: &Path) -> String`
- `SessionSummary`: `session_id: String`, `title: String`, `modified: SystemTime`, `file_path: PathBuf`, `turn_count: usize`, `project_slug: String`
- `LogEntry`:
  - `User { text: String, timestamp: Option<String> }`
  - `Assistant { text: String, model: Option<String>, thinking: Option<String>, timestamp: Option<String> }`
  - `ToolUse { id: String, name: String, input: String, timestamp: Option<String> }`
  - `ToolResult { id: String, content: String, is_error: bool, timestamp: Option<String> }`
- `discover_sessions(claude_dir: Option<&Path>, current_dir: Option<&Path>, all_projects: bool) -> Vec<SessionSummary>`
- `load_session_log(file_path: &Path) -> std::io::Result<Vec<LogEntry>>`
- `render_log_lines(entries: &[LogEntry]) -> Vec<ratatui::text::Line<'static>>`

- [ ] **Step 1: Write failing unit tests for slugification, discovery, and parser in `src/history.rs`**
- [ ] **Step 2: Implement `src/history.rs`**
- [ ] **Step 3: Register `pub mod history;` in `src/lib.rs` and verify all tests pass**

---

### Task 2: History State Machine and Key Handling (`src/app.rs`)

**Files:**
- Modify: `src/app.rs`
- Test: Unit tests in `src/app.rs`

**Interfaces:**
- `HistoryPane` enum: `SessionsList`, `LogDetail`
- `HistoryState` struct:
  - `sessions: Vec<SessionSummary>`
  - `selected_session_idx: usize`
  - `log_lines: Vec<Line<'static>>`
  - `scroll_offset: usize`
  - `focused_pane: HistoryPane`
  - `all_projects: bool`
  - `error: Option<String>`
- `Mode::SessionHistory(HistoryState)`
- Key routing:
  - `Control` mode: `l` / `L` -> transitions to `Mode::SessionHistory`
  - `SessionHistory` mode:
    - `Tab` / `BackTab` / `Left` / `Right`: switch `focused_pane`
    - `j`/`k` / `Up`/`Down`: move selection (if `SessionsList`) or scroll (if `LogDetail`)
    - `PgUp`/`PgDn`: scroll page in `LogDetail`
    - `Home`/`End`: top / bottom
    - `a`: toggle `all_projects` and reload sessions
    - `r` / `Enter`: when on `SessionsList`, spawns new `Session` with `claude --resume <id>`
    - `Esc` / `q`: cancel to `Control` mode

- [ ] **Step 1: Write failing unit tests for `HistoryState` key handling and dispatch**
- [ ] **Step 2: Implement `HistoryState` and integration into `App`**
- [ ] **Step 3: Verify all tests pass**

---

### Task 3: Split Modal UI Rendering (`src/ui.rs`)

**Files:**
- Modify: `src/ui.rs`
- Test: `ui::tests` for `draw_session_history`

**Interfaces:**
- `draw_session_history(f: &mut Frame, state: &HistoryState, app: &App)`
- Update `draw_status_bar` to include `[l] logs` hint in Control mode
- Split view rendering:
  - Outer block: "Past Sessions & Logs"
  - Left pane: list of sessions with timestamps and AI titles, highlighted selection
  - Right pane: formatted conversation lines with `[SCROLL ↑ N]` or line count
  - Bottom hints line

- [ ] **Step 1: Write failing UI test in `src/ui.rs`**
- [ ] **Step 2: Implement `draw_session_history` and status bar hint**
- [ ] **Step 3: Verify UI tests pass**

---

### Task 4: Integration Test Suite (`tests/session_history.rs`)

**Files:**
- Create: `tests/session_history.rs`

- [ ] **Step 1: Write integration tests with mock Claude `.jsonl` files**
  - Verify discovery from mock `~/.claude/projects/`
  - Verify switching between session list and log view
  - Verify scrolling and toggle all projects
  - Verify resume action launches session with `--resume` args
- [ ] **Step 2: Run `cargo test` and ensure zero warnings or errors**
