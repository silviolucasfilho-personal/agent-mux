# Agent Definitions and Discovery in agent-mux

This document details how autonomous agents are defined, discovered, resolved, and migrated in `agent-mux`.

---

## 1. Canonical Agent Packages

Each agent is defined by a canonical package containing an `AGENTS.md` file in its own subdirectory:

```text
.agent-mux/agents/
└── <agent-id>/
    ├── AGENTS.md
    └── generated/          # Disposable generated artifacts for each harness (claude, codex, agy)
```

> **Note**: Repository-root `AGENTS.md` provides guidance for repository development and is never treated as an executable agent package.

### Package Format (`AGENTS.md`)

An `AGENTS.md` file consists of validated YAML frontmatter enclosed in `---` delimiters followed by Markdown instructions:

```yaml
---
id: heimdall
name: Heimdall
icon: ⚡
description: Briefings and evidence-backed session investigations
harnesses: [claude, codex, agy]
default_harness: agy
capabilities: [trace.read]
startup_task: "Read the current session briefing and report progress, blockers and evidence coverage."
mcp_servers: [agent-mux]
---

# Instructions

You are Heimdall, the omniscient watcher and autonomous monitoring agent of agent-mux.
Call `agent_mux_get_briefing` to inspect recent and active sessions.
Distinguish observed facts from terminal heuristics and cite evidence IDs.
```

