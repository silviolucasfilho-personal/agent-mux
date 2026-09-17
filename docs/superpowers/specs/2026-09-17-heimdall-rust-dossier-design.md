# Heimdall Rust dossier: one startup read, tools only for drill-down

Status: Proposed
Date: 2026-09-17
Baseline: `6c263b7` plus the current uncommitted Workflows implementation

## 1. Outcome

Heimdall starts with one Rust-generated dossier containing every fact required for its default report. It no longer spends its first turn calling MCP tools or the trace CLI separately for sessions, skills and subagents.

The startup dossier contains:

- active and recent session facts for the last 24 hours;
- skill usage, waste, trigger, performance and lint facts for the last 30 days;
- subagent usage, latency, failure, child-tool and cost facts for the last 30 days;
- store health, coverage, warnings, sample sizes and evidence identifiers.

Heimdall still uses the read-only MCP tools, or the trace CLI when MCP is unavailable, when the user asks for newer information, another workspace or time window, a named entity, a timeline, a search, or a comparison. Rust computes facts; the skill applies thresholds, chooses findings and presents them.

Success means a normal Heimdall launch can answer its built-in startup prompt—an executive session briefing plus the top skill and agent findings—by reading one file and making no MCP or shell calls.

## 2. Existing behavior and problem

`src/skill/launch.rs::hydrate` currently executes only `Request::Briefing` and writes its envelope to `$AGENT_MUX_BRIEFING`. That snapshot already removes the first session-briefing round trip, but it covers only the last-24-hour session cards.

The Heimdall skill must still collect the other parts of its default report independently:

- `agent_mux_analyze_skills`, followed by inventory and lint CLI work for missed triggers and definition problems;
- `trace agents --json`, because subagent analysis is not a `TraceService` request;
- timeline or SQL calls for child-tool counts, recurring failures and cost share;
- health calls when coverage needs explanation.

This makes the first answer slow, exposes the model to several partial response shapes, and repeats store access that Rust can perform more cheaply and consistently. It also lets reference SQL drift from the store schema.

The existing design rules remain:

1. Facts, joins, percentiles, scope, evidence and response bounds are Rust.
2. Thresholds, prioritisation and prose are the skill.
3. MCP is for information that was not part of the launch snapshot or has changed since it was taken.

## 3. Chosen design

Add a typed, bounded `HeimdallDossier` builder to `src/tracing/analysis/`. The builder opens one read-only SQLite connection, captures one `as_of` timestamp, reads live snapshots once, scans definitions once, and produces all startup sections under a single coverage contract.

The dossier is an analysis product rather than an agent runtime. Its types and builder do not launch Heimdall, render prose, apply Heimdall's thresholds or know about a harness. The `dossier` hydration kind is generic and can be requested by another `trace.read` package later; Heimdall is the first package to opt in.

Alternatives rejected:

- **Bundle existing service envelopes.** Easy, but it opens and queries the store repeatedly, carries pagination intended for interactive tools, and still lacks a complete agent-analysis endpoint.
- **Continuously cached dossier.** Fast at launch, but adds invalidation, background work and durable state before measurements show they are needed.
- **Raw store export.** Unbounded, leaks query responsibility back to the model and makes startup slower rather than faster.

## 4. Contract

The file remains available through `$AGENT_MUX_BRIEFING` so every harness and restored-session path keeps one stable environment variable. Its top-level shape changes to schema version 2:

```rust
pub struct HeimdallDossier {
    pub schema_version: u32,          // 2
    pub as_of: String,
    pub scope: DossierScope,
    pub windows: DossierWindows,
    pub health: DossierHealth,
    pub sessions: DossierSection<SessionDossier>,
    pub skills: DossierSection<SkillDossier>,
    pub agents: DossierSection<AgentDossier>,
}

pub struct DossierSection<T> {
    pub status: CoverageStatus,
    pub warnings: Vec<String>,
    pub errors: Vec<DossierError>,
    pub truncated: bool,
    pub total_matching: usize,
    pub data: T,
}
```

`DossierScope` records the canonical workspace. `DossierWindows` records exact RFC 3339 bounds for the 24-hour sessions window and the 30-day evaluation window. Every section states its own coverage because live snapshots, content capture, inventory roots and priced usage can fail independently.

When a section fails, `data` is its typed empty value, `status` is `unavailable` or `partial`, and `errors` explains the missing query. Zero activity is instead a successful section with empty rows and zero totals.

The dossier contains facts, not verdicts. It must not label a skill “wasteful,” an agent “unreliable,” or an action “recommended.” Heimdall applies the thresholds from `reference/skills.md` and `reference/agents.md`.

### 4.1 Session section

`SessionDossier` reuses the existing briefing model and uncapped totals:

