# Markdown Agents and Correct Trace Analytics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace agent-specific Rust behavior with portable agent packages and reliable shared analytics.

**Architecture:** Extract reusable analysis from src/heimdall.rs into tracing/analysis. Parse AGENTS.md packages, render harness-specific artifacts, and route all agents through one launcher and capability-selected preview.

**Tech Stack:** Rust 2024, existing Harness/LaunchOptions and rusqlite/serde/toml; add a maintained YAML parser and SHA-256 implementation after dependency checks.

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

## Execution map and repository context

Baseline code: `8ed92d4`; spec revision includes the Markdown-only correction committed as `5644bb8`. Inspect HEAD before execution.

This plan is milestone 1. [Milestone 2](2026-09-14-agent-mcp.md) depends on all its tasks. [Milestone 3](2026-09-14-agent-monitoring.md) is separate and optional.

Reuse `src/harness.rs` (`Harness`, `LaunchOptions`, `Resume`), `src/tracing/store/query.rs`, and `src/tracing/experiments.rs`. Existing `Harness::as_str()` returns `agy`; API provider serialization must normalize it to `antigravity`. Existing `PtyExit` events may duplicate and arrive before final output; keep handling idempotent.

### File ownership

| Path | Responsibility |
| --- | --- |
| `src/agent.rs`, `src/agent/definition.rs`, `discovery.rs` | Public facade, typed package parsing and deterministic discovery |
| `src/agent/artifacts.rs`, `src/agent/adapters/{mod,claude,codex,agy}.rs` | Manifest, renderer and native artifact mapping |
| `src/agent/launch.rs`, `install.rs`, `cli.rs` | Generic launch, owned install/migration and commands |
| `agents/heimdall/AGENTS.md` | Normal shipped agent source, read from disk |
| `src/tracing/analysis/{mod,model,correlation,evidence,query,metrics}.rs` | Generic facts and evidence; no persona |
| `src/app.rs`, `src/ui.rs`, `src/session.rs`, `src/persistence.rs` | Generic selection, identity, launch and preview integration |
| `tests/agent_*.rs`, `tests/trace_analysis.rs` | Package, artifact, launch and factual regressions |

### Shared test convention

Code blocks below define representative tests against proposed APIs, not claims that these APIs already exist. Put pure tests next to their functions; integration fixtures belong in `tests/support/analysis.rs`. The fixture helper `seed_store(sql: &str) -> tempfile::TempDir` must create `traces.db` through existing `store::open_rw` with retention disabled and execute literal fixture SQL. Never bypass migrations with a handcrafted production schema.

### Task 1: Define validated AGENTS.md packages

**Files:** Modify `src/agent.rs`, `src/harness.rs`, `Cargo.toml`, `Cargo.lock`; create `src/agent/definition.rs`, `tests/agent_definitions.rs`.

**Interfaces:** `parse_definition(source: &str, path: &Path) -> Result<AgentDefinition, DefinitionError>`; extend existing AgentDefinition with `startup_task: Option<String>`, `capabilities: Vec<String>`, `mcp_servers: Vec<String>`, `source_hash: String`. Existing instructions remain the Markdown body. Define `DefinitionError` with file, field and message.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn declared_default_must_be_supported() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex]\ndefault_harness: agy\n---\nReview changes.";
    assert!(parse_definition(source, Path::new("audit/AGENTS.md")).is_err());
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_definitions
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Use a typed YAML document, then validate nonempty safe IDs (`[a-z0-9][a-z0-9_-]*`), body, unique harnesses and supported defaults. A missing default selects the first supported harness. Known malformed values are errors; unknown keys are warnings. Keep an explicit legacy parser entrypoint for old flat files. Define optional trigger data now as a validated opaque YAML value; milestone 3 replaces it with a typed trigger schema before it can execute.

```rust
if !definition.harnesses.contains(&definition.default_harness) {
    return Err(DefinitionError::field(path, "default_harness", "must be in harnesses"));
}
```

