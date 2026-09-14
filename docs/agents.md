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
| `id` | string | Yes | Lowercase alphanumeric identifier matching `^[a-z0-9][a-z0-9_-]*$`. |
| `name` | string | No | Human-readable name. Defaults to capitalized `id`. |
| `icon` | string | No | Emoji or character icon for the sidebar menu. |
| `description` | string | No | One-sentence summary of the agent's role and purpose. |
| `harnesses` | list of string | Yes | Supported AI coding harnesses (`claude`, `codex`, `agy`). Must contain at least one harness. |
| `default_harness` | string | No | Initial harness selected when launching. Must be declared in `harnesses`. Defaults to first harness in list. |
| `capabilities` | list of string | No | Declared capabilities (e.g. `[trace.read]`). |
| `startup_task` | string | No | Autonomous initial prompt or command run when the session starts. |
| `mcp_servers` | list of string | No | MCP servers required by this agent (e.g. `[agent-mux]`). |
| `triggers` | map / value | No | Declarative event triggers for autonomous monitoring (Milestone 3). |

---

## 2. Discovery and Precedence

When discovering available agents, `agent-mux` performs a pure read-only scan. Discovery never mutates the filesystem.

### Resolution Precedence

Agent definitions are resolved with the following strict hierarchy:

```text
1. Workspace Canonical Packages   (.agent-mux/agents/<id>/AGENTS.md)
2. Workspace Legacy Files         (.agent-mux/agents/<id>.md)
3. Global Canonical Packages      (~/.agent-mux/agents/<id>/AGENTS.md)
4. Global Legacy Files            (~/.agent-mux/agents/<id>.md)
5. Bundled Canonical Packages     (<share-dir>/agents/<id>/AGENTS.md)
```

- **Canonical overrides legacy within the same root**: If both `<id>/AGENTS.md` and `<id>.md` exist in the same directory, the canonical package takes precedence.
- **Workspace overrides global and bundled**: Workspace definitions shadow global or bundled definitions sharing the same `id`.
- **Global overrides bundled**: User global definitions shadow default bundled agents.
- **Duplicate ID rejection**: If two distinct subdirectories within the same root declare the same `id`, the duplicate is rejected and a diagnostic message is emitted.
- **Generated directories**: Subdirectories named `generated/` are explicitly ignored during discovery.

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
3. If bundled assets are missing on the system, `agent-mux` outputs diagnostic warnings rather than using compiled-in fallback persona strings.
