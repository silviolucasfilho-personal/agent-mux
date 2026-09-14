# agent-mux Shared Trace MCP Service

This document provides the reference manual for the `agent-mux` Model Context Protocol (MCP) server, its eight read-only diagnostic tools, scope boundaries, limits, error formats, and native harness integration.

---

## 1. Overview

The `agent-mux` MCP server (`agent-mux mcp serve --stdio`) exposes the SQLite trace and analytics store (`~/.agent-mux/traces.db`) as standard MCP tools to AI coding agents (such as Heimdall) running across Claude Code (`claude`), Codex CLI (`codex`), and Google Antigravity (`agy`).

Key architectural properties:
- **Read-only execution**: Operates strictly in read-only mode (`SQLITE_OPEN_READ_ONLY`). It never creates, migrates, writes to, or locks the SQLite trace store.
- **Zero background inference**: Does not call external LLM models or trigger network traffic.
- **Strict workspace scoping**: By default, each MCP server is scoped to a single workspace directory, rejecting requests attempting to query sessions outside that directory.
- **Uniform response envelope**: Every tool returns structured JSON wrapped in a validated envelope with temporal metadata, pagination cursors, and coverage indicators.
- **Dual content format**: In accordance with MCP protocol requirements, every response includes both typed `structuredContent` and serialized UTF-8 `content[0].text` matching character-for-character.

---

## 2. Invocation and Command-Line Interface

### Server Stdio Transport

Harnesses launch `agent-mux` via stdio using distinct argument elements:

```sh
agent-mux mcp serve --stdio --db <PATH> --workspace <DIR> [--all-workspaces]
```

| Flag | Description | Default |
| :--- | :--- | :--- |
| `--stdio` | Run in standard input/output JSON-RPC mode. | Required |
| `--db <PATH>` | Path to SQLite trace database. | `$AGENT_MUX_TRACE_DB` or `~/.agent-mux/traces.db` |
| `--workspace <DIR>` | Target workspace scope directory. | Current working directory |
| `--all-workspaces` | Authorize querying sessions across all workspaces. | Disabled (single-workspace by default) |

### CLI Parity: `trace briefing`

Users or scripts can run the exact same briefing logic without starting an MCP server:

```sh
# Human-readable executive morning briefing
agent-mux trace briefing [--workspace DIR] [--since RFC3339]

# Machine-readable JSON output matching the MCP envelope
agent-mux trace briefing --json [--workspace DIR] [--since RFC3339] [--db PATH]
```

---

## 3. Tool Reference

The service exposes exactly 8 tools. All arguments use closed schemas (`additionalProperties: false`) and reject unknown fields.

### 1. `agent_mux_get_briefing`
Executive morning briefing summarizing workspace state, active and completed sessions, tool usage, modified files, and resource spend.

- **Arguments**:
  - `since` (string, optional): RFC3339 timestamp filter. Defaults to 24 hours prior to `until`.
  - `until` (string, optional): RFC3339 timestamp filter. Defaults to current UTC time.
  - `provider` (string, optional): Filter by provider (`claude`, `codex`, `agy`).
  - `cursor` (string, optional): Opaque pagination cursor.
  - `limit` (integer, optional): Page size (default 20, max 50).

### 2. `agent_mux_list_sessions`
Lists session cards matching scope, status, and time criteria.

- **Arguments**:
  - `since` (string, optional): RFC3339 timestamp.
  - `until` (string, optional): RFC3339 timestamp.
  - `provider` (string, optional): Filter by provider.
  - `runtime_state` (string, optional): `idle`, `working`, `stuck`, `exited`.
  - `cursor` (string, optional): Pagination cursor.
  - `limit` (integer, optional): Page size.

### 3. `agent_mux_get_session`
Detailed telemetry for a specific session including completed turns, active tools, and correlation quality.

- **Arguments**:
  - `session_key` (string, required): Session key or identity.
  - `launch_id` (string, optional): Specific launch ID if resolving disambiguation.

### 4. `agent_mux_get_timeline`
Chronological turn-by-turn trace observations and tool invocations within a session.

- **Arguments**:
  - `session_key` (string, required): Session identifier.
  - `launch_id` (string, optional): Launch identifier.
  - `cursor` (string, optional): Pagination cursor.
  - `limit` (integer, optional): Page size (default 50, max 100).

### 5. `agent_mux_search_traces`
Full-text search over trace inputs, outputs, and tool observations.

- **Arguments**:
  - `query` (string, required): Text search pattern.
  - `session_key` (string, optional): Restrict search to one session.
  - `provider` (string, optional): Restrict search to one provider.
  - `since` (string, optional): RFC3339 timestamp.
  - `until` (string, optional): RFC3339 timestamp.
  - `cursor` (string, optional): Pagination cursor.
  - `limit` (integer, optional): Page size.

