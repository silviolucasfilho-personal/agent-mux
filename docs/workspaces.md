# Workspaces (`agent-mux workspace …` · F1 → "Open workspaces")

The **Workspaces** screen (`Mode::Workspaces`, `src/workspace/`, `src/app/workspaces_view.rs`, `src/ui/workspaces.rs`) scans the folder tree starting from the directory `agent-mux` was launched in, stopping at each discovered project folder (so a project's internal subdirectories are not explored), inspects the technology stack and documentation of each folder, rolls up agent activity across each folder and its sub-folders, and lets you open or resume an LLM agent directly in any folder.

## Opening the screen

- **TUI**: press `F1` (`⌘P` / `Ctrl+Shift+P`) and choose **Open workspaces**.
- **CLI**:
  - `agent-mux workspace ls [DIR] [--depth N] [--json]`
  - `agent-mux workspace show [DIR] [--json]`
  - `agent-mux workspace summarize [DIR] [--harness claude|codex|agy]`

## What is inspected per folder

1. **Technology stack (`workspace::scan::detect_tech_stack`)**:
   - **Rust**: `Cargo.toml` (package/workspace name, edition, description, `[workspace.members]`, notable crates like `ratatui`, `tokio`, `rusqlite`, `axum`, `tauri`).
   - **TypeScript / JavaScript**: `package.json`, `tsconfig.json`, lockfiles (`pnpm`, `Bun`, `Yarn`, `npm`, `Deno`), and frameworks (`Next.js`, `React`, `Vue`, `Svelte`, `Vite`, `Astro`, `Express`, `NestJS`, `Tailwind`, `Prisma`, `Drizzle`).
   - **Python**: `pyproject.toml`, `requirements.txt`, `setup.py`, `uv.lock`, and frameworks (`FastAPI`, `Django`, `Flask`, `PyTorch`, `LangChain`, `Pydantic`).
   - **Go / JVM / Ruby / Elixir / PHP / Swift / C++**: `go.mod`, `go.work`, `pom.xml`, `build.gradle`, `Gemfile`, `mix.exs`, `composer.json`, `Package.swift`, `CMakeLists.txt`.
   - **Tooling**: `Dockerfile`, `docker-compose.yml`, `flake.nix`, `justfile`, `Makefile`.
2. **Project summary (`workspace::scan::build_summary`)**:
   - Uses a cached LLM summary (`~/.agent-mux/workspaces/<sha256>.json`) when available, else the manifest description, else the first prose paragraph of `README.md`, `AGENTS.md`, or `CLAUDE.md`, else a synthesized stack/sub-project summary.
   - Press `s` on any folder in the TUI (or run `agent-mux workspace summarize <dir>`) to run a read-only headless agent that writes a structured summary (`purpose`, `architecture`, `entry_points`) into `~/.agent-mux/workspaces/`.
3. **Agent activity & sub-folder roll-up (`workspace::activity`)**:
   - **Live sessions**: active/idle sessions from `App::sessions` running in the folder or any of its sub-folders (`[in sub/dir]`).
   - **Scheduled loops**: loops from `agents::schedule::load_registry` targeting the folder or its sub-folders, with their latest outcome from `loop_runs`.
   - **Project personas**: `.agent-mux/agents/*.toml` defined in the folder.
   - **Recent resumable sessions & 30-day roll-up**: recent sessions from `traces.db` and `history::discover_sessions`, with 30-day session count, token total, USD cost, and per-harness breakdown rolled up across sub-folders.

## Keys in `Mode::Workspaces`

| Key | Action |
| --- | --- |
| `↑` / `↓` (`j` / `k`, `g` / `G`) | Move selection in the focused pane |
| `Tab` / `Shift+Tab` | Switch focus between the **Folders** tree (left) and **Agents in this folder** list (right) |
| `Enter` | On a folder: launch a new agent session immediately in that folder with the default harness. On an agent item in the right pane: attach to a live session, open loop runs, launch a persona, or resume a past session |
| `r` | Resume the most recent past session in the selected folder (or run/resume the selected item in the right pane) |
| `n` | Open the New Session dialog pre-filled with the selected folder's path |
| `e` | Open the folder's `AGENTS.md`, `CLAUDE.md` or `README.md` in the Markdown viewer (or edit a selected loop/persona in the right pane) |
| `s` | Generate or refresh the folder's cached LLM summary in the background |
| `/` | Filter folders by name, technology, framework, or agent |
| `→` / `←` | Step the workspace root down into the selected sub-folder (`→`) or up to the parent directory (`←`) |
| `Ctrl+R` | Rescan the filesystem and refresh agent activity |
| `Esc` / `q` | Back / close |
