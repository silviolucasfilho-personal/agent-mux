# Generic Agent Monitoring and Durable Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in, bounded event-triggered investigations and review memory for any Markdown-defined agent.

**Architecture:** A generic watcher consumes committed trace changes and source-defined triggers. Derived state lives in agent-state.db; investigations use the same read-only MCP tools through versioned managed harness adapters. The agent ID namespaces state, never selects behavior.

**Tech Stack:** Rust, existing SQLite migration/writer pipeline, Tokio, structured child-process protocols and the MCP service from milestone 2.

**Spec:** [Heimdall and generic agent service design](../specs/2026-09-14-heimdall-agent-service-design.md)

## Global Constraints

- Every agent is defined completely by an `AGENTS.md` source and has generated artifacts for each harness.
- No agent may require an agent-specific Rust file, implementation branch, embedded persona, or compiled registration.
- Adding, changing, or removing an agent must require only Markdown/configuration changes and artifact regeneration, never rebuilding agent-mux.
- Do not overwrite a user's repository-root `AGENTS.md` to activate an agent.
- All v1 tools are read-only and carry corresponding MCP annotations.
- List limit defaults to 20 and is capped at 100.
- Default query deadline: 2 seconds; analytics/comparison: 5 seconds.
- Maximum concurrent requests: 4; bounded waiting queue: 16.
- Maximum serialized response: 64 KiB. Cursors expire after ten minutes.
- Publish live snapshots every second; after five seconds without a heartbeat treat live state as stale, not exited.
- Automatic model investigation is disabled by default.
- Do not add operational write tools, remote transport, hard-coded agent workflows, or another trace ingestion pipeline.
- Preserve unrelated work. At execution, inspect current instructions/status and use an isolated checkout when appropriate. Do not execute this plan merely because it has been written.
- Native flags, installation paths and supported versions must be verified against installed binaries and official documentation before implementing an adapter. Never guess capability support.
- Tests run against temporary databases/configuration roots. Live harness tests are opt-in and must report unavailable harnesses as unverified.

---

## Dependency and file map

Requires [milestone 1](2026-09-14-markdown-agents.md) and [milestone 2](2026-09-14-agent-mcp.md). This is optional milestone 3; do not delay the interactive MCP release for it. Runtime defaults keep automatic model calls disabled.

| Path | Responsibility |
| --- | --- |
| `src/tracing/store/{schema,mod}.rs` | Additive committed-change journal |
| `src/agent/{state,triggers,watch,budgets}.rs` | Review records, declarative events, scheduling and enforcement |
| `src/agent/managed/{mod,codex,claude,agy}.rs` | Version-specific structured process adapters |
| `src/agent/{definition,cli}.rs`, `src/app.rs`, `src/ui.rs` | Validated source triggers, explicit controls and review acknowledgment |
| `tests/{trace_changes,agent_state,agent_triggers,agent_watch,managed_agents}.rs` | Restart, dedupe, budget and adapter tests |
| `docs/agent-monitoring.md` | Opt-in workflow and operational limits |

No paid/native harness execution is needed to implement deterministic scheduling tests. Use scripted child processes and a fake clock; run actual harness verification explicitly only after core tests pass.

### Shared interfaces

Task 1 defines `Change { seq: i64, entity_kind: String, entity_id: String, session_key: Option<String>, launch_id: Option<String>, operation: String }`.
Task 2 defines `AgentScope { agent_id: String, source_path: PathBuf, workspace: PathBuf }`, `StateStore`, `Job`, `JobStatus`, and `BriefingRecord`.
Task 3 defines `TriggerDefinition`, `TriggerEvent`, `Finding` and `JobRequest`.
Task 4 defines `ManagedCapabilities`, `ManagedRequest`, `ManagedEvent`, `ManagedSession` and `ManagedAdapter`.
Task 5 composes them. All proposed types belong to the files listed above, not to an agent-named module.

### Task 1: Journal committed trace changes

**Files:** Modify `src/tracing/store/schema.rs`, `src/tracing/store/mod.rs`; create `tests/trace_changes.rs`.