Add cases for multiline YAML, quoted colons, duplicate harnesses, invalid IDs/path traversal, empty instructions, missing default and unknown fields. Hash canonical source bytes; no time-dependent hash input.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Confirm existing flat-file parsing remains supported through the compatibility entrypoint; preserve original user instructions verbatim.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: parse validated AGENTS.md agent packages`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 2: Discover, migrate and ship data-only agents

**Files:** Create `src/agent/discovery.rs`, `agents/heimdall/AGENTS.md`, `tests/agent_discovery.rs`; modify `src/agent.rs`, `Cargo.toml`; create `docs/agents.md`.

**Interfaces:** `discover_agents(workspace: &Path, global: &Path, bundled: Option<&Path>) -> DiscoveryReport`; report contains `agents: Vec<AgentDefinition>` and diagnostics. `migrate_legacy(source: &Path, destination: &Path) -> Result<(), MigrationError>` copies without overwriting.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn migration_preserves_legacy_source() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("audit.md");
    std::fs::write(&source, "---\nid: audit\n---\nReview changes.").unwrap();
    let destination = root.path().join("audit/AGENTS.md");
    migrate_legacy(&source, &destination).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), std::fs::read(destination).unwrap());
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_discovery
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Scan canonical packages and legacy files explicitly. Canonical overrides legacy within a root; workspace overrides global; global overrides bundled. Reject duplicate canonical IDs in one root with deterministic diagnostics. Never scan generated directories as packages.

```text
resolution precedence:
workspace canonical > workspace legacy > global canonical > global legacy > bundled canonical
```

Ship `agents/heimdall/AGENTS.md` as a filesystem asset; runtime searches an explicit configured bundle root and an installation share directory, with the repository assets used only for development. Missing assets produce diagnostics, not compiled fallback personas. Include assets in packaging and document installing binary plus share directory. Test unknown agent IDs, absent bundled assets, read-only roots, duplicate IDs, migration conflicts and no implicit writes during discovery.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Ensure legacy originals survive migration and existing custom Heimdall definitions are never reseeded over.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: discover and migrate data-only agent packages`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 3: Build deterministic artifacts for every harness

**Files:** Create `src/agent/artifacts.rs`, `src/agent/adapters/{mod,claude,codex,agy}.rs`, `tests/agent_artifacts.rs`, `tests/fixtures/agent-adapters/`; modify `src/agent.rs`.