### 6. `agent_mux_analyze_skills`
Token usage, invocation frequency, latency percentiles (p50, p95), and attribution across skills.

- **Arguments**:
  - `skill` (string, optional): Specific skill name.
  - `provider` (string, optional): Specific provider filter.
  - `since` (string, optional): RFC3339 timestamp.
  - `until` (string, optional): RFC3339 timestamp.
  - `cursor` (string, optional): Pagination cursor.
  - `limit` (integer, optional): Page size.

### 7. `agent_mux_compare_runs`
Side-by-side behavioral comparison of two runs or sessions.

- **Arguments**:
  - `a` (string, required): First run ID or session key.
  - `b` (string, required): Second run ID or session key.

### 8. `agent_mux_get_health`
Service and collector health status, available features, database connectivity, and timestamp of the latest collected trace.

- **Arguments**: None (`{}`).

---

## 4. Common Response Envelope

All tools return data encapsulated in a standard envelope:

```json
{
  "schema_version": 1,
  "as_of": "2026-09-14T12:00:00Z",
  "scope": {
    "workspace": "/Users/sifilho/workspace/agent-mux",
    "all_workspaces": false
  },
  "window": {
    "since": "2026-09-13T12:00:00Z",
    "until": "2026-09-14T12:00:00Z"
  },
  "data": { ... },
  "coverage": {
    "status": "full",
    "reasons": []
  },
  "warnings": [],
  "next_cursor": null,
  "truncated": false
}
```

---

## 5. Scope & Admission Limits

1. **Admission Control**:
   - **Max concurrent requests**: 4.
   - **Queue capacity**: 16 pending requests.
   - **Rejection on saturation**: If the queue exceeds capacity, queries fail immediately with code `BUSY`.
2. **Query Deadlines**:
   - Standard queries: 2-second deadline enforced via SQLite progress handlers.
   - Heavy analytical queries (`analyze_skills`, `compare_runs`): 5-second deadline.
   - Queries exceeding the deadline return `QUERY_TIMEOUT`.
3. **Payload Bounds**:
   - Max serialized response size: **64 KiB**.
   - If a page exceeds the limit, records are safely truncated and `truncated: true` is indicated.
4. **Scope Enforcement**:
   - Single-workspace mode enforces strict path prefixing.
   - Attempts to access traces with relative path traversal (`../`) or outside the workspace return `SCOPE_DENIED`.

---

## 6. Error Handling

Errors conform to JSON-RPC 2.0 error responses with standardized error codes:

| Code | Meaning | Retryable |
| :--- | :--- | :--- |
| `INVALID_ARGUMENT` | Missing required fields, unknown parameters, invalid timestamps. | No |
| `NOT_FOUND` | Session or trace ID does not exist. | No |
| `SCOPE_DENIED` | Target resource belongs to an unauthorized workspace. | No |
| `DB_UNAVAILABLE` | Database file missing, corrupted, or unreadable. | No |
| `SCHEMA_UNSUPPORTED` | SQLite schema version incompatible. | No |
| `CONTENT_UNAVAILABLE` | Full content requested but running in metadata-only mode. | No |
| `QUERY_TIMEOUT` | Query exceeded execution deadline (2s or 5s). | Yes |
| `BUSY` | All concurrency and queue slots occupied. | Yes |
| `CURSOR_EXPIRED` | Pagination cursor expired (TTL 300s) or filter hash mismatched. | No |
| `INTERNAL_ERROR` | Unhandled internal exception. | No |

---

## 7. Native Harness Configuration

When building an agent with `mcp_servers: [agent-mux]` (e.g. `agent-mux agent build heimdall`), artifacts are generated for each supported harness:

### Claude Code (`claude/mcp.json`)
```json
{
  "mcpServers": {
    "agent-mux": {
      "command": "/usr/local/bin/agent-mux",
      "args": [
        "mcp",
        "serve",
        "--stdio",
        "--db",
        "/Users/sifilho/.agent-mux/traces.db",
        "--workspace",
        "/Users/sifilho/workspace/agent-mux"
      ]
    }
  }
}
```

### Codex CLI (`codex/mcp.toml`)
```toml
[mcp_servers.agent-mux]
command = "/usr/local/bin/agent-mux"
args = ["mcp", "serve", "--stdio", "--db", "/Users/sifilho/.agent-mux/traces.db", "--workspace", "/Users/sifilho/workspace/agent-mux"]
```

### Google Antigravity (`agy/mcp.json`)
```json
{
  "mcpServers": {
    "agent-mux": {
      "command": "/usr/local/bin/agent-mux",
      "args": [
        "mcp",
        "serve",
        "--stdio",
        "--db",
        "/Users/sifilho/.agent-mux/traces.db",
        "--workspace",
        "/Users/sifilho/workspace/agent-mux"
      ]
    }
  }
}
```
