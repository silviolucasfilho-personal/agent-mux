# Generic Agent Monitoring and Review Memory

`agent-mux` provides opt-in, bounded event-triggered investigations and persistent review memory for any Markdown-defined agent package (`AGENTS.md`).

---

## 1. Core Principles

- **Pure Source Definition**: All triggers, prompts, and monitoring budgets are defined in the agent's `AGENTS.md` frontmatter. No agent-specific Rust code exists.
- **Disabled by Default**: Automatic model investigations are strictly disabled (`automatic: false`) by default. No background model calls occur unless explicitly enabled by the user.
- **Review Memory Separation**: Review state, briefings, findings, and job tracking are stored in `~/.agent-mux/agent-state.db`, completely separated from trace storage (`traces.db`).
- **Read-Only Ingestion**: Reading traces and generating briefings never modifies `traces.db` and never advances review cursors. Cursors advance solely through explicit user acknowledgment.
- **Lineage Suppression**: Watchers never trigger on their own actions or actions of their descendant subagents.

---

## 2. Declarative Triggers and Monitoring Schema

Agent packages declare monitoring behavior using `triggers` and `monitoring` frontmatter sections:

```yaml
---
id: heimdall
name: Heimdall
harnesses: [claude, codex, agy]
default_harness: agy
triggers:
  - event: repeated_error
    prompt: "Inspect the repeated error evidence and report its likely cause with citations."
    debounce_ms: 5000
  - event: process_exit
    prompt: "Summarize session completion and verify outcomes."
    debounce_ms: 5000
monitoring:
  automatic: false
  max_jobs_per_hour: 3
  max_concurrent_jobs: 1
  timeout_seconds: 120
  max_turns: 3
  max_tokens: 50000
  max_cost_usd: 0.25
---
```

### Supported Trigger Events

| Event | Meaning | Detection |
| :--- | :--- | :--- |
| `repeated_error` | 3 or more recurring tool errors within a session | Count of error tool observations in committed changes |
| `process_exit` | A coding session or launch terminated | Update or termination change on session / launch |
| `waiting_for_user` | Session paused waiting for human input | Update change on trace |
| `collector_stale` | Telemetry dropped or stalled | Absence of observations after expected interval |

### Monitoring Configuration Bounds

| Field | Default | Description |
| :--- | :--- | :--- |
| `automatic` | `false` | When `false`, changes are tracked in findings but no background jobs run |
| `max_jobs_per_hour` | `3` | Maximum autonomous investigations launched within a sliding 60-minute window |
| `max_concurrent_jobs`| `1` | Maximum parallel running investigation processes |
| `timeout_seconds` | `120` | Wall-clock deadline for each investigation session |
| `max_turns` | `3` | Maximum turns allowed in the model/tool execution loop |
| `max_tokens` | `None` | Optional token consumption limit |
| `max_cost_usd` | `None` | Optional cost limit |

---

## 3. CLI Management Commands

Agent monitoring and review progress are managed via explicit CLI commands:

```sh
# Start background monitoring watcher for an agent
agent-mux agent watch start heimdall

# Stop background monitoring watcher
agent-mux agent watch stop heimdall

# Inspect watcher status and cursor progress
agent-mux agent watch status heimdall

# Acknowledge a briefing, advancing the review cursor
agent-mux agent mark-reviewed <briefing-id> [agent-id]

# Explicitly retry an interrupted or failed investigation job
agent-mux agent retry <job-id>
```

---

## 4. Operational Guarantees

### Crash & Restart Safety
If `agent-mux` or the host system restarts while an investigation is in flight:
- Startup recovery transitions all `running` jobs in `agent-state.db` to `interrupted`.
- Interrupted jobs are **never** replayed automatically.
- Users can inspect interrupted jobs and explicitly retry them using `agent-mux agent retry <job-id>`.

### Non-Regressing Review Cursors
- `record_briefing` stores new briefings with `acknowledged = 0`.
- The user's `reviewed_through` sequence advances strictly through explicit `acknowledge` / `mark-reviewed` calls.
- Acknowledging older briefings never regresses the review cursor (`max(current, briefing.through_seq)`).