```rust
pub struct SessionDossier {
    pub sessions: Vec<SessionCard>,
    pub total_sessions: usize,
    pub total_turns: i64,
    pub total_tools: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
}
```

The builder calls the lower-level briefing query with already-loaded live sessions. It must not invoke `TraceService::execute`, reopen the database or reread live snapshot files.

Cards remain ordered live first, then most recently active. The section includes enough evidence to report goal, now, work done, last output, resources and coverage without a timeline request.

### 4.2 Skill section

Each `SkillFact` joins 30-day store metrics with the effective on-disk definition:

```rust
pub struct SkillFact {
    pub key: String,
    pub harness: Option<String>,
    pub scope: Option<String>,
    pub path: Option<PathBuf>,
    pub triggers: Vec<String>,
    pub definition_bytes: Option<u64>,
    pub turns_loaded: i64,
    pub turns_unused: i64,
    pub missed_triggers: i64,
    pub generations: i64,
    pub tools: i64,
    pub attributed_calls: i64,
    pub errors: i64,
    pub schema_errors: i64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub lint: Vec<DefinitionFinding>,
    pub examples: Vec<SkillEvidence>,
    pub limitations: Vec<String>,
}

pub struct SkillDossier {
    pub skills: Vec<SkillFact>,
    pub total_definitions: usize,
    pub total_observed: usize,
}
```

The implementation consolidates the useful parts of `analysis::metrics::analyze_skills`, `store::query::skill_stats`, and `inventory::skill_reports` behind one typed analysis API. It does not duplicate their SQL.

Examples are bounded to the three newest items in each applicable category: unused loads, missed triggers and recurring attributed errors. They contain source IDs and short snippets, never full prompts or outputs. Totals and sample sizes are computed before example caps.

Definitions are resolved with the same project-over-home-over-plugin precedence used by the inventory. Lint uses Rust's current `inventory::lint` and the measured known-tool set. A stored skill with no current definition remains a row with `not_on_disk`; a definition with no 30-day use remains a row with zero metrics and `never_used_in_window`.

### 4.3 Agent section

Subagent analysis becomes a typed Rust API rather than a CLI-only formatter:

```rust
pub struct AgentFact {
    pub agent_type: String,
    pub definitions: Vec<AgentDefinitionFact>,
    pub invocations: i64,
    pub failures: i64,
    pub mean_ms: Option<u64>,
    pub p50_ms: Option<u64>,
    pub p90_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub child_tools: i64,
    pub child_tool_errors: i64,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub maximum_session_cost_share: Option<AgentCostShareFact>,
    pub recurring_failures: Vec<AgentFailureFact>,
    pub examples: Vec<AgentInvocationFact>,
    pub limitations: Vec<String>,
}

pub struct AgentDossier {
    pub agents: Vec<AgentFact>,
    pub total_definitions: usize,
    pub total_observed: usize,
}
```

The query is workspace- and time-window scoped. This corrects the current `trace agents` behavior for the dossier without silently changing the CLI's all-store default. Percentiles use nearest rank as elsewhere. Child-tool counts include direct children only, matching the documented agent observation model. Cost share is computed per session; `AgentCostShareFact` carries the maximum observed share, its session key and the agent/session costs used to derive it.

Examples are the three newest failed invocations, or the three slowest invocations when none failed. Each names its observation ID, trace ID, session key, duration, child-tool count and a bounded task snippet. Recurring failures are grouped by child tool and normalized status message, capped at five groups after uncapped counts.

On-disk agent definitions include harness, scope, path, declared tools, model and Rust lint findings. Definitions with zero invocations are included so Heimdall can identify idle agents without another inventory scan.

### 4.4 Health and evidence

`DossierHealth` includes:

- reader and store schema versions;
- database path and content mode;
- collector freshness and provider coverage;
- live snapshot availability;
- priced/unpriced generation counts where available;
- dossier build duration per section and total;
- byte size and section caps applied.

Every representative row contains stable session, trace, observation or definition-path identifiers. Heimdall can cite these directly. When content mode withholds text, snippets are absent and the section limitation explains why; Rust does not replace them with guesses.

## 5. Query and module boundaries

Create `src/tracing/analysis/dossier.rs`. It owns the public dossier types, `DossierConfig`, section orchestration, deterministic ranking and byte budgeting.

The section implementations delegate to focused APIs:

- `query.rs`: existing session briefing and session evidence;
- `metrics.rs`: scoped skill performance metrics;
- a new `agents.rs` under `analysis/`: scoped subagent aggregates and examples;
- `inventory.rs`: definition discovery, trigger matching and lint;
- `store::query`: low-level reusable rows only where they are already canonical.