**Interfaces:** `render_artifacts(definition: &AgentDefinition, source: &Path) -> Result<ArtifactSet, ArtifactError>`; `ArtifactSet { files: BTreeMap<PathBuf, Vec<u8>>, source_hash: String }`. `write_artifacts(root: &Path, set: &ArtifactSet) -> Result<(), ArtifactError>`. Renderer version and hashes go in manifest.json; `source_hash` is not exposed as instructions.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn arbitrary_agent_gets_all_three_artifact_sets() {
    let d = parse_definition("---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes.", Path::new("audit/AGENTS.md")).unwrap();
    let a = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let b = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(a.files, b.files);
    for path in ["claude/agent.md", "codex/AGENTS.md", "agy/agent.md"] {
        assert!(a.files.contains_key(Path::new(path)));
    }
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_artifacts
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Produce exactly the staging layout in spec section 4, plus manifest. Every native instruction artifact derives from the source body; generic adapter metadata may differ. Capture non-mutating `claude --version/--help`, `codex --version/--help`, and `agy --version/--help` into reviewed fixture files via apply_patch at execution. Verify native configuration contracts with official documentation before rendering native fields. Mark unavailable compatibility as unverified; do not invent flags.

```rust
let manifest = serde_json::json!({
    "schema_version": 1,
    "generator_version": 1,
    "agent_id": definition.id,
    "source_hash": definition.source_hash,
    "enabled_harnesses": definition.harnesses.iter().map(Harness::as_str).collect::<Vec<_>>()
});
```

Add per-file hashes and adapter versions to this manifest, and omit volatile timestamps. Build into a sibling staging directory, fsync where supported, and publish a coherent generation using an atomic pointer/rename strategy with recovery after interruption. Refuse modified owned files; preserve unowned files. Test Unicode, empty MCP fragments, disabled harnesses, source changes, drift, crash recovery and deterministic hashes.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Verify no generated file embeds secrets or live trace snapshots and no template is selected by agent ID.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: generate versioned native agent artifacts`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 4: Unify launch, installation and agent identity

**Files:** Create `src/agent/launch.rs`, `src/agent/install.rs`, `src/agent/cli.rs`, `tests/agent_launch.rs`, `tests/agent_install.rs`; modify `src/app.rs`, `src/harness.rs`, `src/main.rs`, `src/session.rs`, `src/persistence.rs`, `src/tracing/mod.rs`.

**Interfaces:** `build_agent_launch(definition: &AgentDefinition, profile: &Profile, options: &LaunchOptions, workspace: &Path, artifacts: &ArtifactSet) -> Result<AgentLaunch, LaunchError>`; `AgentLaunch { profile: Profile, cwd: PathBuf, env: Vec<(String,String)>, agent_id: String, source_hash: String, diagnostics: Vec<String> }`. `selected_harness_index(&AgentDefinition) -> usize` is used by the generic picker.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn picker_honors_source_default() {
    let d = parse_definition("---\nid: audit\nharnesses: [claude, codex, agy]\ndefault_harness: agy\n---\nReview changes.", Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(selected_harness_index(&d), 2);
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_launch
cargo test --test agent_install
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Parse recognized profile options structurally before composition. Preserve unknown flags or return a diagnostic if their arity makes merging ambiguous; never discard them. Supply instructions and a distinct source-defined startup task on every harness, retaining native trust and approvals. Avoid duplicate tracing injection by continuing through `spawn_traced`. Use native supported instruction mechanisms verified in Task 3, not root AGENTS.md replacement.

```text
effective options = explicit launch override > supported agent field > profile > native default
attachment identity = agent_id + provider + workspace + active launch_id
```

Persist agent identity/source hash with launch metadata and session persistence; old saved sessions lacking identity remain legacy rather than matching by name. Include needs-attention sessions in attachment. Implement build, doctor, migrate, install and uninstall subcommands. Install only owned adapter artifacts/config entries, preserve unrelated entries, record prior values and roll back partial failures. Do not mutate user settings on ordinary launch.

Test all three argv vectors using fake executables, profile flags, fresh/resume conflicts, spaces/metacharacters, startup task delivery, source reload, unsupported harnesses, busy attachment without prompt injection, config conflicts, repeat installs and owned-only uninstall.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Run existing launch/persistence tests too: `cargo test --test persistent_sessions` and `cargo test --lib harness`.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `feat: launch and install all agents through generic adapters`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 5: Create evidence types and exact session correlation

**Files:** Create `src/tracing/analysis/{mod,model,correlation}.rs`, `tests/trace_analysis.rs`, `tests/support/analysis.rs`; modify `src/tracing/mod.rs`.

**Interfaces:** `RuntimeState` and `TaskOutcome` use spec enums. `Evidence<T> { value: Option<T>, source: EvidenceSource, observed_at: Option<String>, confidence: Confidence, evidence_ids: Vec<String>, limitations: Vec<String> }`. `resolve_binding(conn: &Connection, run_id: &str, local_id: usize, exact_launch: Option<&str>, native_key: Option<&str>) -> Result<Option<Binding>, AnalysisError>`; `Binding { launch_id: String, session_key: Option<String> }`.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn absence_of_exact_identity_stays_uncorrelated() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
      INSERT INTO launches VALUES('old','previous-run',1,'claude:old',1);").unwrap();
    assert!(resolve_binding(&db, "current-run", 1, None, None).unwrap().is_none());
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test trace_analysis
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Never use cwd to bind a session. Validate explicit native keys against provider parsing; contradictory exact identities return a correlation error. Define `AnalysisError` variants matching the future service error codes, plus an internal correlation error mapped to partial coverage. Treat absent or incomplete timing/usage as null, not zero.

```sql
SELECT id, session_key FROM launches
WHERE run_id = ?1 AND agent_mux_session = ?2
ORDER BY started_ns DESC LIMIT 1;
```

Production queries and integration fixtures use the real migrated schema; the small table above isolates the run-ID regression. Add two mux runs sharing local IDs, concurrent same-directory sessions, different providers, conflicting bindings and a launch not yet bound to a native session. Derive liveness from exact process snapshots, not open spans.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Confirm no cwd-only SQL appears in binding code and all provided evidence IDs resolve to the same bound session.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `fix: correlate trace summaries by exact launch identity`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 6: Normalize tool evidence and compute uncapped recaps

**Files:** Create `src/tracing/analysis/evidence.rs`, `src/tracing/analysis/query.rs`; extend `model.rs`, `tests/trace_analysis.rs`; reuse `src/tracing/store/query.rs`.

**Interfaces:** `snippet(text: &str, max_chars: usize) -> String`; `briefing(conn: &Connection, workspace: &Path, since_ns: i64, until_ns: i64, live: &[LiveSession]) -> Result<Briefing, AnalysisError>`. Define `LiveSession` with run/launch/session identity, runtime state and timestamp; define `Briefing` with cards and exact session/window totals. Cards contain spec section 6 fields, distinct session/launch totals, null coverage, active-tool previews and counts.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn snippets_are_unicode_safe() {
    assert_eq!(snippet("á🦀日本語", 3), "á🦀日…");
    assert_eq!(snippet("ok", 3), "ok");
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test trace_analysis
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Use typed provider messages for first user request; strip complete preamble blocks only as fallback. Normalize command/cmd/CommandLine and patch records. Count before pagination; retain relative paths and distinguish attempted edits, successful tool results and independently confirmed changes. Merge history overlapping the window with exact live launches; do not duplicate session totals.

```rust
pub fn snippet(text: &str, max_chars: usize) -> String {
    let mut iter = text.chars();
    let mut out: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() { out.push('…'); }
    out
}
```

Use dedicated totals queries instead of summing top-five categories. Seed 7 tool names, 12 distinct paths including same basenames, 9 commands, failed edits, multiple active tools, late output and an older still-live session. Screen fields remain separate heuristic evidence; metadata-only mode suppresses content. Dead/open spans show stale evidence; clock skew clamps elapsed duration and adds a warning.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Assert exact totals independently of preview limits and historical inclusion while a live session exists.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `fix: produce complete trace recaps with qualified evidence`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 7: Correct skill and usage metrics

**Files:** Create `src/tracing/analysis/metrics.rs`; extend `model.rs`, `tests/trace_analysis.rs`; inspect `src/tracing/usage.rs`, `pricing.rs`, `store/query.rs`.

**Interfaces:** `completed_percentiles(durations: &[Option<u64>]) -> Option<(u64,u64,u64,usize)>` returns p50/p95/max/count using nearest-rank percentiles. `ttft_ms(request_ns: Option<i64>, first_token_ns: Option<i64>) -> Option<u64>`. Skill rows expose loaded/attributed counts, coverage, classified errors and ongoing spans separately.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn generation_duration_is_not_ttft() {
    assert_eq!(ttft_ms(Some(1_000_000), None), None);
    assert_eq!(ttft_ms(Some(1_000_000), Some(4_000_000)), Some(3));
    assert_eq!(completed_percentiles(&[None, Some(10), Some(30)]), Some((10,30,30,2)));
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test trace_analysis
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Aggregate measured generation usage without adding parent-agent inclusive totals twice. Use existing normalized provider usage semantics and pricing provenance. Keep unpriced/missing usage counts beside partial sums. Open observations are excluded from completed percentiles. Label no attribution literally; do not estimate wasted instruction tokens. Classify schema errors only from provider/error evidence and retain unknown categories.

```rust
pub fn ttft_ms(request_ns: Option<i64>, first_token_ns: Option<i64>) -> Option<u64> {
    let (start, first) = (request_ns?, first_token_ns?);
    if first < start { return None; }
    Some(((first - start) / 1_000_000) as u64)
}
```

Test no samples, ongoing-only tools, one sample, errors unrelated to schemas, nested agents, mixed pricing coverage and provider attribution gaps.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Require no TTFT value without first-token timing and no complete-bill label for partial pricing.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `fix: report measured analytics without unsupported diagnoses`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

### Task 8: Remove agent-specific Rust and wire generic previews

**Files:** Modify `src/app.rs`, `src/ui.rs`, `src/lib.rs`, `src/events.rs`, `src/agent.rs`; delete `src/heimdall.rs` after migration; move `tests/heimdall_agents.rs` to `tests/agent_ui.rs`; extend `docs/agents.md`.

**Interfaces:** Generic `AgentLauncherState` replaces HeimdallLauncherState. Preview selection tests `capabilities.contains("trace.read")`, not identity. Define `AnalysisUpdated { revision: u64, result: Result<Briefing,String> }` in AppEvent; the view reads cached facts. Reuse Tasks 4–7.

- [ ] **Step 1 — Add the regression/contract test.** Include this test and the additional cases listed in Step 3.

```rust
#[test]
fn runtime_has_no_agent_named_module() {
    let lib = std::fs::read_to_string("src/lib.rs").unwrap();
    assert!(!lib.contains("pub mod heimdall"));
    assert!(!std::path::Path::new("src/heimdall.rs").exists());
}
```

- [ ] **Step 2 — Verify the test fails for the missing behavior.**

```sh
cargo test --test agent_ui
```

Expected before implementation: a missing proposed API or the stated assertion fails. Fix fixture errors before changing production code.

- [ ] **Step 3 — Implement the smallest complete deliverable.**

Replace special launch/preview branches and compiled personas. Retain neutral UI labels such as session totals; persona and output narrative live only in the Markdown asset. Spawn a coalesced blocking query worker outside rendering; feed immutable results into AppEvent. A pending flag admits only one refresh and the latest desired revision. Retain successful cache on errors with its timestamp.

```text
Tick + visible trace-capable preview + >=1s + no pending query
  -> capture live values -> queue query -> AnalysisUpdated
Render -> read cached Briefing only
```

Test a newly added audit agent with trace.read obtains the same generic preview, an agent with no capability gets a definition preview, stale errors preserve cached facts, and rendering never opens SQLite. Document migration/installation and ordinary binary-plus-assets packaging.

- [ ] **Step 4 — Verify the deliverable.** Rerun the command above. Run `cargo test --test agent_ui`, `cargo test --test app_flow`, `cargo test --test persistent_sessions`, then milestone gate below.

- [ ] **Step 5 — Review the scoped diff and commit only this task's files.** Suggested commit: `refactor: remove Heimdall-specific runtime and UI paths`. Use explicit paths with `git add`; run `git diff --cached --check` before committing.

## Milestone 1 exit gate

- [ ] Run `cargo test` and `cargo fmt --all -- --check`; compare any pre-existing formatting issues to baseline, do not reformat unrelated modules.
- [ ] Run `rg -n 'HeimdallHarness|launch_heimdall|DEFAULT_HEIMDALL|pub mod heimdall' src`; expect no matches. Inspect remaining case-insensitive Heimdall references for embedded behavior.
- [ ] Build once, add `audit/AGENTS.md` in a temporary package root, generate all artifacts, and launch through fake harnesses without rebuilding.
- [ ] Save a compatibility record in `docs/agent-harness-compatibility.md` identifying actual probed versions and unverified ones.
- [ ] Confirm shipped Heimdall Markdown contains its mission and startup task, while Rust contains only generic capabilities.
- [ ] Review task diffs and acceptance mapping before starting milestone 2.

## Spec coverage

Sections 1–4: Tasks 1–4, 8. Sections 5–6: Tasks 5–7. Section 7 TUI cache: Task 8. Section 9 owned artifacts: Tasks 3–4. Section 11 evidence policy: Tasks 5–7. Section 12 architecture and milestone 1 acceptance: Task 8 and exit gate. Shared MCP limits/live publisher are delivered in milestone 2, not silently omitted.
