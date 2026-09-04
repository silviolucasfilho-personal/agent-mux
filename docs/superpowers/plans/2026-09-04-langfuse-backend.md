# Langfuse backend: implementation plan

Spec: `../specs/2026-09-04-langfuse-backend-design.md`. Each task ends green (`cargo clippy --all-targets`, `cargo test --no-fail-fast`) and is independently reviewable.

### Task 1: Config (`src/config.rs`, `Cargo.toml`, `profiles.example.toml`)

**Interfaces:** `Backend { Local, Langfuse, Both }` with `parse`/`as_str`; `LangfuseConfig { host, public_key, secret_key, flush_interval_ms }` under `[tracing.langfuse]`; `ResolvedLangfuse { host, public_key, secret_key, flush_interval_ms, secret_from_file }`; `ResolvedTracing.backend`, `ResolvedTracing.langfuse: Option<_>`; `ProfileTracing.backend: Option<String>`; legacy `[langfuse]` section and legacy keys inside `[tracing]` adopted as credentials.

- [x] **Step 1:** Tests: backend parsing (unknown → Local), `[tracing.langfuse]` resolution with env fallbacks and host normalization, legacy adoption from both places, `secret_from_file`, missing keys → `None`.
- [x] **Step 2:** Implement; add `ureq`; document keys in `profiles.example.toml`. Credentials also resolve from the environment alone (`$LANGFUSE_PUBLIC_KEY` + `$LANGFUSE_SECRET_KEY`), which makes the backend selectable without any config section; nothing is sent unless a launch picks it.

### Task 2: Exporter (`src/tracing/langfuse/mod.rs`)

**Interfaces:** `ExporterConfig::new(&ResolvedLangfuse)` (endpoint `{host}/api/public/ingestion`, basic auth, batch/retry/breaker knobs); `spawn_exporter(cfg, status, stats) -> ExporterHandle { tx: SyncSender<StoreOp>, dropped, finish(deadline) }`; `probe(&ResolvedLangfuse) -> Result<(), String>`; per-launch aggregates reported through `stats: Box<dyn FnMut(&str, LaunchStats) + Send>` after every flushed batch.

- [x] **Step 1:** Tests against an in-process fake HTTP server: 207 success and `errors` surfaced once; 401×2 disables; 5xx retries then breaker; 429 honors `Retry-After`; queue-full drops counted; bounded final drain; probe outcomes.
- [x] **Step 2:** Implement, reusing the 9173c23 `export.rs` thread and policy. `replay_sessions` (blocking sends, bounded finish) backs `trace export --langfuse`.
- [x] **Step 3 (follow-up):** Transport corrected from `POST /api/public/ingestion` to OTLP (`POST /api/public/otel/v1/traces`) after the first live run: a Langfuse v4.24 server running `LANGFUSE_MIGRATION_V4_WRITE_MODE=events_only` rejected every trace and observation event with "Event type not accepted". `partialSuccess.rejectedSpans` replaces the 207 `errors` array. Verified live: a one-session export landed nine events in `events_core`/`events_full` with the right trace name, session id, user, tags, nesting, usage and cost.

### Task 3: Mapper (`src/tracing/langfuse/map.rs`)

**Interfaces:** `event_for(op: &StoreOp, ctx: &MapCtx) -> Option<Event>` where `MapCtx { prices, version, tags }`; `Event { id, timestamp, type_, body }` serializable; `Batch` builder closing at 256 events / 1 MiB; usage and cost records.

- [x] **Step 1:** Golden tests per row kind (trace, generation with usage + cost, tool, agent with parent, event without `endTime`, level promotion), timestamp formatting, batch limits.
- [x] **Step 2:** Implement.

### Task 4: Runtime wiring (`src/tracing/mod.rs`, `src/tracing/store/mod.rs`)

**Interfaces:** `LaunchPlan.backend: Backend` (+ `backend_requested` when downgraded); `PipelineCtx { backend, op_tx (writer), export_tx: Option<_> }`; `send_ops` fan-out with launch ops always local; launch metadata `backend`; `TraceRuntime::langfuse_configured()`; shutdown finishes writer and exporter within the deadline; `store::read_session_ops(conn, session_key) -> Vec<StoreOp>` for replay.

- [x] **Step 1:** Tests: precedence dialog > profile > global; downgrade with notice; routing per backend (writer receives launch only for `langfuse`, everything for `local`, everything for `both` with the exporter receiving all but launch); stats event from the exporter reaches the app channel; `read_session_ops` round-trips a stored session.
- [x] **Step 2:** Implement.

### Task 5: Dialog, badge, help (`src/app.rs`, `src/ui.rs`)

- [x] **Step 1:** Tests: `DialogField::Backend` cycles local → langfuse → both when available, stays local with the hint otherwise; submit sets `profile.tracing.backend`; badge glyph per backend.
- [x] **Step 2:** Implement (`DialogState::new(profiles, langfuse_available)`, `SessionTrace.backend`, help rows).

### Task 6: Doctor, export, end to end (`src/tracing/cli.rs`, `tests/trace_langfuse.rs`)

- [x] **Step 1:** Tests: `trace export --langfuse --dry-run` counts; replay of a stored session against the fake server yields one `trace-create` per turn with matching ids; e2e `both` and `langfuse` launches via a fake claude.
- [x] **Step 2:** Implement doctor section, `export --langfuse`, spec status update. The doctor probe POSTs an empty batch to the ingestion endpoint (Langfuse answers 207); the export without `--session` replays every session seen since `--since`.
