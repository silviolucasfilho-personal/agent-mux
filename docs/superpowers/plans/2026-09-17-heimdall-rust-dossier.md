# Heimdall Rust Dossier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build one bounded Rust-generated startup dossier containing the 24-hour session briefing and 30-day skill and subagent facts, so Heimdall's default first answer needs no MCP or trace CLI calls.

**Architecture:** Add focused `analysis::skills`, `analysis::agents`, and `analysis::dossier` modules. Hydration opens the trace store once, reads live state and definitions once, passes the open connection into the dossier builder, and writes a schema-v2 snapshot while preserving schema-v1 `briefing` hydration. Heimdall applies thresholds and prose; Rust owns facts, scoping, evidence, coverage, deadlines and bounds.

**Tech Stack:** Rust 2024 edition, `rusqlite` 0.40 with `bundled` and `hooks`, Serde/JSON, `time`, `schemars`, Tokio integration tests, existing trace store and inventory APIs.

**Spec:** [`docs/superpowers/specs/2026-09-17-heimdall-rust-dossier-design.md`](../specs/2026-09-17-heimdall-rust-dossier-design.md)

## Global Constraints

- Do not begin implementation until the current dirty Workflow work is committed or otherwise placed on the implementation branch. `src/app.rs`, `src/skill/mod.rs`, `src/prompts.rs`, `src/prompts.toml`, `src/ui.rs`, `tests/skill_package.rs`, and documentation already contain unrelated edits; never discard or accidentally commit them.
- Use `git diff --check` and inspect `git diff -- <touched files>` before every commit. Commit only the task's changes from a clean baseline.
- Sessions use the exact 24-hour window ending at one captured `as_of`; skill and agent evaluation uses the exact 30-day window ending at the same instant.
- Dossier output is at most 64 KiB. Defaults retain at most 20 session cards, 30 skill rows, 30 agent rows, and three examples per evidence category after computing uncapped totals.
- Database access is read-only. Hydration opens one connection; dossier section functions receive `&rusqlite::Connection` and must not open or migrate a store.
- Read live snapshots once and scan inventory once per dossier.
- Facts and evidence are Rust. Thresholds, verdicts, prioritisation and prose remain in `skills/heimdall/`.
- Preserve `Hydration::Briefing`, schema-v1 snapshots, existing MCP/CLI contracts, shadow-package precedence, private atomic writes, exit cleanup, and the 24-hour sweep.
- `briefing` and `dossier` hydration are mutually exclusive package settings.
- A section failure produces typed partial coverage and an empty typed section; it must not erase successful sections or block the harness launch.
- Install a temporary SQLite progress handler for the shared five-second deadline and remove it before returning.
- Every production change follows red-green-refactor. Use real SQLite fixtures and real inventory files; do not mock query results.

## File Map

- Create `src/tracing/analysis/skills.rs`: scoped 30-day skill facts, definition joins, lint and bounded evidence.
- Create `src/tracing/analysis/agents.rs`: scoped 30-day subagent aggregates, percentiles, failures, child tools, cost share, definitions and evidence.
- Create `src/tracing/analysis/dossier.rs`: schema-v2 contract, builder, section coverage, shared deadline, deterministic ranking and 64 KiB reduction.
- Modify `src/tracing/analysis/mod.rs`: expose the three modules and their public interfaces.
- Modify `src/tracing/analysis/query.rs`: accept already-loaded live sessions and preserve uncapped totals while applying dossier card caps outside the query.
- Modify `src/tracing/inventory.rs`: expose serializable lint facts and window-aware trigger matching helpers without changing inventory precedence.
- Modify `src/skill/mod.rs`: add `Hydration::Dossier` and reject `briefing` plus `dossier` together.
- Modify `src/skill/launch.rs`: select schema-v1 briefing or schema-v2 dossier, pass home/roots, and return the snapshot schema.
- Modify `src/app.rs`: pass the definition home and export `AGENT_MUX_BRIEFING_SCHEMA`.
- Modify `src/prompts.toml` and `src/prompts.rs`: make the hydration hint schema-neutral and describe tools as drill-down.
- Modify `skills/heimdall/SKILL.md`, `skill.toml`, and `reference/*.md`: consume the dossier first and remove default-report collection calls.
- Create `tests/heimdall_dossier.rs`: seeded-store end-to-end dossier contract, partial failures, lifecycle and size tests.
- Modify `tests/skill_package.rs` and `tests/skill_hydrate.rs`: package validation, backward compatibility, launch environment and harness parity.
- Modify `scripts/verify-trace-matrix.sh`: optional live assertion that the opening Heimdall answer does not call MCP or shell before responding.
- Create `docs/heimdall-dossier.md`: stable schema-v2 field, coverage, error, ordering and truncation reference.
- Modify `README.md`, `docs/skills.md`, and `AGENTS.md`: document schema v2, windows, compatibility and drill-down rules and link the detailed contract.