### Frontmatter Schema

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `id` | string | No | Lowercase alphanumeric identifier matching `^[a-z0-9][a-z0-9_-]*$`. Defaults to the package directory name. |
| `name` | string | No | Human-readable name. Defaults to capitalized `id`. |
| `icon` | string | No | Emoji or character icon for the sidebar menu. |
| `description` | string | No | One-sentence summary of the agent's role and purpose. |
| `harnesses` | list of string | Yes | Supported AI coding harnesses (`claude`, `codex`, `agy`). Must contain at least one harness. |
| `default_harness` | string | No | Initial harness selected when launching. Must be declared in `harnesses`. Defaults to first harness in list. |
| `capabilities` | list of string | No | Declared capabilities (e.g. `[trace.read]`). |
| `startup_task` | string | No | Autonomous initial prompt or command run when the session starts. |
| `mcp_servers` | list of string | No | MCP servers required by this agent (e.g. `[agent-mux]`). |
| `triggers` | list of TriggerDefinition | No | Declarative event triggers for autonomous monitoring (see [Agent Monitoring](agent-monitoring.md)). |
| `monitoring` | MonitoringConfig | No | Bounds and rate limits for autonomous investigations (defaults to `automatic: false`). |
| `launch` | map of harness → LaunchOverrides | No | Per-harness launch parameters (see [Per-Harness Launch Parameters](#7-per-harness-launch-parameters)). Keys must appear in `harnesses`. |

---

## 2. Discovery and Precedence

When discovering available agents, `agent-mux` performs a pure read-only scan. Discovery never mutates the filesystem.

### Resolution Precedence

Agent definitions are resolved with the following strict hierarchy:

```text
1. Workspace Canonical Packages   (.agent-mux/agents/<id>/AGENTS.md)
2. Global Canonical Packages      (~/.agent-mux/agents/<id>/AGENTS.md)
3. Bundled Canonical Packages     (<share-dir>/agents/<id>/AGENTS.md)
4. Workspace Legacy Files         (.agent-mux/agents/<id>.md)
5. Global Legacy Files            (~/.agent-mux/agents/<id>.md)
6. Compiled-in Packages           (Heimdall, embedded in the binary)
```

- **Canonical overrides legacy within the same root**: If both `<id>/AGENTS.md` and `<id>.md` exist in the same directory, the canonical package takes precedence.
- **Workspace overrides global and bundled**: Workspace definitions shadow global or bundled definitions sharing the same `id`.
- **Global overrides bundled**: User global definitions shadow default bundled agents.
- **Duplicate ID rejection**: If two distinct subdirectories within the same root declare the same `id`, the duplicate is rejected and a diagnostic message is emitted.
- **Generated directories**: Subdirectories named `generated/` are explicitly ignored during discovery.
- **Compiled-in last**: `discover_agents` reports only what is on disk; `discover_agents_with_builtins` (used by the TUI and CLI) appends the compiled-in packages for any id not already found.

### Live Reload

The Agents sidebar is rebuilt from these roots at startup and whenever they change on disk. A filesystem watch (the `notify` crate) covers the workspace, global, and bundled roots; a root that does not exist yet is watched through its nearest existing ancestor and promoted once created. Bursts of events are debounced (400 ms) into one rescan, and the selection follows the agent id across rescans. Switching to the Agents section with Tab also rescans.

### Directory Resolution Paths

1. **Workspace Root**:
   - Resolved relative to current working directory: `./.agent-mux/agents`
2. **Global Root**:
   - `AGENT_MUX_AGENTS_DIR` (environment variable override), or
   - `~/.agent-mux/agents` (default)
3. **Bundled Root**:
   - `AGENT_MUX_BUNDLED_AGENTS_DIR` (explicit override)
   - `AGENT_MUX_SHARE_DIR/agents`
   - Relative to executable: `<prefix>/share/agent-mux/agents`
   - System paths: `/usr/local/share/agent-mux/agents`, `/usr/share/agent-mux/agents`
   - Development repository path: `./agents`

---

## 3. Migration from Legacy Flat Files

Prior to package-based agents, agents were stored as flat files (`~/.agent-mux/agents/<id>.md`).

### `migrate_legacy`

`agent-mux` provides explicit non-destructive migration:
- Copies the legacy source `<id>.md` to `<id>/AGENTS.md`.
- **Preserves** the original source file.
- **Refuses to overwrite** an existing destination file.

---

## 4. Packaging and Distribution

When distributing `agent-mux`:
1. The pre-built binary is installed to `<prefix>/bin/agent-mux`.
2. The bundled agent assets under `agents/` (including `agents/heimdall/AGENTS.md`) are installed to `<prefix>/share/agent-mux/agents/`.
3. Heimdall is also compiled into the binary (`include_str!` of `agents/heimdall/AGENTS.md`), so it is present even when the share directory is missing. A compiled-in agent has no source path, is marked built-in, and cannot be uninstalled; a workspace, global, or bundled package with the same id shadows it. A missing share directory still produces a diagnostic.

---

## 5. TUI Previews and Telemetry Caching

In `agent-mux`, autonomous agents are displayed in the **Agents** sidebar section.

### Launching and the Singleton Rule

Pressing Enter on an agent opens the harness picker, listing only the harnesses the package declares, with `default_harness` preselected. An agent is a **singleton**: at most one live session per agent id, whatever harness or workspace it was started with. While a session exists, the sidebar shows the running harness next to the agent (for example `⚡ Heimdall [claude]`), Enter attaches to that session instead of opening the picker, and a programmatic launch for a different harness attaches with a warning. Closing the session is the only way to start the agent on another harness. Sessions restored from `sessions.json` count, as long as they are still alive.

### Capability-Based Previews
Previews are dynamically selected based on declared agent capabilities rather than agent identity:
- **`trace.read` Capability**: When an agent declares `capabilities: [trace.read]` (e.g. Heimdall or an Audit agent), the main pane renders an **Executive Briefing & Telemetry** dashboard with real-time session clues, active tools, files modified, recent commands, and uncapped metrics.
- **Definition Previews**: Agents without `trace.read` render an autonomous agent definition card containing metadata, origin, supported harnesses, capabilities, and parsed markdown instructions.

### Asynchronous Telemetry Cache
- **Non-blocking UI**: The TUI render loop never initiates database I/O or blocks on SQLite.
- **Background Worker**: A dedicated blocking task runs queries outside the UI thread, emitting `AppEvent::AnalysisUpdated { revision, result }`.
- **Throttling & Coalescing**: Telemetry refreshes at most once per second while a trace-capable preview is visible.
- **Resilient Stale Cache**: In the event of SQLite lock contention or transient query failures, the cached briefing is preserved and displayed alongside a clear warning indicator.

---

## 6. CLI Management Commands

Agents can be inspected, built, installed, and validated directly from the CLI:

```sh
# Validate agent definitions and show diagnostics
agent-mux agent doctor <agent-id>

# Generate disposable harness artifacts without launching
agent-mux agent build <agent-id>

# Install an agent package into the global agent directory (~/.agent-mux/agents/)
agent-mux agent install /path/to/my-agent/AGENTS.md

# Remove an installed agent package
agent-mux agent uninstall <agent-id>

# Non-destructively migrate a legacy flat file (.agent-mux/agents/<id>.md)
agent-mux agent migrate <legacy-file>
```

For the compiled-in Heimdall, `build`, `doctor` and `install` keep artifacts under `~/.agent-mux/agents/heimdall/generated/`; `uninstall` refuses and points at shadowing instead.

---

## 7. Per-Harness Launch Parameters

The launch builder maps a package onto each harness's real command line. Precedence for every value is:

```text
explicit launch override > launch.<harness> block > agent field > profile > harness default
```

| Harness | Instructions | MCP (`mcp_servers: [agent-mux]`) | Startup task | Other |
| :--- | :--- | :--- | :--- | :--- |
| `claude` | `--append-system-prompt <body>` | `--mcp-config <inline JSON>` | positional prompt | `--model`, `--dangerously-skip-permissions` |
| `codex` | `-c developer_instructions=<json string>` | `-c mcp_servers.agent-mux.command/args` | positional prompt | `--model`, `--yolo`; `--no-alt-screen` is added at spawn for every Codex session |
| `agy` | custom main agent written to `~/.gemini/config/agents/<id>/agent.md` and selected with `--agent <name>` | registered through `agy mcp add agent-mux …` before spawn | `--prompt-interactive` | `--model`, `--dangerously-skip-permissions` |

The `agent-mux` MCP server is started with `--all-workspaces`, so an agent's briefing spans every session agent-mux runs.

The optional `launch` block adds per-harness values without touching the profile:

```yaml
launch:
  claude:
    args: [--add-dir, /shared/docs]
    model: claude-opus-5
    env:
      AUDIT_LEVEL: strict
  codex:
    bypass_approvals: true
```

| Field | Type | Effect |
| :--- | :--- | :--- |
| `args` | list of string | Appended after the harness mapping and before any explicit extra args and the startup task. |
| `env` | map | Environment variables for the harness process. |
| `model` | string | Model, unless an explicit override names one. |
| `bypass_approvals` | bool | Skip permission prompts, unless an explicit override decides. |

A `launch` key for a harness missing from `harnesses` is a validation error, and unknown fields inside a block are rejected. Files an harness needs (the Antigravity `agent.md`) and setup commands (`agy mcp add`) are only materialized by `prepare_launch` right before spawn; building a launch never touches disk.
