# agent-mux Local Trace Store (SQLite) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Langfuse OTLP export with a local SQLite trace store. Every agent-mux session (Claude / Codex / Antigravity) is recorded as typed rows — launches, sessions, per-turn traces, generation/tool observations with provider-correct usage and locally computed cost — in `~/.agent-mux/traces.db`, with no network, no keys, and zero configuration. A `trace` CLI and a TUI trace browser replace the Langfuse UI.

**Architecture:** Keep the pipeline (plan → correlate → tail → parse → assemble), swap the sink. `src/langfuse/` becomes `src/tracing/`; the turn assembler emits `StoreOp`s (typed rows) instead of OTLP spans; the exporter OS thread becomes a writer OS thread owning one `rusqlite` connection (WAL, one transaction per flush, breaker); usage normalization (`usage.rs`) and model matching + pricing (`pricing.rs` + bundled `pricing.toml`) move in-process; aggregates are SQL views. `ureq`/`rustls` are removed.

**Tech Stack:** existing deps − `ureq` + `rusqlite` (bundled SQLite with FTS5/JSON1); `uuid` (v4 + v5 ids), `time` (RFC3339), `toml` (bundled price table).

**Spec:** `docs/superpowers/specs/2026-09-03-sqlite-trace-store-design.md` (read it first; it is authoritative on every behavior below)

---

### Task 1: Dependencies + Config (`Cargo.toml`, `src/config.rs`, `profiles.example.toml`)

**Interfaces:**
- `TracingConfig` (global `[tracing]`, alias `[langfuse]`): `enabled: Option<bool>` (default true), `db_path`, `content_mode` ("full" default), `user_id`, `release`, `tags`, `environment`, `content_max_bytes` (65536), `redact_literals`, `backfill_max_bytes`, `poll_interval_ms`, `flush_interval_ms` (250), `shutdown_flush_ms` (1000), `retention_days` (0), `claude_dir`/`codex_dir`/`antigravity_dir`, `models: Vec<ModelPriceConfig>`, plus deprecated `host`/`public_key`/`secret_key` (parsed, ignored, flagged).
- `ModelPriceConfig { id, provider, match, input, output, cache_read, cache_write, cache_write_1h, reasoning }` (USD per 1M tokens).
- `ProfileTracing` (alias `langfuse`): `enabled`, `provider`, `content_mode`, `inject_session_id`. `Profile.tracing`, `Config.tracing`, `Config.legacy_langfuse_section` (`#[serde(skip)]`, set by `load()` from the file text).
- `ResolvedTracing` + `resolve_tracing(cfg, env) -> Option<ResolvedTracing>`: `None` only for `enabled = false`; `db_path` = config → `$AGENT_MUX_TRACE_DB` → `~/.agent-mux/traces.db`.

- [x] **Step 1:** `Cargo.toml`: add `rusqlite = { version = "0.40", features = ["bundled"] }` (keep `ureq` until Task 7 removes the exporter)
- [x] **Step 2:** Tests: absent section ⇒ enabled + defaults; `enabled = false` ⇒ None; `[langfuse]` alias loads; env `AGENT_MUX_TRACE_DB`; `[[tracing.models]]` parse; legacy keys flagged
- [x] **Step 3:** Implement; rename every `langfuse: None` literal to `tracing: None`; document `[tracing]` in `profiles.example.toml`

### Task 2: Ids + store (`src/tracing/ids.rs`, `src/tracing/store/{mod,schema,model,writer,query}.rs`)

**Interfaces:**
- `ids`: `AMX_NS`, `trace_id_for`, `span_id_for`, `hex`, `trace_id_hex`, `span_id_hex` (stability vectors unchanged).
- `model`: `LaunchRow`, `SessionRow`, `TraceRow`, `ObservationRow`, `StoreOp { Launch, Session, Trace, Observation }`.
- `schema`: `SCHEMA_VERSION`, `MIGRATIONS` (tables `meta`, `runs`, `sessions`, `launches`, `traces`, `observations`, `models`; FTS5 `observations_fts`/`traces_fts` + triggers; views `trace_stats`, `session_stats`).
- `store`: `open_rw(path, OpenOptions) -> Result<Store, String>` (dirs, 0600, pragmas, migrate/refuse-newer, seed models, `runs` row, recovery sweep, retention), `open_ro(path)`, `Store::apply(&[StoreOp])` (one transaction; per-op failures counted, not fatal; cost computed via `PriceTable`), `heartbeat`, `end_run`.
- `writer`: `spawn_writer(store, WriterConfig, StatusSink) -> WriterHandle { tx, dropped, finish(deadline) }`; batching (flush interval / 512 ops); BUSY retries; breaker.
- `query`: `list_sessions`, `list_traces`, `list_observations`, `find_session`, `find_trace`, `search`, `counts`, `unpriced_models`.