---

### Task 1: Extend the hydration package contract

**Files:**
- Modify: `src/skill/mod.rs:140-175` and the `[agent]` validation inside `parse_skill`
- Modify: `skills/heimdall/skill.toml`
- Test: `tests/skill_package.rs`

**Interfaces:**
- Consumes: existing `Hydration`, `SkillDefinition::hydrate`, and `parse_skill`.
- Produces: `Hydration::{Briefing, Dossier}`, `Hydration::ALL`, and validation that rejects both values in one package.

- [ ] **Step 1: Write failing package-contract tests**

Add tests that independently assert the new enum value, built-in setting, and invalid combination:

```rust
#[test]
fn dossier_hydration_is_valid_and_exclusive_with_briefing() {
    let dossier = parse_skill(
        "---\nname: audit\ndescription: Use when asked to \"audit\".\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nhydrate = [\"dossier\"]\n"),
        vec![],
        None,
    )
    .unwrap();
    assert_eq!(dossier.hydrate, vec![Hydration::Dossier]);

    let both = parse_skill(
        "---\nname: audit\ndescription: Use when asked to \"audit\".\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nhydrate = [\"briefing\", \"dossier\"]\n"),
        vec![],
        None,
    )
    .unwrap_err();
    assert!(both.message.contains("mutually exclusive"));
}
```

Change `builtin_heimdall_is_a_complete_package` to expect `vec![Hydration::Dossier]`.

- [ ] **Step 2: Run the package tests and verify RED**

Run: `cargo test --test skill_package dossier_hydration_is_valid_and_exclusive_with_briefing -- --exact`

Expected: compilation fails because `Hydration::Dossier` does not exist.

- [ ] **Step 3: Implement the minimal package contract**

Use this enum shape and parser:

```rust
pub enum Hydration {
    Briefing,
    Dossier,
}

impl Hydration {
    pub const ALL: [Hydration; 2] = [Hydration::Briefing, Hydration::Dossier];

    pub fn as_str(self) -> &'static str {
        match self {
            Hydration::Briefing => "briefing",
            Hydration::Dossier => "dossier",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "briefing" => Some(Hydration::Briefing),
            "dossier" => Some(Hydration::Dossier),
            _ => None,
        }
    }
}
```

After deduplication in `parse_skill`, return a `SkillError` when both variants are present. Change only the built-in `skills/heimdall/skill.toml` hydration line to `hydrate = ["dossier"]`; do not rewrite its instructions yet.

- [ ] **Step 4: Run package tests and verify GREEN**

Run: `cargo test --test skill_package`

Expected: all package tests pass, including schema-v1 custom-package coverage.

- [ ] **Step 5: Commit the contract**

```bash
git add src/skill/mod.rs skills/heimdall/skill.toml tests/skill_package.rs
git commit -m "feat: add dossier hydration contract"
```

---

### Task 2: Add scoped Rust subagent facts

**Files:**
- Create: `src/tracing/analysis/agents.rs`
- Modify: `src/tracing/analysis/mod.rs`
- Modify: `src/tracing/inventory.rs`
- Test: unit tests in `src/tracing/analysis/agents.rs`

**Interfaces:**
- Consumes: `inventory::Definition`, observations/traces/sessions/session_stats, and `AnalysisError`.
- Produces:

```rust
pub fn analyze_agents(
    conn: &rusqlite::Connection,
    workspace: &std::path::Path,
    since_ns: i64,
    until_ns: i64,
    definitions: &[crate::tracing::inventory::Definition],
    examples_per_category: usize,
) -> Result<AgentDossier, AnalysisError>;
```

and serializable `AgentDossier`, `AgentFact`, `AgentDefinitionFact`, `AgentCostShareFact`, `AgentFailureFact`, and `AgentInvocationFact`. `src/tracing/inventory.rs` produces the shared serializable `DefinitionFinding`.

- [ ] **Step 1: Write failing tests over a real in-memory store**

