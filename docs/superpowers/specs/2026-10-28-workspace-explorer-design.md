# Workspace Explorer (`Workspaces`) — design

Status: draft, 2026-10-28.

## 1. Why

When `agent-mux` starts in a parent directory (for example `~/workspace` or a monorepo root), the user has many project folders and sub-folders under that directory. Today, starting a session in a sub-folder requires opening the New Session dialog and navigating the directory picker, and seeing which agents, sessions or scheduled loops work in each folder requires cross-referencing the sidebar, History, Runs and Traces by hand.

The **Workspaces** screen (`Mode::Workspaces`, `ActionId::Workspaces`, CLI `agent-mux workspace …`) scans the workspace tree rooted at the launch directory (or any chosen root), inspects each folder and sub-folder for its technology stack, documentation summary and git state, rolls up all agent activity (live sessions, resumable past sessions, scheduled loops, workflows and project-level agent definitions) per folder and across its sub-folders, and lets the user launch a new agent or resume the latest agent directly on any folder.

## 2. Discovery & Tree Model

### Root & bounded scan

- **Default root**: the directory `agent-mux` was started in (`App::default_dir` / `std::env::current_dir()`), canonicalized.
- **Root navigation**: the user can step into any sub-folder as the new root (`→` / `l`), step up to the parent directory (`←` / `h` or `..`), or filter the tree with `/`.
- **Scan rules**:
  - Depth: up to `SCAN_DEPTH = 3` levels below the current root by default.
  - Skipped directories: hidden directories (`.git`, `.claude`, `.codex`, `.gemini`, `.agent-mux`, `.venv`, `.next`, `.turbo`, etc.) and build/vendor artifacts (`node_modules`, `target`, `dist`, `build`, `__pycache__`, `venv`, `vendor`, `.gradle`, `Pods`). Note: hidden harness/config folders inside a candidate folder *are* inspected as markers for that folder, but are never listed as workspace rows of their own.
  - Visit cap: at most `SCAN_MAX_VISITED = 4_000` directories per scan so opening on `~` or `/` stays under 50 ms.
- **Row inclusion & ordering**:
  - The root folder itself is row `0`.
  - Direct children and nested sub-folders that either (a) contain at least one project marker, (b) have recorded agent activity in `traces.db` / `loops.json`, or (c) are direct (depth-1) children of the root are shown in tree order (`root`, then alphabetical tree pre-order with indentation).
  - Purely intermediate folders with no markers produce a collapsed path (`services/payments`) when only one child is a project, or remain expandable nodes when multiple sub-projects live underneath.

## 3. Technology Stack & Project Summary

### 3.1 Instant deterministic inspection (`workspace::inspect`)

Every discovered folder is inspected synchronously in Rust without spawning processes (except an optional bounded `git` read for branch/dirty state on the selected row):

| Signal | Files inspected | Extracted facts |
| --- | --- | --- |
| **Rust** | `Cargo.toml`, `rust-toolchain.toml` | crate/workspace name, edition, `[workspace.members]`, key dependencies (e.g. `ratatui`, `tokio`, `rusqlite`, `axum`) |
| **Node / TS / JS** | `package.json`, `tsconfig.json`, `pnpm-workspace.yaml`, `bun.lockb`, `deno.json` | package name, description, runtime/package manager, framework tags (`next`, `react`, `vite`, `astro`, `express`, `nestjs`) |
| **Python** | `pyproject.toml`, `requirements.txt`, `setup.py`, `uv.lock` | project name, description, Python version, key libraries (`fastapi`, `django`, `pytorch`, `langchain`) |
| **Go** | `go.mod`, `go.work` | module path, Go version |
| **Java / Kotlin / JVM** | `pom.xml`, `build.gradle`, `build.gradle.kts` | build tool (`Maven`, `Gradle`), Kotlin/Java |
| **Ruby / Elixir / PHP / .NET / C++ / Swift** | `Gemfile`, `mix.exs`, `composer.json`, `*.csproj`, `CMakeLists.txt`, `Package.swift` | language and ecosystem badge |
| **Infra & Containers** | `Dockerfile`, `docker-compose.yml`, `flake.nix`, `Makefile`, `justfile`, `*.tf` | tooling badges (`Docker`, `Nix`, `Terraform`, `Make`, `Just`) |
| **Git** | `.git` (file or directory) | repo root vs worktree vs subfolder inside repo; branch & dirty file count (read lazily for visible/selected row with a 150 ms timeout) |