- [x] **Step 1:** Tests: id vectors; fresh DB schema version + pragmas; newer version refused; upsert idempotency + COALESCE semantics; open→closed transition; recovery sweep guarded by heartbeat; retention cascade; FTS sync; views
- [x] **Step 2:** Implement store + writer; `tests/trace_store.rs` (tempdir): batch commit, `finish` deadline with a held write lock, breaker on read-only file, queue-full counting

### Task 3: Usage + pricing (`src/tracing/usage.rs`, `src/tracing/pricing.rs`, `src/tracing/pricing.toml`)

**Interfaces:** `NormalizedUsage`, `normalize(provider, raw)`; `ModelPrice`, `PriceTable::{builtin, with_overrides, find}`, `normalize_model_name`, `cost_for(price, usage) -> Cost`.

- [x] **Step 1:** Tests: Claude cache buckets (uncached input preserved; 5m/1h split); Codex inclusive input/cached and output/reasoning; Antigravity empty; name normalization vectors; exact beats prefix; `<synthetic>` never matches; cost vectors; config override
- [x] **Step 2:** Implement + bundled table (values per spec; dated)

### Task 4: Assembler → rows (`src/tracing/map.rs`)

**Interfaces:** `MapSettings` gains `run_id`, `correlation_plan`, `injected`, `attached`; `TurnAssembler::{feed, finalize} -> Vec<StoreOp>`; open trace row at turn open, tool row at ToolUse (`end_ns: None`), `usage_only` generation for unattached usage, session title op on turn 1 (full mode); `launch_started` / `launch_ended` replace the lifecycle spans. All classification, dedup, masking, ordinal and id rules unchanged.

- [x] **Step 1:** Convert every existing assembler test to row assertions (one for one), add: open-then-closed trace, in-flight tool row, usage-only generation, metadata mode ⇒ content columns NULL
- [x] **Step 2:** Implement; green

### Task 5: Runtime wiring (`src/tracing/mod.rs`, `correlate.rs`, `tail.rs`)

**Interfaces:** `TraceRuntime::{new -> Result<_, String>, plan_launch, plan_attach, start_session, shutdown, run_id, db_path}`; pipeline sends `StoreOp`s; adoption sends `Session` + `Launch` updates; finalize sends `Launch(ended)`; shutdown default 1 s.

- [x] **Step 1:** `git mv src/langfuse src/tracing`; `correlate.rs`/`tail.rs` unchanged; plan tests use a tempdir DB
- [x] **Step 2:** Implement; `tests/trace_tail.rs` / `tests/trace_correlate.rs` renamed and green

### Task 6: CLI (`src/tracing/cli.rs`, `src/main.rs`)

- [x] **Step 1:** `trace doctor | path | ls | show | search | import | export | prune | recost | sql`; `langfuse doctor` prints the deprecation pointer
- [x] **Step 2:** Tests against a fixture DB: `ls`/`show`/`search`/`sql`; `import` idempotent; `prune --dry-run`

### Task 7: App / UI / events wiring; remove the exporter

- [x] **Step 1:** `AppEvent::TraceStatus`; `App.tracing` / `take_tracing`; strings ("Tracing:", "toggle tracing"); `main.rs` runtime construction with notices (store unavailable, legacy `[langfuse]`, `/mnt/*`)
- [x] **Step 2:** Delete `otlp.rs`, `export.rs`, `doctor.rs`, `tests/langfuse_export.rs`; remove `ureq`; `tests/trace_session.rs` e2e asserts DB rows after `kill_all` + `shutdown`
- [x] **Step 3:** Full `cargo test` green, zero warnings

### Task 8: Phase 2 — live badges + trace browser

**Interfaces:** `AppEvent::TraceStats { session, turns, total_tokens, cost_usd, running_tool }` published by the writer (throttled 1/s per launch); `Mode::TraceBrowser(TraceBrowserState)` on `T`: sessions / turns / detail panes over `query.rs`, refreshed on `Tick` while a live session is selected; `r` resumes.

- [x] **Step 1:** Stats event + badges in sidebar and main-pane title
- [x] **Step 2:** Browser state, key handling, `ui::draw_trace_browser`, help entries; render test on a fixture DB

### Task 9: Antigravity usage source (`src/tracing/agy_usage.rs`)

**Interfaces:** `GenUsage` + `decode_gen_metadata(idx, blob)` (raw protobuf walker over agy's `conversations/<id>.db` → `gen_metadata`), `conversation_db_for(transcript_path, id)`, `AgyUsageReader::{new, skip_existing, poll, read_all}`; `TranscriptEvent::Assistant.step_index`; `TurnAssembler::attach_step_usage` (upsert of the emitted generation by step, held when the record precedes the transcript line); pipeline polls per tick after adoption; `trace import` reads the whole db; `usage::normalize` maps `prompt/output/thoughts/context_tokens` (cache read = context − prompt).

- [x] **Step 1:** Fixture from real agy 1.1.25 records (no content); decoder, path, reader, normalization, and either-order attachment tests
- [x] **Step 2:** Implement; live pipeline + import wiring; green