Seed two workspaces, five invocations of one agent, one definition-only agent, direct child tools, a grandchild tool, failures, tokens and session costs. Use fixed nanosecond timestamps. Assert:

```rust
let dossier = analyze_agents(
    &conn,
    Path::new("/work/a"),
    1_000,
    10_000,
    &[defined_agent("idle-reviewer"), defined_agent("reviewer")],
    3,
).unwrap();

let reviewer = dossier.agents.iter().find(|a| a.agent_type == "reviewer").unwrap();
assert_eq!(reviewer.invocations, 5);
assert_eq!(reviewer.failures, 2);
assert_eq!(reviewer.child_tools, 7); // direct children only
assert_eq!(reviewer.child_tool_errors, 2);
assert_eq!((reviewer.p50_ms, reviewer.p90_ms), (Some(30), Some(90)));
assert_eq!(reviewer.recurring_failures[0].tool_name, "Bash");
assert_eq!(reviewer.maximum_session_cost_share.as_ref().unwrap().session_key, "claude:a");
assert_eq!(dossier.agents.iter().find(|a| a.agent_type == "idle-reviewer").unwrap().invocations, 0);
assert!(dossier.agents.iter().all(|a| a.agent_type != "outside-workspace"));
```

Add a metadata-mode case where task/error snippets are `None` but counts remain.

- [ ] **Step 2: Run the agent tests and verify RED**

Run: `cargo test --lib tracing::analysis::agents::tests`

Expected: compilation fails because the module and API do not exist.

- [ ] **Step 3: Define the serializable fact types**

Use `Serialize`, `Deserialize`, `JsonSchema`, `Debug`, `Clone`, `PartialEq`, and `Default` where a typed empty section needs it. Keep absence distinct from zero:

```rust
pub struct AgentDossier {
    pub agents: Vec<AgentFact>,
    pub total_definitions: usize,
    pub total_observed: usize,
}

pub struct AgentCostShareFact {
    pub session_key: String,
    pub agent_cost_usd: f64,
    pub session_cost_usd: f64,
    pub share: f64,
}
```

`DefinitionFinding` contains `level`, `rule`, and `message` as owned strings so inventory findings can cross the JSON boundary.

Add the conversion beside `inventory::Finding`:

```rust
impl From<&Finding> for DefinitionFinding {
    fn from(value: &Finding) -> Self {
        Self {
            level: value.level.as_str().to_string(),
            rule: value.rule.to_string(),
            message: value.message.clone(),
        }
    }
}
```

- [ ] **Step 4: Implement one window- and workspace-scoped query path**

Select agent observations joined to traces and sessions with:

```sql
WHERE a.type = 'agent'
  AND a.start_ns >= ?1 AND a.start_ns < ?2
  AND s.cwd = ?3
```

Read invocation rows and direct children in bounded set-based queries, group in Rust, sort duration vectors, and use nearest-rank indices `ceil(0.50*n)-1` and `ceil(0.90*n)-1`. Normalize recurring failure messages with the existing public `crate::loops::breaker::error_signature` helper. Join definitions through `Definition::store_names()` and run `inventory::lint` with the provider's measured known-tool set.

- [ ] **Step 5: Run agent tests and the existing CLI query tests**

Run:

```bash
cargo test --lib tracing::analysis::agents::tests
cargo test --lib tracing::store::query::
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit agent facts**

```bash
git add src/tracing/analysis/agents.rs src/tracing/analysis/mod.rs src/tracing/inventory.rs
git commit -m "feat: compute scoped subagent dossier facts"
```

---

### Task 3: Add scoped Rust skill facts

**Files:**
- Create: `src/tracing/analysis/skills.rs`
- Modify: `src/tracing/analysis/mod.rs`
- Modify: `src/tracing/inventory.rs`
- Test: unit tests in `src/tracing/analysis/skills.rs`

**Interfaces:**
- Consumes: effective `inventory_all` definitions, 30-day traces/observations, known tools, `inventory::lint`.
- Produces:

```rust
pub fn analyze_skills_for_dossier(
    conn: &rusqlite::Connection,
    workspace: &std::path::Path,
    since_ns: i64,
    until_ns: i64,
    definitions: &[crate::tracing::inventory::Definition],
    examples_per_category: usize,
) -> Result<SkillDossier, AnalysisError>;
```

and serializable `SkillDossier`, `SkillFact`, `SkillEvidence`, and `SkillEvidenceKind`. Derive `Default` for `SkillDossier` so a failed section has a typed empty value.

- [ ] **Step 1: Write failing scoped skill tests**

Seed loaded/unused turns, attributed calls, a schema error, a slow call, a missed trigger, a stored-only skill, a definition-only skill, and identical data outside the workspace/window. Assert literal results:

```rust
let facts = analyze_skills_for_dossier(
    &conn,
    Path::new("/work/a"),
    1_000,
    10_000,
    &definitions,
    3,
).unwrap();

