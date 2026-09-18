# Heimdall Startup Dossier (Schema Version 2)

Heimdall briefs the user on active and recent sessions and evaluates skills and subagents without initial tool calls. agent-mux computes these facts in Rust before launching Heimdall, writing an owner-only JSON document to `<runtime>/briefings/<launch-id>.json` and setting `$AGENT_MUX_BRIEFING`, `$AGENT_MUX_BRIEFING_AS_OF`, and `$AGENT_MUX_BRIEFING_SCHEMA=2`.

## Design Principles

1. **Zero Startup Tool Calls**: For the default briefing and analysis queries, Heimdall answers directly from the dossier without invoking MCP tools, the `trace` CLI, or SQL queries.
2. **Single SQLite Connection**: The dossier is produced in Rust from one read-only SQLite connection (`store::open_ro`), one filesystem definition scan (`inventory_all`), and one live-snapshot read.
3. **Strict Time Windows**:
   - **Sessions**: strictly the 24-hour window ending at `as_of` (`[as_of - 24h, as_of)`).
   - **Evaluations (Skills & Agents)**: strictly the 30-day window ending at `as_of` (`[as_of - 30d, as_of)`).
4. **Hard 64 KiB Payload Budget**: If the uncompressed dossier exceeds 65,536 bytes, a deterministic reduction algorithm removes evidence examples first, then zero-activity definition rows, then lowest-ranked metric rows, while always preserving totals, sample sizes, and error telemetry.
5. **Section Failure Isolation**: A failure in one section (e.g. SQLite timeout or inventory error) marks only that section `unavailable` with a typed error code (e.g. `QUERY_TIMEOUT`, `SECTION_QUERY_FAILED`), leaving other sections fully usable.

---

## Schema Structure

```json
{
  "schema_version": 2,
  "as_of": "2026-09-18T10:00:00Z",
  "scope": {
    "workspace": "/path/to/project"
  },
  "windows": {
    "sessions": {
      "since": "2026-09-17T10:00:00Z",
      "until": "2026-09-18T10:00:00Z"
    },
    "evaluations": {
      "since": "2026-08-19T10:00:00Z",
      "until": "2026-09-18T10:00:00Z"
    }
  },
  "health": {
    "reader_schema_version": 11,
    "store_schema_version": 11,
    "db_path": "/path/to/traces.db",
    "content_mode": "full",
    "live_snapshot_available": true,
    "provider_coverage": ["claude", "codex", "antigravity"],
    "priced_generations": null,
    "unpriced_generations": null,
    "total_build_ms": 42,
    "sessions_build_ms": 18,
    "skills_build_ms": 12,
    "agents_build_ms": 11,
    "bytes": 18450,
    "max_session_cards": 20,
    "max_skill_rows": 30,
    "max_agent_rows": 30,
    "examples_per_category": 3
  },
  "sessions": {
    "status": "full",
    "warnings": [],
    "errors": [],
    "truncated": false,
    "total_matching": 3,
    "data": {
      "sessions": [ /* SessionCard */ ],
      "total_sessions": 3,
      "total_turns": 42,
      "total_tools": 118,
      "total_tokens": 125000,
      "total_cost_usd": 1.45
    }
  },
  "skills": {
    "status": "full",
    "warnings": [],
    "errors": [],
    "truncated": false,
    "total_matching": 12,
    "data": {
      "skills": [ /* SkillFact */ ],
      "total_definitions": 10,
      "total_observed": 8
    }
  },
  "agents": {
    "status": "full",
    "warnings": [],
    "errors": [],
    "truncated": false,
    "total_matching": 4,
    "data": {
      "agents": [ /* AgentFact */ ],
      "total_definitions": 3,
      "total_observed": 3
    }
  }
}
```

---

## Section Envelopes and Coverage Status

Every top-level data section (`sessions`, `skills`, `agents`) is wrapped in a `DossierSection<T>` envelope:
- `status`:
  - `full`: section computed cleanly without truncation or warnings.
  - `partial`: section contains warnings, or rows/examples were truncated.
  - `unavailable`: query timed out or failed. `data` is empty, and `errors` lists the cause.
- `warnings`: array of human-readable warnings (e.g. `"Examples omitted to fit byte limit"`).
- `errors`: array of `DossierError { code, message }` (e.g. `QUERY_TIMEOUT`, `SECTION_QUERY_FAILED`).
- `truncated`: boolean indicating whether items were truncated due to row caps or byte budget.
- `total_matching`: uncapped count of matching records before truncation.

---

## Deterministic Ordering and Tie Breakers

- **Sessions**: live processes first, then `last_active_ns` descending, then `session_key` ascending.
- **Skills**: active skills (`has_activity()`) first, then `turns_unused` descending, then `errors` descending, then `cost_usd` descending, then `key` ascending.
- **Agents**: active agents (`has_activity()`) first, then `failures` descending, then `cost_usd` descending, then `p90_ms` descending, then `agent_type` ascending.
- **Examples**: category severity, timestamp descending, source ID ascending.

---

## 64 KiB Budget Reduction Order

When serialized JSON size exceeds `max_bytes` (default 65,536 bytes):
1. **Examples**: cleared from lowest-ranked skill and agent rows first.
2. **Zero-Activity Definitions**: definition-only rows with zero observed usage are removed from lowest-ranked skills and agents.
3. **Metric Rows**: lowest-ranked active rows are removed until the dossier fits the budget.
4. **Failure Threshold**: if an empty dossier envelope cannot fit `max_bytes`, hydration writes a typed `DOSSIER_TOO_LARGE` error envelope.

Top-level totals (`total_sessions`, `total_matching`, `total_definitions`, `total_observed`), sample sizes, and health telemetry are never discarded.

---

## Drill-Down Protocol

Heimdall consults the MCP server or the `trace` CLI only when:
- The requested information is newer than `$AGENT_MUX_BRIEFING_AS_OF`.
- The user asks about a different workspace or an older time window.
- The item was omitted or truncated from the dossier.
- The query requires detailed turn-by-turn timelines (`agent_mux_get_timeline`), search (`agent_mux_search_traces`), or run comparison (`agent_mux_compare_runs`).

---

## Backward Compatibility

- Packages that request `hydrate = ["briefing"]` receive schema-v1 briefings (`schema_version: 1`).
- Built-in Heimdall requests `hydrate = ["dossier"]` and receives schema-v2 dossiers (`schema_version: 2`).
- Packages cannot request both `briefing` and `dossier` simultaneously.