### 3.2 Deterministic text summary

Before any LLM call, each folder gets an immediate human-readable summary prioritized from:

1. **Cached LLM summary** (`~/.agent-mux/workspaces/<path-hash>.json`) if one was previously generated, showing a `(cached · Ctrl+R to refresh)` badge and whether the folder's manifest/README mtime changed since generation.
2. **Manifest description**: `package.json` `description`, `Cargo.toml` `[package] description`, or `pyproject.toml` `[project] description`.
3. **Documentation lead paragraph**: first non-heading, non-badge paragraph from `README.md`, `AGENTS.md`, or `CLAUDE.md` (stripped of Markdown syntax, capped at ~280 chars).
4. **Fallback**: synthesized from detected stack and sub-folder count (e.g. *"Rust Cargo workspace with 4 crates; uses tokio, ratatui, rusqlite"* or *"Parent folder containing 6 sub-projects (3 TypeScript, 2 Rust, 1 Python)"*).

### 3.3 On-demand LLM summary enrichment

Pressing `g` (or choosing **Summarize folder with agent** from the screen) spawns a headless print-mode run via `spawn_traced_full` on the user's default harness (read-only tools) that inspects the folder's manifests, directory structure and `README.md`/`AGENTS.md` and writes a structured summary into `~/.agent-mux/workspaces/<path-hash>.json`:

```json
{
  "path": "/Users/user/workspace/agent-mux",
  "generated_at": "2026-10-28T11:05:00Z",
  "harness": "claude",
  "purpose": "Terminal multiplexer for AI coding harnesses with a local SQLite trace store and Loop/Workflow runtimes.",
  "architecture": "Single Rust binary (ratatui TUI + rusqlite WAL store + MCP server); skills, loops, workflows and agents compiled in and shadowed by ~/.agent-mux/.",
  "entry_points": ["src/main.rs", "src/app.rs", "src/tracing/store/mod.rs"],
  "subprojects": []
}
```

## 4. Agent Resume & Roll-up per Folder and Sub-folder

For each folder `F`, `workspace::activity` computes both **Direct** stats (cwd == `F`) and **Subtree roll-up** stats (`cwd == F` or `cwd` starts with `F/`):

1. **Live sessions (`App::sessions`)**:
   - Count and list of currently running or idle sessions in `F` (and rolled up from `F/*`), showing harness, profile/agent name, status (`● running`, `◦ idle`, `! waiting`), and current turn title.
2. **Resumable past sessions (`traces.db` `sessions` + `history::discover_sessions`)**:
   - Most recent sessions that ran in `F` or its sub-folders: harness (`claude`, `codex`, `agy`), relative time (`2h ago`), title/first prompt, turn count, token total and USD cost.
   - Tracks the latest resumable session for `F` so pressing `r` immediately resumes it in `F` (`harness::Resume::Session(id)` for Claude/Codex or transcript handoff for Antigravity).
3. **30-day telemetry roll-up (`traces.db`)**:
   - Total sessions, total tokens, total cost USD, and last active timestamp across `F` and its sub-folders.
   - Breakdown by harness (`claude: 14 · codex: 5 · agy: 3`).
4. **Configured agents, loops & harness wiring**:
   - **Scheduled loops (`agents::schedule::load_registry`)**: any loop whose `workspace` is `F` or inside `F/*`, with its pattern/task, schedule (`⟳ 1d`), last outcome (`✓ no-op`, `! fix-proposed`, `✗ failed`), and readiness score if cached.
   - **Workflow runs (`workflow_runs`)**: recent workflow runs targeting `F`.
   - **Project-local customization markers**: presence of `AGENTS.md`, `CLAUDE.md`, `.agent-mux/agents/*.toml` (listing the persona names), `.claude/skills/`, `.codex/skills/`, `.gemini/`, `gate.yaml`, `loop.md`.