let audit = facts.skills.iter().find(|s| s.key == "audit").unwrap();
assert_eq!((audit.turns_loaded, audit.turns_unused), (6, 4));
assert_eq!(audit.missed_triggers, 2);
assert_eq!(audit.attributed_calls, 5);
assert_eq!((audit.errors, audit.schema_errors), (1, 1));
assert_eq!(audit.p95_ms, Some(5_000));
assert_eq!(audit.examples.len(), 3);
assert!(facts.skills.iter().any(|s| s.key == "not-on-disk"));
assert!(facts.skills.iter().any(|s| s.key == "never-used"));
assert!(facts.skills.iter().all(|s| s.key != "outside-workspace"));
```

Name the mutation each test catches in a comment: missing workspace predicate, all-time `skill_stats` reuse, attributed-only discovery, or capped totals.

- [ ] **Step 2: Run skill tests and verify RED**

Run: `cargo test --lib tracing::analysis::skills::tests`

Expected: compilation fails because the module and API do not exist.

- [ ] **Step 3: Add window-aware inventory inputs**

Add pure helpers rather than changing all-time UI behavior:

```rust
pub fn missed_trigger_count(def: &Definition, prompts: &[PromptRow]) -> i64;
```

Keep `skill_reports` intact for the Skills view. The dossier query selects prompt rows with `start_ns`, session cwd and exact bounds, then passes those rows to `missed_trigger_count`.

- [ ] **Step 4: Implement set-based 30-day metrics**

Do not use the all-time `skill_stats` view. Query bounded traces and observations, aggregate by the names returned from `Definition::store_names()`, then merge definition-only and store-only rows. Calculate duration percentiles from completed observations and retain `ongoing_count` separately. `definition_bytes` comes from file metadata or text length and is `None` for stored-only rows.

Return uncapped `total_definitions` and `total_observed`. Examples are the newest unused loads, missed triggers, and recurring errors, each with a stable trace/observation ID and a snippet only when content exists.

- [ ] **Step 5: Run skill and inventory tests**

Run:

```bash
cargo test --lib tracing::analysis::skills::tests
cargo test --lib tracing::inventory::tests
cargo test --lib tracing::analysis::metrics::tests
```

Expected: all selected tests pass and existing MCP skill analysis is unchanged.

- [ ] **Step 6: Commit skill facts**

```bash
git add src/tracing/analysis/skills.rs src/tracing/analysis/mod.rs src/tracing/inventory.rs
git commit -m "feat: compute scoped skill dossier facts"
```

---

### Task 4: Build the typed dossier from one connection

**Files:**
- Create: `src/tracing/analysis/dossier.rs`
- Modify: `src/tracing/analysis/mod.rs`
- Modify: `src/tracing/analysis/query.rs`
- Create: `tests/heimdall_dossier.rs`

**Interfaces:**
- Consumes: `analysis::briefing`, `analyze_skills_for_dossier`, `analyze_agents`, one inventory vector, one live-session vector, one `as_of`.
- Produces:

```rust
pub struct DossierInputs<'a> {
    pub conn: &'a rusqlite::Connection,
    pub db_path: &'a std::path::Path,
    pub workspace: &'a std::path::Path,
    pub home: &'a std::path::Path,
    pub live_sessions: &'a [LiveSession],
    pub live_snapshot_available: bool,
    pub as_of: time::OffsetDateTime,
}