**Interfaces:** `read_changes(conn: &Connection, after_seq: i64, limit: usize) -> rusqlite::Result<Vec<Change>>`; `latest_change_seq(conn: &Connection) -> rusqlite::Result<i64>`. Migration is appended after the current version (baseline is 10; recheck at execution).

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn rolled_back_changes_do_not_advance_review_sequence() {
    let root = seed_store("");
    let conn = rusqlite::Connection::open(root.path().join("traces.db")).unwrap();
    let before = latest_change_seq(&conn).unwrap();
    conn.execute_batch("BEGIN;
        INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
        VALUES('claude:rollback','claude','rollback',1,1);
        ROLLBACK;").unwrap();
    assert_eq!(latest_change_seq(&conn).unwrap(), before);
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test trace_changes
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Reuse milestone 1's `seed_store` fixture helper, which runs store::open_rw migrations. Extend the rollback test with canonical observation writes. Append a change-journal table and trigger/journal writes for sessions, launches, traces and observations. Capture insert, meaningful update and delete, including a session/launch locator before deletion. Journal and canonical write share the same transaction; readers see neither until commit. Assign a persistent random trace-store UUID in meta during this writer migration if none exists; state consumers use it to distinguish databases, never the pathname alone.

```sql
CREATE TABLE trace_changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  entity_kind TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  session_key TEXT,
  launch_id TEXT,
  operation TEXT NOT NULL CHECK(operation IN ('insert','update','delete'))
);
```

Avoid journaling no-op upserts by comparing persisted values. Existing rows form an initial baseline scan; bootstrap watcher scan and latest sequence in one read transaction, then consume strictly newer changes. Retain journal entries initially; document growth and do not prune past consumers silently. Test late observation updates with old timestamps, rollback, concurrent reader/writer, no-op upserts and retention deletes.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Run existing trace-store/import tests; confirm shipped migrations are unchanged and the read-only MCP server never performs the migration.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: journal committed trace changes for review cursors`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 2: Persist review state independently of traces

**Files:** Create `src/agent/state.rs`, `tests/agent_state.rs`; modify `src/agent.rs`.

**Interfaces:** `StateStore::open(path: &Path) -> Result<Self, StateError>`; `record_briefing(scope: &AgentScope, through_seq: i64, text: &str, evidence_ids: &[String]) -> Result<String, StateError>`; `acknowledge(scope: &AgentScope, briefing_id: &str) -> Result<(), StateError>`; `reviewed_through(scope: &AgentScope) -> Result<i64, StateError>`. Define StateError with missing record, scope conflict and database failure variants.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn generating_a_briefing_does_not_acknowledge_it() {
    let root = tempfile::tempdir().unwrap();
    let mut state = StateStore::open(&root.path().join("agent-state.db")).unwrap();
    let scope = AgentScope { agent_id: "audit".into(), source_path: "/agents/audit/AGENTS.md".into(), workspace: "/work".into() };
    let id = state.record_briefing(&scope, 42, "Observed progress", &[]).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 0);
    state.acknowledge(&scope, &id).unwrap();
    assert_eq!(state.reviewed_through(&scope).unwrap(), 42);
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_state
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Create separate schema metadata, review cursors, briefings, findings and jobs. Namespace by agent ID, normalized canonical source path and workspace; store source hash on each record as a revision, not part of namespace, so instruction updates retain review history. Also bind records to canonical trace DB identity to prevent mixing two trace stores beside one state DB.

```text
acknowledge(scope, briefing_id):
  begin transaction
  verify briefing belongs to scope and trace store
  reviewed_through = max(current, briefing.through_seq)
  mark that briefing acknowledged
  commit
```

Jobs store request/evidence revision, dedupe key, status, attempts, native session identity and budget usage. Persist before process creation. On startup transition running jobs to interrupted; never auto-replay them. Findings have open/acknowledged/resolved states and first/last sequence. Test crash restart, wrong-scope acknowledgment, old acknowledgments not regressing cursors, instruction edits and harness changes preserving state, and corrupted state DB leaving manual MCP usable.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Verify state writes do not modify traces.db and reads do not advance a cursor.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: persist generic agent findings and review progress`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 3: Parse source-defined triggers and detect findings

**Files:** Create `src/agent/triggers.rs`, `src/agent/budgets.rs`, `tests/agent_triggers.rs`; modify `src/agent/definition.rs`, `agents/heimdall/AGENTS.md`.

**Interfaces:** `TriggerDefinition { event: EventKind, prompt: String, debounce_ms: u64 }`; EventKind includes process_exit, waiting_for_user, repeated_error, collector_stale. `should_trigger(observer: &str, origin_agent: Option<&str>, ancestor_agents: &[String]) -> bool`. `evaluate_changes(changes: &[Change], definition: &AgentDefinition) -> Vec<Finding>` consumes measured/classified context; use source rules, never agent IDs.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn watcher_does_not_trigger_on_its_descendants() {
    assert!(!should_trigger("audit", Some("audit"), &[]));
    assert!(!should_trigger("audit", Some("helper"), &["audit".into()]));
    assert!(should_trigger("audit", Some("builder"), &[]));
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_triggers
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Replace opaque trigger YAML with a closed schema and document it. Extend AgentDefinition with `triggers: Vec<TriggerDefinition>` and `monitoring: MonitoringConfig`; MonitoringConfig has `automatic: bool`, `max_jobs_per_hour: u32`, `max_concurrent_jobs: u32`, `timeout_seconds: u64`, `max_turns: u32` and optional token/cost budgets. Its default is disabled with the bounds below. Source-defined prompts and selection rules supply behavior. Default triggers debounce 5000ms; repeated_error requires a configurable count (default 3) of the same classified tool/error category within one session; collector_stale indicates missing evidence, not failed work. Resolve finding keys from scope + exact session + event kind + classification, and revisions from committed change sequence.

```yaml
triggers:
  - event: repeated_error
    prompt: "Inspect the repeated error evidence and report its likely cause with citations."
    debounce_ms: 5000
monitoring:
  automatic: false
  max_jobs_per_hour: 3
  max_concurrent_jobs: 1
  timeout_seconds: 120
  max_turns: 3
```

Put persona-specific trigger choices in Heimdall's source asset, not defaults for every agent. Generic parsed defaults apply only when monitoring is configured. Test unrelated agent IDs with identical declarations produce equivalent findings, malformed budgets, zero/negative limits, unknown event names, duplicate exit notifications and no activity without collection.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Assert deterministic findings require no model calls and thresholds are included in evidence.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: evaluate declarative agent triggers and budgets`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 4: Implement bounded managed adapters

**Files:** Create `src/agent/managed/{mod,codex,claude,agy}.rs`, `tests/managed_agents.rs`, `tests/fixtures/managed/`; modify `src/agent.rs`, `src/agent/budgets.rs`, compatibility docs.

**Interfaces:** `ManagedCapabilities { cancel: bool, max_turns: bool, token_budget: bool, cost_budget: bool, resume: bool }`; `validate_budget(caps: &ManagedCapabilities, budget: &Budget) -> Result<(), BudgetError>`. `Budget { max_turns: u32, timeout_seconds: u64, max_tokens: Option<u64>, max_cost_usd: Option<f64> }`. `ManagedAdapter::start(request: ManagedRequest) -> Future<Result<ManagedSession, ManagedError>>`; ManagedSession exposes `next_event` and `cancel` async methods.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn unsupported_enforcement_rejects_enablement() {
    let caps = ManagedCapabilities { cancel: true, max_turns: false, token_budget: false, cost_budget: false, resume: true };
    let budget = Budget { max_turns: 3, timeout_seconds: 120, max_tokens: None, max_cost_usd: None };
    assert!(validate_budget(&caps, &budget).is_err());
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test managed_agents
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

ManagedRequest contains definition/source hash, startup prompt, scoped MCP config, Budget and optional exact native session identity. ManagedEvent variants are Started, TurnStarted, Usage, Evidence, Completed, Failed; expose only fields supported by verified provider events. Use structured subprocess protocols with explicit process-group ownership, bounded stdout/stderr parsing and cancellation cleanup; never send simulated PTY keystrokes.

```text
start -> parse structured events -> account actual usage and turns
      -> completion | error | deadline | budget exceeded
deadline/budget exceeded -> native cancel if supported -> bounded shutdown -> kill owned child group
```

Verify candidate protocols at execution: Codex App Server, Claude structured CLI/SDK, AGY structured streaming. Reuse native authentication configuration rather than assuming all SDKs share CLI login. Document exact versions, event mapping, cancellation and resume behavior. If a provider cannot enforce model/tool-loop turns, disable automatic mode for that adapter rather than calling outer prompts 'turns'. Cost caps based on delayed usage must expose lag and cannot be advertised as strict spend limits.

Script fake child processes for malformed events, large frames, auth errors, duplicate completions, no output, cancellation, descendants, usage lag and resume IDs. Native smoke runs are explicit and recorded, not required for ordinary unit tests.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Ensure a timed-out process tree is actually terminated and unsupported budgets fail before starting a model.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: add capability-checked managed harness adapters`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 5: Schedule opt-in jobs with restart-safe controls

**Files:** Create `src/agent/watch.rs`, `tests/agent_watch.rs`; modify `src/agent/cli.rs`, `src/app.rs`, `src/ui.rs`.

**Interfaces:** `Watcher::tick(now_ns: i64) -> Result<(), WatchError>` composes StateStore, change reader, trigger evaluator and injected ManagedAdapter. `watch start/stop/status`, `mark-reviewed <briefing-id>` and `retry <job-id>` are explicit CLI/TUI commands scoped by agent and workspace. No MCP write API is added.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn default_monitoring_does_not_launch_models() {
    let source = "---\nid: audit\nharnesses: [codex]\n---\nInspect progress.";
    let d = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert!(!d.monitoring.automatic);
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_watch
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Use injected fake clock and adapter launch counters in scheduler tests. One watcher lease per agent/source/workspace/trace-store scope prevents duplicate processes scheduling paid jobs. A job is deduplicated by finding/evidence revision; refreshed evidence is coalesced through the five-second debounce. Use sliding one-hour admissions, one running job, three launches/hour defaults. Persist pending/running before launch; attempt counts include failed/auth launches to avoid repeated retries.

```text
tick:
  read committed changes after consumption cursor
  upsert findings + advance consumption cursor atomically in agent-state.db
  if automatic disabled: return
  acquire/renew watcher lease
  select due deduplicated request within admission and adapter capabilities
  persist running state, start child, observe events, persist final state
```

Keep consumption cursor separate from user review cursor. Recover interrupted jobs without re-execution; retry is an explicit action that consumes budget. Store structured investigation output with validated source IDs; missing/pruned evidence becomes a warning. Exclude watcher activity and descendants by generic lineage metadata. Handle state/trace DB failures without affecting interactive harness sessions.

Start remains foreground unless the user explicitly requests background execution. Stop targets the exact watcher/owned investigation, not other coding sessions. No automatic OS service installation. Test 20 repeated events -> one job, new revision after debounce, two watcher instances, crash between persist and spawn, auth failure, rate boundary, changed harness and source instruction update.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Confirm jobs use the same read-only MCP scope, user acknowledgment is explicit and no ordinary read advances review progress.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: schedule bounded opt-in agent investigations`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 6: Verify restart, review and budget guarantees

**Files:** Create `tests/agent_monitoring_e2e.rs`, `docs/agent-monitoring.md`; update `docs/agents.md`, compatibility records.

**Interfaces:** No new runtime API. Compose the real temporary trace/state databases with scripted managed adapters and MCP read clients.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn default_budget_is_bounded() {
    let budget = Budget::default();
    assert_eq!(budget.max_turns, 3);
    assert_eq!(budget.timeout_seconds, 120);
    assert_eq!(budget.max_cost_usd, None);
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_monitoring_e2e
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Define Budget::default consistently with Task 3. End-to-end fixture: create source-defined audit agent; ingest old-timestamp new observation; generate finding and briefing; read repeatedly through MCP; explicitly acknowledge; restart watcher; ingest correction; verify it remains unreviewed and produces one new eligible finding revision.

```sh
cargo test --test trace_changes
cargo test --test agent_state
cargo test --test agent_triggers
cargo test --test managed_agents
cargo test --test agent_watch
cargo test --test agent_monitoring_e2e
cargo test
cargo fmt --all -- --check
```

Compare trace/state database contents before and after MCP reads, excluding unrelated fixture writer activity. Test all budget stops with fake time/processes, and real cancellation per supported native adapter only in opt-in smoke runs. Document enabling, disabling, stopping, retrying, acknowledgment, stale collector messages, cost reporting lag and how to recover interrupted jobs.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Require no automatic replay, no self-trigger loop, unchanged trace data from MCP reads and preserved review state across harness changes.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `test: verify durable agent monitoring guarantees`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

## Milestone 3 exit gate and coverage

- [ ] Every automatic adapter has verified enforcement for its enabled limits; unsupported modes remain disabled.
- [ ] Default installation and ordinary launch make no background model calls.
- [ ] New agent monitoring behavior is configured entirely in its AGENTS.md.
- [ ] Late changes and corrections remain discoverable despite older timestamps.
- [ ] An interrupted job is inspectable/retryable but never replayed automatically.
- [ ] Every spec section 10 requirement maps to Tasks 1–5; milestone 3 acceptance and failure behavior map to Task 6.
- [ ] Record actual native smoke coverage before claiming managed compatibility with all three harnesses.