## 5. UI & Layout (`src/app/workspaces_view.rs`, `src/ui/workspaces.rs`)

###Two-pane split screen

```text
┌─ Workspaces — ~/workspace (8 projects, 3 active sessions) ──────────────────────┐
│ Folders (/ filter, ←/→ root)   │ agent-mux  ~/workspace/agent-mux        [git: main *2] │
│ ▾ ~/workspace       [3● 42 sess]│ Stack   Rust 2024 · Cargo · ratatui · tokio · sqlite  │
│   ▸ ai-memory       Go · SQLite│ Config  AGENTS.md · CLAUDE.md · 5 built-in agents     │
│ ▸ ▾ agent-mux  [2●] Rust · TUI │                                                       │
│     ├─ loops        Skills/MD  │ Summary                                               │
│     └─ workflows    TOML/Skills│ `agent-mux` is a terminal multiplexer for AI coding   │
│   ▸ shareable       TS · Next  │ harnesses (Claude Code, Codex CLI, Google Antigravity)│
│   ▸ lottery-ai      Python     │ with a local SQLite trace store and read-only CLI.    │
│                                │                                                       │
│                                │ Agents in this folder (and 2 sub-folders)             │
│                                │ ● Live (2)                                            │
│                                │   claude  "Add workspace explorer spec"     running   │
│                                │   agy     "Review keymap actions"           idle 4m   │
│                                │ ⟳ Scheduled & Project Agents (2)                      │
│                                │   ci-triage (⟳ 2h, claude)          ✓ report-only 3h  │
│                                │   pr-review-triage (⟳ 1d, codex)    ✓ no-op 14h       │
│                                │ ↺ Recent Sessions (30d: 28 sessions · 1.4M tok · $6.20)│
│                                │   1. 2h ago  claude  "Fix memory feed cursor"  (12t)  │
│                                │   2. 1d ago  codex   "Refactor dir_picker"     (8t)   │
│                                │   3. 2d ago  agy     "Update markdown mermaid" (5t)   │
├─ Enter new agent · r resume latest · n pick agent/harness · / filter · Ctrl+R rescan ──┤
└────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Left pane (Workspace Tree)**:
  - Shows folder hierarchy relative to the current root.
  - Compact badges per row: live session dot (`●`), primary language/tech tag (`Rust`, `TS`, `Py`, `Go`), and roll-up indicator when sub-folders have active/recent agents.
- **Right pane (Folder Dossier)**:
  - **Header**: folder name, path, git branch + dirty status, and Direct vs Sub-folder roll-up toggle/counts.
  - **Technology & Customization bar**: detected languages, frameworks, build tools, and agent config badges (`AGENTS.md`, `.agent-mux/agents`, `.claude`, `.codex`, `.gemini`, `gate.yaml`).
  - **Summary block**: deterministic or cached LLM summary; `e` or clicking `README.md`/`AGENTS.md` opens it in `Mode::Markdown`.
  - **Agents working here (Direct + Sub-folders)**:
    1. **Live sessions**: attachable directly (`Enter` when focused on a session row, or `1`–`9` / `Tab` to focus the Agents list in the right pane).
    2. **Scheduled loops & project personas**: shows loop status and local `.agent-mux/agents/*.toml` personas.
    3. **Recent sessions & 30-day usage**: list of recent sessions in this folder and its sub-folders (showing `[in sub-folder]` tag when rolled up from a child), with token/cost roll-up.

## 6. Keyboard & Actions (`docs/keyboard.md` alignment)

All bindings follow `src/keymap.rs` (`VERBS` and `ACTIONS`) without introducing conflicting single-letter verbs:

| Key | Action | Behavior in `Mode::Workspaces` |
| --- | --- | --- |
| `F1` / `⌘P` → **Open workspaces** | `ActionId::Workspaces` | Opens `Mode::Workspaces` rooted at the launch directory (or active session's workspace root) |
| `↑` / `↓` (`g` / `G`, `Ctrl+D` / `Ctrl+U`) | List navigation | Moves selection in the focused pane (Folder tree or Folder's Agent list) |
| `Tab` / `Shift+Tab` | Switch pane focus | Toggles focus between the **Folder Tree** (left) and the **Folder's Agents / Sessions** list (right) |
| `Enter` (`Verb::Open`) | Open / Launch | On a folder: launches a new session in that folder with the default harness (or attaches if an active session is selected in the right pane). |
| `n` (`Verb::New`) | New agent in folder | Opens the New Session / Agent picker pre-filled with the selected folder's path so the user can choose harness, persona or prompt |
| `r` (`Verb::Run`) | Resume latest | Resumes the most recent past session in the selected folder (or the specifically selected session/loop in the right pane) |
| `e` (`Verb::Edit`) | Open docs in Markdown viewer | Opens the folder's `AGENTS.md` or `README.md` in `Mode::Markdown` |
| `Ctrl+R` (`Verb::Reload`) | Rescan & refresh | Re-scans the filesystem, reloads trace store stats, and refreshes the LLM summary if requested |
| `/` | Filter folders | Filters the workspace tree by name, technology tag, or agent name (`Esc` clears filter) |
| `→` / `←` | Change root | `→` makes the selected folder the root of the tree; `←` steps the root up to its parent directory |
| `Esc` / `q` (`Verb::Back`) | Close | Returns to `Mode::Main` (or the originating session) |

## 7. CLI & MCP Surface

To keep parity between the TUI and CLI (like `agent-mux skill`, `agent-mux loop`, `agent-mux memory`):

- `agent-mux workspace ls [path] [--depth N] [--json]`: scans `path` (default `.`), prints the folder tree with detected tech stack, summary excerpt, live/30d session counts, and configured loops/agents.
- `agent-mux workspace show <path> [--json]`: prints the full dossier for one folder and its sub-folders (stack, manifests, git info, local agents, loops, recent sessions, 30d tokens/cost).
- `agent-mux workspace summarize <path> [--harness claude|codex|agy]`: runs the headless read-only summarizer agent and caches the result under `~/.agent-mux/workspaces/`.

## 8. Module Structure & Implementation Plan

1. **`src/workspace/mod.rs`** — Core data model (`WorkspaceNode`, `TechStack`, `ProjectSummary`, `FolderActivity`, `RollupStats`).
2. **`src/workspace/scan.rs`** — Bounded filesystem walk, skip rules, marker detection, and manifest/README parsers (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `README.md`, `AGENTS.md`, git head/status probe).
3. **`src/workspace/activity.rs`** — Read-only queries against `traces.db` (`sessions`, `traces`, `loop_runs`, `workflow_runs`), `history::discover_sessions`, `agents::schedule::load_registry`, and local `.agent-mux/agents/` to compute Direct and Subtree roll-up activity per folder.
4. **`src/workspace/summary_cache.rs`** — Load/save cached LLM summaries in `~/.agent-mux/workspaces/<hash>.json` and headless summarization runner.
5. **`src/app/workspaces_view.rs` & `src/ui/workspaces.rs`** — `Mode::Workspaces(Box<WorkspacesViewState>)`, key handling (`Outcome::LaunchInDir`, `Outcome::NewSessionInDir`, `Outcome::ResumeSession`, `Outcome::AttachSession`, `Outcome::OpenMarkdown`), and two-pane ratatui rendering.
6. **`src/keymap.rs` & `src/app/commands.rs`** — Register `ActionId::Workspaces` ("Open workspaces", keywords `"projects folders stack explorer resume"`), wire availability and footer hints.
7. **`src/workspace/cli.rs`** — `agent-mux workspace ls|show|summarize` subcommands and unit/integration tests using temporary directories and an isolated SQLite trace store.