pub fn build_dossier(
    inputs: DossierInputs<'_>,
    config: DossierConfig,
) -> Result<HeimdallDossier, DossierBuildError>;
```

- [ ] **Step 1: Write the failing end-to-end contract test**

Create a temporary store using the current schema, seed records on both sides of the 24-hour and 30-day boundaries, write project/home definitions, and pass one live session with `live_snapshot_available: true`. Assert:

```rust
let dossier = build_dossier(inputs_at("2026-09-17T12:00:00Z"), DossierConfig::default()).unwrap();
assert_eq!(dossier.schema_version, 2);
assert_eq!(dossier.windows.sessions.since, "2026-09-16T12:00:00Z");
assert_eq!(dossier.windows.evaluations.since, "2026-08-18T12:00:00Z");
assert_eq!(dossier.sessions.data.total_sessions, 2);
assert_eq!(dossier.skills.data.skills[0].key, "audit");
assert_eq!(dossier.agents.data.agents[0].agent_type, "reviewer");
assert_eq!(dossier.health.db_path, db.display().to_string());
assert_eq!(dossier.health.live_snapshot_available, true);
```

Assert that a record exactly at `since` is included and one exactly at `until` is excluded.

In `src/tracing/analysis/dossier.rs`, add a unit test around the private section-orchestration helper using injected section closures: make the agent closure return `AnalysisError`, let the session and skill closures succeed, and assert that the agent section is `Unavailable` with `SECTION_QUERY_FAILED` while the successful data remains. This test defines the failure-isolation seam without adding a public test-only option to `DossierConfig`.

- [ ] **Step 2: Run the dossier test and verify RED**

Run: `cargo test --test heimdall_dossier dossier_contains_every_startup_section -- --exact`

Expected: compilation fails because `analysis::dossier` does not exist.

- [ ] **Step 3: Define the schema-v2 types and defaults**

Implement `HeimdallDossier`, `SessionDossier`, `DossierScope`, `DossierWindows`, `DossierWindow`, `DossierHealth`, `DossierSection<T>`, `DossierError`, `DossierConfig`, and `DossierBuildError`. Derive `Default` for `SessionDossier`, `SkillDossier`, and `AgentDossier`; `DossierSection<T>` requires `T: Default` for a typed empty section:

```rust
impl<T: Default> DossierSection<T> {
    fn unavailable(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: CoverageStatus::Unavailable,
            warnings: Vec::new(),
            errors: vec![DossierError::new(code, message)],
            truncated: false,
            total_matching: 0,
            data: T::default(),
        }
    }
}
```

- [ ] **Step 4: Implement one-pass orchestration**

Capture bounds from `inputs.as_of`, call `inventory_all` once, filter the provided live sessions once, and call each section API with `inputs.conn`. Route the three section closures through one private isolation helper and convert a section error into `DossierSection::unavailable`. Only invalid scope or an unsupported store schema becomes a builder-level error; database-open failures occur before the builder and hydration serializes their top-level envelope.

Populate health with `PRAGMA user_version`, content mode, provider coverage, live availability and per-section elapsed milliseconds. Apply row caps after each section returns uncapped totals. Do not call `TraceService::execute`.

- [ ] **Step 5: Run dossier and existing briefing tests**

Run:

```bash
cargo test --test heimdall_dossier dossier_contains_every_startup_section -- --exact
cargo test --lib tracing::analysis::query::tests
cargo test --lib tracing::analysis::service::tests
```

Expected: all selected tests pass; existing briefing envelopes are byte-for-byte compatible where golden tests assert them.

- [ ] **Step 6: Commit the dossier core**

```bash
git add src/tracing/analysis/dossier.rs src/tracing/analysis/mod.rs src/tracing/analysis/query.rs tests/heimdall_dossier.rs
git commit -m "feat: build the Heimdall startup dossier"
```

---

### Task 5: Enforce deadline, partial coverage, ordering and 64 KiB budget

**Files:**
- Modify: `src/tracing/analysis/dossier.rs`
- Modify: `tests/heimdall_dossier.rs`

**Interfaces:**
- Consumes: `build_dossier`, `DossierConfig` and `rusqlite::Connection::progress_handler`.
- Produces: deterministic `finalize_dossier(dossier, config) -> Result<HeimdallDossier, DossierBuildError>` and the documented reduction order.

- [ ] **Step 1: Add failing boundary tests**

Add four real-behavior tests:

1. Two builds of the same fixture serialize to identical bytes.
2. A deliberately small byte budget removes examples before rows, preserves totals, and sets section truncation.
3. A minimal budget returns `DossierBuildError::TooLarge` rather than invalid JSON.
4. A SQLite query forced past the deadline is interrupted, yields `QUERY_TIMEOUT` for the affected section, and a subsequent `SELECT 1` on the same connection succeeds, proving handler cleanup.

Core assertions:

```rust
let json = serde_json::to_vec(&dossier).unwrap();
assert!(json.len() <= config.max_bytes);
assert!(dossier.skills.truncated);
assert_eq!(dossier.skills.total_matching, 80);
assert!(dossier.skills.data.skills.len() < 80);
assert!(dossier.sessions.status != CoverageStatus::Unavailable);
```

- [ ] **Step 2: Run boundary tests and verify RED**

Run: `cargo test --test heimdall_dossier dossier_ -- --nocapture`

Expected: size, truncation, or timeout assertions fail because finalization is not implemented.

- [ ] **Step 3: Implement deterministic sorting and reduction**

Sort with total ordering and stable IDs:

```rust
skills.sort_by(|a, b| {
    b.has_activity()
        .cmp(&a.has_activity())
        .then_with(|| b.turns_unused.cmp(&a.turns_unused))
        .then_with(|| b.errors.cmp(&a.errors))
        .then_with(|| b.cost_usd.unwrap_or(0.0).total_cmp(&a.cost_usd.unwrap_or(0.0)))
        .then_with(|| a.key.cmp(&b.key))
});
```

Implement the spec's reduction sequence: lowest-ranked examples, zero-activity definition rows, then lowest-ranked metric rows. Re-serialize after each batch removal. Never remove totals, sample sizes, coverage or errors.

- [ ] **Step 4: Install and remove the shared progress handler**

Use an absolute `Instant` deadline captured by `Arc`:

```rust
let deadline = Arc::new(std::time::Instant::now() + config.deadline);
let check = Arc::clone(&deadline);
inputs.conn.progress_handler(10_000, Some(move || std::time::Instant::now() >= *check))?;
let result = build_sections(&inputs, &config);
inputs.conn.progress_handler(0, None::<fn() -> bool>)?;
```

Wrap handler removal in a guard so early returns and panics during error conversion cannot leave it installed. Map `rusqlite::ErrorCode::OperationInterrupted` to `QUERY_TIMEOUT` for the current section.

- [ ] **Step 5: Run all dossier tests and JSON round trips**

Run: `cargo test --test heimdall_dossier`

Expected: all dossier tests pass and every successful serialized result is at most 65,536 bytes.

- [ ] **Step 6: Commit bounds and failure isolation**

```bash
git add src/tracing/analysis/dossier.rs tests/heimdall_dossier.rs
git commit -m "feat: bound and time-limit Heimdall dossiers"
```

---

### Task 6: Wire schema-v2 hydration through every harness launch

**Files:**
- Modify: `src/skill/launch.rs`
- Modify: `src/app.rs:prepare_agent_launch`
- Modify: `src/prompts.toml`
- Modify: `src/prompts.rs`
- Modify: `tests/skill_hydrate.rs`
- Modify: `tests/skill_package.rs`

**Interfaces:**
- Consumes: `Hydration::{Briefing, Dossier}`, `build_dossier`, `default_snapshot_dir`, App's resolved home/runtime/db/workspace.
- Produces:

```rust
pub struct Hydrated {
    pub path: PathBuf,
    pub as_of: String,
    pub schema_version: u32,
    pub ok: bool,
    pub error: Option<String>,
}

