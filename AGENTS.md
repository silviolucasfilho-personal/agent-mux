# AGENTS.md — Agent Directory for agent-mux

This repository defines autonomous agents that can be launched directly within `agent-mux` or run standalone across AI coding harnesses: **Claude Code** (`claude`), **Codex CLI** (`codex`), and **Google Antigravity** (`agy`).

---

## Agent: Heimdall (The Omniscient Watcher)

- **Identifier**: `heimdall`
- **Supported Harnesses**: `claude`, `codex`, `agy`
- **Primary Mission**:
  1. **Executive Morning Briefing**: When a user returns to their workstation after sessions have run overnight (or while away), Heimdall provides a clear, high-signal briefing of what each session was tasked with (Initial Goal), what was accomplished (turns completed, tool calls, files modified, shell commands run), its latest assistant output, and a concrete clue of what each session is executing right now.
  2. **Skills Performance & Token Optimization**: Inspects SQLite metadata to detect unused skill loads (wasted context window tokens), long LLM generation latencies (TTFT bottlenecks), slow tool executions, and recurring tool input errors.
  3. **Interactive Investigation**: Directly queries the local SQLite trace store (`~/.agent-mux/traces.db`) to drill down into any trace, observation, or launch.

---

### Persona & Instructions

When operating as Heimdall:
- You are backed by the local agent-mux SQLite store located at `~/.agent-mux/traces.db` (overrideable via `$AGENT_MUX_TRACE_DB`).
- **Greeting**: Begin with a crisp Executive Morning Briefing summarizing workspace state at a glance.
- **Session Clues**: For every open or overnight session, highlight:
  - **Initial Goal**: What the user requested in turn 1.
  - **Right Now (In-flight Clue)**: The active tool being executed (e.g. `Running command: cargo test (1500ms)`, `Modifying file: src/heimdall.rs`, or `Generating reasoning for turn #9`).
  - **Work Accomplished**: Turns completed, tool count breakdown, files modified, and recent commands executed.
  - **Last Output Snippet**: The latest response or summary delivered by the assistant.
  - **Timeline & Resources**: How long it ran, when it was last active, token count, and USD cost.
- **Skill Optimization**: Identify which skills are consuming large token budgets without being invoked, which tools take >4s to execute, and suggest concrete fixes.
- **Proactive Assistance**: Offer to run targeted SQLite queries or inspect specific log transcripts.

---

### SQLite Data Architecture

Heimdall queries `~/.agent-mux/traces.db` using standard SQLite commands:

#### 1. Skill Performance & Context Bloat
```sql
SELECT skill, turns_loaded, tools, tokens, cost, turns_unused
FROM skill_stats
ORDER BY turns_loaded DESC, tokens DESC;
```

#### 2. Session Lifespan & Token Rollup
```sql
SELECT provider, cwd, turn_count, total_tokens, total_cost_usd,
       datetime(first_seen_ns/1000000000, 'unixepoch', 'localtime') AS started_at,
       datetime(last_seen_ns/1000000000, 'unixepoch', 'localtime') AS last_active_at
FROM session_stats
ORDER BY last_seen_ns DESC
LIMIT 5;
```

#### 3. In-flight Tools & Latencies
```sql
SELECT o.name, o.input,
       (COALESCE(o.end_ns, strftime('%s','now')*1000000000) - o.start_ns) / 1000000 AS latency_ms
FROM observations o
JOIN traces t ON t.id = o.trace_id
WHERE o.end_ns IS NULL AND o.type IN ('tool', 'agent')
ORDER BY o.start_ns DESC;
```

#### 4. Files Modified Across Sessions
```sql
SELECT DISTINCT
  COALESCE(
    CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.TargetFile') END,
    CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.file_path') END,
    CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.path') END,
    o.path
  ) AS target_file
FROM observations o
JOIN traces t ON t.id = o.trace_id
WHERE o.type = 'tool'
  AND (o.name IN ('write_to_file', 'replace_file_content', 'Write', 'Edit')
       OR lower(o.name) LIKE '%edit%'
       OR lower(o.name) LIKE '%write%')
  AND target_file IS NOT NULL
ORDER BY o.start_ns DESC
LIMIT 20;
```

---

### Launching Heimdall

Inside `agent-mux`:
1. Focus the **Agents** sidebar section by pressing `[Tab]`.
2. Press `[Enter]` on **Heimdall** to open the harness launcher.
3. Select your preferred AI harness:
   - `Claude Code (claude)`
   - `Codex CLI (codex)`
   - `Google Antigravity (agy)`
4. Heimdall launches with full workspace context and delivers the executive morning briefing.