`DossierBuilder` receives an open `rusqlite::Connection`, workspace, definition roots or home, live sessions, `as_of`, and `DossierConfig`. This makes one-connection behavior testable and prevents it from resolving global paths internally.

```rust
pub struct DossierConfig {
    pub session_window: Duration,       // 24 h
    pub evaluation_window: Duration,    // 30 d
    pub max_bytes: usize,               // 64 KiB default
    pub max_session_cards: usize,       // 20
    pub max_skill_rows: usize,          // 30
    pub max_agent_rows: usize,          // 30
    pub examples_per_category: usize,   // 3
}
```

The dossier keeps the existing 64 KiB service-response ceiling. Avoiding tool round trips must not replace them with an oversized model-context load. Rust computes uncapped totals and rankings before retaining the most relevant 20 session cards and 30 skill and agent rows. Limits can initially be constants in `DossierConfig::default`; no user-facing configuration is added until measurements justify it.

## 6. Determinism and truncation

All ordering has explicit tie breakers:

- sessions: live state, last activity descending, session key;
- skills: activity present first, unused-load count descending, errors descending, cost descending, key;
- agents: failures descending, cost descending, p90 descending, agent type;
- examples: category severity, timestamp descending, stable source ID.

The builder serializes once after normal row caps. If the result exceeds `max_bytes`, it removes examples from the lowest-ranked rows first, then lowest-ranked zero-activity definition rows, then lowest-ranked metric rows. It never removes top-level totals, sample sizes, health, coverage or section errors. Each affected section sets `truncated = true`, retains `total_matching`, and adds a warning describing what was omitted.

If the minimal dossier still exceeds the budget, hydration writes a typed `DOSSIER_TOO_LARGE` error envelope rather than an invalid partial JSON file.

## 7. Hydration and launch flow

`Hydration` gains `Dossier`; `Briefing` remains supported for third-party packages and schema-v1 compatibility. They are mutually exclusive because both own the single `$AGENT_MUX_BRIEFING` file; package validation rejects a `hydrate` list containing both.

Heimdall's `skill.toml` changes to:

```toml
[agent]
hydrate = ["dossier"]
mcp = "auto"
```

At skill launch:

1. Resolve the trace database, canonical workspace, runtime directory and definition roots.
2. Open the store read-only once.
3. Capture `as_of` and derive both windows from that instant.
4. Read live snapshots once.
5. Build all dossier sections, isolating section failures.
6. Atomically write `<runtime>/briefings/<launch-id>.json` with owner-only permissions.
7. Export the existing `AGENT_MUX_BRIEFING` and `AGENT_MUX_BRIEFING_AS_OF` variables plus `AGENT_MUX_BRIEFING_SCHEMA=2`.
8. Append a short prompt hint telling the skill to answer its default report from the dossier and use tools only for newer or narrower questions.

Hydration remains best effort and never prevents the harness from launching. A database-open failure writes a schema-v2 top-level typed error envelope for `dossier` (and the existing schema-v1 envelope for `briefing`). A failure in inventory, skills or agents preserves the successful session section and records the section error. The TUI notice summarizes partial coverage without listing every row-level warning.

Snapshot cleanup on PTY exit, app shutdown and the 24-hour sweep remains unchanged.

The synchronous work stays off the render path's critical section. If launch currently calls hydration inline, the implementation keeps one bounded blocking phase and records its elapsed time. The target is less than 500 ms for a typical store and a hard five-second deadline for the full dossier. The builder installs a temporary SQLite progress handler on the dossier connection, backed by the shared deadline, so a long statement is interrupted rather than merely noticed afterwards; it removes the handler before returning. Deadline expiry produces partial sections and a warning rather than continuing unbounded.

## 8. Heimdall behavior

`skills/heimdall/SKILL.md` changes its setup contract:

1. Read the dossier once before any command.
2. For the built-in startup prompt, answer from `sessions`, `skills` and `agents` without MCP or CLI calls.
3. Apply the documented thresholds to the raw metrics and state sample sizes, windows, coverage and truncation.
4. Use MCP only when the requested fact is newer than `as_of`, outside the dossier windows or workspace, about a named item that was truncated, or requires timeline/search/compare detail.
5. Use the CLI only when MCP is unavailable and the same drill-down condition applies.

The sessions, skills and agents reference files keep thresholds and report shapes. Their SQL becomes developer material or explanatory provenance; the default playbooks do not instruct the model to execute it.

The startup prompt remains: “Give me the executive briefing of active and recent sessions, then the top skill and agent findings.” A test harness will enforce that this path reads the file and performs no trace tool or shell call.

## 9. Compatibility and rollout