pub fn hydrate(
    def: &SkillDefinition,
    trace_db: Option<&Path>,
    cwd: &Path,
    home: &Path,
    runtime_dir: &Path,
    key: &str,
) -> Option<Hydrated>;
```

- [ ] **Step 1: Write failing schema-v2 launch tests**

Extend the existing fake-Claude fixture and add table-driven harness launch construction for Claude, Codex and Antigravity. For built-in Heimdall assert:

```rust
assert_eq!(doc["schema_version"], 2);
assert!(doc["sessions"]["data"]["sessions"].is_array());
assert!(doc["skills"]["data"]["skills"].is_array());
assert!(doc["agents"]["data"]["agents"].is_array());
assert_eq!(env["AGENT_MUX_BRIEFING_SCHEMA"], "2");
assert!(args.contains("use tools only for newer or narrower questions"));
```

Add a custom package with `hydrate = ["briefing"]` and assert schema 1 plus absence of the skills/agents sections. Keep missing-store and exit-cleanup assertions for both kinds.

- [ ] **Step 2: Run hydration tests and verify RED**

Run: `cargo test --test skill_hydrate`

Expected: dossier launch assertions fail because hydration still emits only schema 1 and no schema environment variable.

- [ ] **Step 3: Split hydration by requested kind**

Open the store with `store::open_ro` once in the dossier branch, capture `as_of`, call `analysis::read_snapshots(runtime_dir, now_ns)` once, flatten its sessions and retain whether a usable snapshot was available, then pass the connection, live sessions, availability flag and `home` into `build_dossier`. Keep the existing `TraceService::execute(Request::Briefing(...))` branch unchanged for schema 1.

Write a schema-appropriate top-level error envelope on database or build failure. Continue using `write_private` and the same path lifecycle.

- [ ] **Step 4: Export schema and use a schema-neutral prompt hint**

In `prepare_agent_launch`, pass `&self.skill_home()` to `hydrate` and add:

```rust
prep.env.push((
    "AGENT_MUX_BRIEFING_SCHEMA".into(),
    h.schema_version.to_string(),
));
```

Change the built-in hydration hint to instruct the model to inspect the snapshot's schema and answer from it first. Do not hardcode schema 2 for custom schema-v1 packages. Keep the existing config-library override key so user overrides do not disappear.

- [ ] **Step 5: Run launch, package, config-library and persistence tests**

Run:

```bash
cargo test --test skill_hydrate
cargo test --test skill_package
cargo test --test config_library
cargo test --test persistent_sessions
```

Expected: all selected tests pass in an isolated home/skills fixture.

- [ ] **Step 6: Commit launch integration**

```bash
git add src/skill/launch.rs src/app.rs src/prompts.toml src/prompts.rs tests/skill_hydrate.rs tests/skill_package.rs
git commit -m "feat: hydrate Heimdall with the Rust dossier"
```

---

### Task 7: Rewrite Heimdall around the dossier and document the contract

**Files:**
- Modify: `skills/heimdall/SKILL.md`
- Modify: `skills/heimdall/reference/sessions.md`
- Modify: `skills/heimdall/reference/skills.md`
- Modify: `skills/heimdall/reference/agents.md`
- Modify: `scripts/verify-trace-matrix.sh`
- Create: `docs/heimdall-dossier.md`
- Modify: `README.md`
- Modify: `docs/skills.md`
- Modify: `AGENTS.md`
- Test: `tests/skill_package.rs`

**Interfaces:**
- Consumes: schema-v2 dossier sections and existing MCP/CLI drill-down tools.
- Produces: a default playbook that reads the dossier once and does not collect startup facts through tools.

- [ ] **Step 1: Use the writing-skills workflow to pressure-test the revised instructions**

Before editing `SKILL.md`, invoke `superpowers:writing-skills`. Create three pressure cases in the skill-development notes used during the task:

1. Default startup prompt with complete dossier: no tool call is justified.
2. Default startup prompt with partial agent section: report the limitation; do not silently run broad SQL.
3. Follow-up asking for a session timeline newer than `as_of`: use the exact MCP timeline tool, CLI only when MCP is unavailable.

The revised instructions must make the expected decision unambiguous in all three cases.

- [ ] **Step 2: Rewrite the setup and playbooks**

The setup contract must say, in substance:

```markdown
Read `$AGENT_MUX_BRIEFING` once. For schema version 2, the `sessions`,
`skills`, `agents`, and `health` sections contain every fact required by
the startup report. Do not call MCP, the trace CLI, or SQL for that report.
Use tools only when the request is newer than `as_of`, outside the recorded
workspace/window, names an omitted/truncated item, or explicitly needs a
timeline, search, comparison, or different scope.
```

Remove instructions that make broad `analyze_skills`, `trace agents`, health or SQL calls mandatory during startup. Keep exact MCP/CLI mappings for permitted drill-down. Preserve read-only prohibitions.

- [ ] **Step 3: Align reference files with dossier fields**

For each threshold, name the dossier fields it uses. For example, wasted loads use `turns_unused / turns_loaded`; unreliable agents use `failures / invocations`; slow agents use `p90_ms`. Move executable SQL out of the normal procedure and keep only provenance or developer links to `docs/trace-sql-examples.md`.

- [ ] **Step 4: Add a live no-call verification path**

Extend `scripts/verify-trace-matrix.sh` behind `AGENT_MUX_LIVE=1` so it launches built-in Heimdall with a seeded trace store, captures the launch's first turn, and fails if an observation of type `tool` appears before the first assistant answer for the default startup prompt. Then send a second prompt requesting a fresh timeline and assert the exact timeline MCP tool or documented CLI fallback appears.

Keep CI deterministic: ordinary package tests validate parsing, embedded-file parity and the dossier contract; the real-model behavior check is opt-in because model/network behavior is not stable enough for normal CI.

- [ ] **Step 5: Update human documentation**

Document:

- schema-v2 sections and the 24-hour/30-day windows;
- 64 KiB cap and truncation metadata;
- `AGENT_MUX_BRIEFING_SCHEMA`;
- schema-v1 `briefing` compatibility;
- MCP/CLI as fresher/narrower drill-down only;
- one connection, one live snapshot read, one inventory scan;
- troubleshooting partial coverage and shadowed Heimdall packages.

Put the complete field/coverage/error/ordering/truncation contract in `docs/heimdall-dossier.md`; keep `README.md`, `docs/skills.md`, and `AGENTS.md` concise and link to it.

- [ ] **Step 6: Run package and documentation consumers**

Run:

```bash
cargo test --test skill_package
cargo test --test skill_hydrate
cargo test --lib skill::tests::builtin_heimdall_parses -- --exact
```

Expected: all tests pass and the compiled-in package matches repository files.

- [ ] **Step 7: Commit the skill and docs**

```bash
git add skills/heimdall scripts/verify-trace-matrix.sh README.md docs/skills.md docs/heimdall-dossier.md AGENTS.md tests/skill_package.rs
git commit -m "docs: make Heimdall dossier-first"
```

---

### Task 8: Prove performance and complete repository verification

**Files:**
- Modify: `tests/heimdall_dossier.rs`
- Modify: `docs/superpowers/specs/2026-09-17-heimdall-rust-dossier-design.md` (status only after verification)

**Interfaces:**
- Consumes: final dossier and hydration implementation.
- Produces: regression coverage for query shape, size and representative build timing; completed design status.

- [ ] **Step 1: Add the large deterministic fixture test**

Seed a small fixture and a large fixture containing at least 100 sessions, 100 skill names, 100 agent types, 1,000 turns and representative child observations. Install a temporary `rusqlite::Connection::authorizer` on each connection, increment an `Arc<AtomicUsize>` for every `AuthAction::Select`, build both dossiers from the same fixed `as_of`, then remove the authorizers. Assert that the large build prepares no more `SELECT` statements than the small build, guarding against per-row queries, and assert:

```rust
let bytes = serde_json::to_vec(&dossier).unwrap();
assert!(bytes.len() <= 65_536);
assert!(dossier.sessions.data.sessions.len() <= 20);
assert!(dossier.skills.data.skills.len() <= 30);
assert!(dossier.agents.data.agents.len() <= 30);
assert_eq!(dossier.skills.total_matching, 100);
assert_eq!(dossier.agents.total_matching, 100);
assert_eq!(large_selects.load(Ordering::SeqCst), small_selects.load(Ordering::SeqCst));
```

Use `rusqlite::hooks::{AuthAction, Authorization}` and return `Authorization::Allow` for every callback; this counts prepared `SELECT` operations without changing query results. Print build milliseconds only with `--nocapture`; do not assert a fragile wall-clock threshold in CI. Assert `health.total_build_ms` exists so real runs expose whether the less-than-500-ms target is being met.

- [ ] **Step 2: Run the focused performance fixture**

Run: `cargo test --test heimdall_dossier large_fixture_is_bounded_and_reports_build_time -- --exact --nocapture`

Expected: PASS, serialized size at most 65,536 bytes, an identical bounded `SELECT` count for small and large fixtures, and a printed local duration for review.

- [ ] **Step 3: Run formatting, lint, build and the full suite**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

Expected: all commands exit 0. If the dirty baseline's environment-sensitive installed-skill test still fails, rerun it with an isolated `AGENT_MUX_SKILLS_DIR`, record both outputs, and do not misattribute it to the dossier.

- [ ] **Step 4: Inspect the complete diff against the spec**

Run:

```bash
git log --oneline --grep="dossier"
git diff --check "$(git rev-list --max-count=1 --grep='feat: add dossier hydration contract' HEAD)^"..HEAD
git diff --stat "$(git rev-list --max-count=1 --grep='feat: add dossier hydration contract' HEAD)^"..HEAD
rg -n "trace agents|agent_mux_analyze_skills|trace sql" skills/heimdall
```

Expected: no whitespace errors; the remaining tool/CLI mentions are only conditional drill-down mappings, never default-startup steps. Check every acceptance criterion in the spec against code or test evidence.

- [ ] **Step 5: Mark the design implemented and commit verification evidence**

Change only the spec header to `Status: Implemented on 2026-09-17` and add a short deviations paragraph if implementation intentionally differs.

```bash
git add tests/heimdall_dossier.rs docs/superpowers/specs/2026-09-17-heimdall-rust-dossier-design.md
git commit -m "test: verify the Heimdall Rust dossier"
```
