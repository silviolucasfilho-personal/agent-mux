# AI Harness Compatibility Record

This document records the compatibility status, probed CLI versions, configuration formats, and operational flags for AI coding harnesses supported by `agent-mux`.

---

## 1. Probed Versions (Verified on Development Environment)

Probed on macOS (Darwin arm64) as of 2026-09-14:

| Harness | CLI Command | Probed Version | Binary Location | Launch Strategy |
| :--- | :--- | :--- | :--- | :--- |
| **Claude Code** | `claude` | `2.1.270` | `/Users/sifilho/.local/bin/claude` | Injects system instructions via `--append-system-prompt`, startup prompt via argument, and MCP servers via `--mcp-config` / `generated/claude/mcp.json`. |
| **Codex CLI** | `codex` | `0.154.0` | `/opt/homebrew/bin/codex` | Interactive CLI with `--no-alt-screen` and prompt argument; MCP tools configured via `mcp.toml` or CLI options. |
| **Google Antigravity** | `agy` | `1.2.2` | `/Users/sifilho/.local/bin/agy` | Interactive launcher with `--prompt-interactive` and configuration in `.antigravity/` / `generated/agy/mcp.json`. |

---

## 2. Artifact and Configuration Compatibility

### Claude Code (`claude`)
- **Artifact Path**: `<agent-pkg>/generated/claude/`
- **Supported Options**:
  - `--append-system-prompt <PROMPT>`: Appends agent package Markdown instructions.
  - `--mcp-config <PATH>`: Registers local agent MCP servers (`agent-mux mcp serve --stdio`).
- **Compatibility Status**: Verified with Claude Code `2.1.x`.

### Codex CLI (`codex`)
- **Artifact Path**: `<agent-pkg>/generated/codex/`
- **Supported Options**:
  - `--no-alt-screen`: Prevents full-screen hijacking to preserve multiplexer terminal scrolling.
  - `mcp.toml`: Declarative MCP server registration for Codex tool invocations.
- **Compatibility Status**: Verified with `codex-cli 0.154.x`.

### Google Antigravity (`agy`)
- **Artifact Path**: `<agent-pkg>/generated/agy/`
- **Supported Options**:
  - `--prompt-interactive <PROMPT>`: Pre-seeds the conversation with the agent startup task.
  - `mcp.json`: Antigravity MCP definition file.
- **Compatibility Status**: Verified with `agy 1.2.x`.

---

## 3. Unverified Versions and Environments

The following configurations are implemented per specifications but have not yet been probed in active end-to-end integration runs on this machine:
- **Windows (x86_64 / aarch64)**: Uses `cmd.exe` / `.cmd` / `.bat` shims; pty handling uses Windows pseudo-console handles (`ConPTY`).
- **Linux (x86_64 / arm64)**: Standard POSIX ptys; paths follow XDG directories (`$XDG_DATA_HOME/agent-mux`).
- **Codex CLI < 0.100.0**: Legacy versions may not support `mcp.toml` configuration format without command-line flag conversions.
- **Antigravity CLI 2.0+ (Preview)**: Early alpha builds of Antigravity 2.0 with sidecar agent protocols.

---

## 4. MCP Server & Harness Verification Script

`agent-mux` includes an automated verification script at `scripts/verify-agent-mcp.sh`:

```sh
# Run verification across all detected harnesses using an isolated fixture DB
./scripts/verify-agent-mcp.sh

# Run targeted check for a single harness
./scripts/verify-agent-mcp.sh --harness claude
./scripts/verify-agent-mcp.sh --harness codex
./scripts/verify-agent-mcp.sh --harness agy
```

### Safety Guarantee
The script refuses to execute if pointed to production trace data (`~/.agent-mux/traces.db` or `$AGENT_MUX_TRACE_DB`). All assertions run against an isolated SQLite store in a temporary directory.