- Schema-v1 `briefing` hydration remains valid for other packages.
- `$AGENT_MUX_BRIEFING` keeps its name; consumers must inspect `schema_version`.
- `AGENT_MUX_BRIEFING_SCHEMA` is additive and lets prompts explain mismatches without opening the file first.
- Existing MCP tools and CLI commands retain their contracts. The new agent analysis API can later back a typed MCP tool, but adding that tool is not required for the startup-speed objective.
- `trace agents` may be refactored to render from the new analysis rows while preserving its current all-store CLI output.
- A shadowed `~/.agent-mux/skills/heimdall/` package continues to win. If it still requests `briefing`, it receives schema v1; only the updated built-in package requests `dossier`.

## 10. Error handling

Errors are classified and serialized, never flattened into missing data:

- `DB_UNAVAILABLE`: no dossier sections can be queried;
- `SCHEMA_UNSUPPORTED`: store is newer or missing required views;
- `SECTION_QUERY_FAILED`: one section failed; other sections remain usable;
- `INVENTORY_UNAVAILABLE`: store metrics remain, definitions and lint do not;
- `LIVE_SNAPSHOT_UNAVAILABLE`: stored session data remains;
- `CONTENT_WITHHELD`: counts and timings remain, snippets do not;
- `QUERY_TIMEOUT`: section stops at its deadline;
- `DOSSIER_TOO_LARGE`: minimal valid dossier cannot fit the hard budget;
- `SNAPSHOT_WRITE_FAILED`: launch continues and the TUI reports the write failure.

Warnings distinguish incomplete evidence from zero values. For example, no recorded failures is `failures = 0`; an unavailable failure query is an error with the field omitted.

## 11. Verification

### Unit tests

- Session and evaluation windows are derived from one fixed `as_of` instant.
- Skill joins cover stored-only, definition-only, duplicate harness definitions, unused loads, missed triggers, errors, percentiles and lint findings.
- Agent rows cover zero invocations, failure propagation from children, direct-child tool counts, p50/p90 nearest rank, recurring failures and maximum session cost share.
- Workspace and time-window scoping excludes unrelated records.
- Rankings and tie breakers are deterministic.
- Example caps do not alter totals or sample sizes.
- Byte-budget reduction follows the documented order and always emits valid JSON with truncation metadata.
- One section failure preserves other sections.

### Integration tests

- A seeded store produces one schema-v2 dossier with the expected 24-hour and 30-day sections.
- The builder API accepts the already-open connection, and section tests run through that connection; database opening and path resolution remain exclusively in hydration.
- A fake Claude, Codex and Antigravity launch receives the same dossier path, schema environment variable and prompt hint.
- Snapshot files remain owner-only, atomic, removed on exit and swept after 24 hours.
- A missing database produces a typed top-level error and does not block launch.
- Metadata content mode omits snippets but retains counts, timing, tokens, cost and limitations.
- The installed or built-in Heimdall package asks for `dossier`; an older or custom `briefing` package still receives schema v1.

### Behavioral acceptance test

A fake harness launches built-in Heimdall with a seeded store and records every attempted shell or MCP call. Given the default startup prompt, the fixture response must be supportable entirely from the dossier, and the call log must remain empty. A second prompt asking for a newer timeline is allowed to invoke the corresponding MCP tool.

### Performance test

A deterministic fixture with at least 100 sessions, 100 skills, 100 agent types and representative observations records dossier build duration and size. The test asserts the configured size bound and guards against accidental per-row queries using statement or query counters. Wall-clock performance is reported in benchmarks, not asserted tightly in CI.

## 12. Documentation changes

Update:

- `AGENTS.md`: Heimdall is hydrated with a Rust dossier, not only a briefing snapshot;
- `README.md`: launch data flow, schema version and tool-use boundary;
- `docs/skills.md`: `dossier` hydration contract and backward-compatible `briefing`;
- tracing analysis documentation: dossier types, windows, coverage and limits;
- `skills/heimdall/SKILL.md` and all three reference files;
- `src/prompts.toml`: the schema-v2 hydration hint.

## 13. Acceptance criteria

1. Built-in Heimdall requests `hydrate = ["dossier"]`.
2. One Rust builder produces session, skill, agent and health facts from one read-only database connection and one live-snapshot read.
3. Sessions cover 24 hours; skills and agents cover 30 days; exact bounds are serialized.
4. The snapshot is typed, deterministic, private, atomic and bounded.
5. Partial section failures are explicit and do not discard successful sections.
6. The default startup response requires no MCP or CLI call.
7. MCP and CLI remain the path for fresher, narrower or out-of-scope questions.
8. Schema-v1 briefing hydration and existing tool contracts remain compatible.
9. Tests cover facts, scoping, truncation, compatibility, lifecycle and the no-call startup behavior.
10. `cargo fmt`, `cargo clippy --all-targets`, `cargo test` and `cargo build` pass, aside from independently documented pre-existing failures in the dirty baseline.
