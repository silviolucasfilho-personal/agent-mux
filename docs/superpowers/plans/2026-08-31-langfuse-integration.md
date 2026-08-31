# agent-mux Langfuse Session Tracing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When agent-mux starts a session for any of the three profiles (claude / codex / agy), the session is traced into Langfuse: one Langfuse session per CLI conversation, one trace per user turn, generations with token usage where available, tool spans, and a lifecycle trace per spawn. Opt-in via `[langfuse]` in profiles.toml, metadata-only content by default, fail-open everywhere.

**Architecture:** In-process uniform exporter. At spawn, `LangfuseRuntime::plan_launch` correlates the session with the CLI's own transcript (Claude: injected `--session-id <uuid>`; Codex: cwd-verified newest-rollout watch; Antigravity: brain-dir watch + `--conversation` resume fix). A per-session tokio task poll-tails the live transcript JSONL, parses lines through a shared `src/transcript.rs` (refactored out of `history.rs`, extended with usage/structured tool I/O + a net-new Codex parser), assembles turns in `src/langfuse/map.rs`, and hands OTLP/JSON spans to one blocking exporter OS thread (`ureq`) POSTing to `{host}/api/public/otel/v1/traces` (Basic auth, `x-langfuse-ingestion-version: 4`), with circuit breaker, 429/401 policy, and a breaker-aware bounded shutdown flush after `App::kill_all`.

**Tech Stack:** existing deps + `ureq` (rustls, blocking HTTP), `uuid` (v4 + v5 deterministic ids), `time` (RFC3339 parsing).

**Spec:** `docs/superpowers/specs/2026-08-30-langfuse-integration-design.md` (read it first; it is authoritative on every behavior below)

---

### Task 1: Dependencies + Config (`Cargo.toml`, `src/config.rs`, `profiles.example.toml`)

**Interfaces:**
- `LangfuseConfig` (global `[langfuse]` table: enabled, host, public_key, secret_key, content_mode, user_id, release, tags, environment, content_max_bytes, redact_literals, backfill_max_bytes, poll/flush/shutdown intervals, claude_dir/codex_dir/antigravity_dir) — Deserialize-only, `Option` scalars, `#[serde(default)]` collections.
- `ProfileLangfuse` (per-profile all-Option override: enabled, provider, content_mode, inject_session_id); `Profile.langfuse: Option<ProfileLangfuse>`; `Config.langfuse: Option<LangfuseConfig>`; `Config.loaded_from: Option<PathBuf>` (`#[serde(skip)]`).
- `ContentMode { Metadata, Full }`; `ResolvedLangfuse` (global resolution: env fallbacks `LANGFUSE_PUBLIC_KEY`/`SECRET_KEY`/`HOST` via injected env fn, host normalization, defaults) + `resolve_langfuse(...) -> Option<ResolvedLangfuse>`.
- Acceptance-gate fix in `load()`: a langfuse-only file is accepted with `default_profiles()` filled in (documented cwd-shadowing precedence).

- [x] **Step 1:** `cargo add ureq --no-default-features -F rustls; cargo add uuid -F v4,v5; cargo add time --no-default-features -F parsing`
- [x] **Step 2:** Failing tests: full/partial/absent section parse, per-profile table parse, gate fix, env fallback, enabled-without-keys → None, host normalization
- [x] **Step 3:** Implement structs + resolver + gate fix; update every `Profile` literal (`config.rs`, `app.rs` resume fallbacks + fixtures, `ui.rs`, tests) with `langfuse: None`
- [x] **Step 4:** Document `[langfuse]` + `[profiles.langfuse]` in `profiles.example.toml`; `cargo test` green

### Task 2: Shared transcript parser (`src/transcript.rs`) with bit-for-bit history parity

**Interfaces:**
- `Provider { Claude, Codex, Antigravity }`; `TranscriptEvent { User, Assistant{model, thinking, usage}, ToolUse{id, name, args: Value}, ToolResult{id, content: Value, is_error}, Thinking, TokenCount{usage}, TurnBoundary{kind}, SessionMeta{session_id, cwd, extra} }` — all with raw ISO `ts: Option<String>`.
- `parse_claude_line`, `parse_antigravity_line` (lifted from history.rs, extended: usage, thinking, structured args, array tool_result content), `parse_codex_line` (net-new, per rollout schema), `detect_provider`, `parse_rfc3339_nanos`, `extract_user_request` (moved here, re-exported from history).
- `history::load_claude_log` / `load_antigravity_log` become thin adapters producing today's `LogEntry` **bit-for-bit** (Claude thinking dropped, tool args flattened with the exact current rules, array tool_result content → `""`).

- [x] **Step 1:** Write transcript unit tests (Claude usage/thinking/array-result/`"<synthetic>"` model; Antigravity PLANNER_RESPONSE/tool_calls; malformed lines; detect_provider; rfc3339 parse)
- [x] **Step 2:** Implement `src/transcript.rs`; register in `lib.rs`
- [x] **Step 3:** Rewrite the two history.rs loaders as adapters; ALL existing history/viewer tests pass unchanged (the parity pin)

### Task 3: Codex rollout parser fixtures (`src/transcript.rs` tests)

- [x] **Step 1:** Source-derived fixtures: session_meta / message(user+assistant) / reasoning / function_call(+output, arguments-as-string) / local_shell_call / token_count(last_token_usage) / task_started / task_complete / turn_aborted; unknown types skipped
- [x] **Step 2:** `parse_codex_line` implementation green against fixtures

### Task 4: OTLP encoding + deterministic ids (`src/langfuse/otlp.rs`)

**Interfaces:** `AnyValue`/`KeyValue`/`Span` serde structs (hex ids, string nanos, lowerCamelCase, status ints), `build_request(spans) -> String` (resource: service.name/version), `trace_id_for(key)`, `span_id_for(key)` (UUIDv5 under fixed `AMX_NS`), `basic_auth(pk, sk)` + local base64.

- [x] **Step 1:** Failing tests: UUIDv5 stability vectors (pinned exact hex, never change), golden serialized request, base64 RFC 4648 vectors, hex encoding
- [x] **Step 2:** Implement; `cargo test` green

### Task 5: Turn assembler (`src/langfuse/map.rs`)

**Interfaces:** `MapSettings` (content mode, masking, truncation, tags/user/release/environment, trace metadata); `TurnAssembler::{prime, feed, finalize} -> Vec<Span>`; lifecycle span builders `session_started_span` / `session_ended_span`; secret masking (`sk-`, `pk-lf-`, `AKIA`, `ghp_`, `xox`, `Bearer `, `-----BEGIN` + redact_literals); ordinal = assembler turn boundaries; Codex generations buffered per turn so `token_count` usage attaches to the final generation (root span when none).

- [x] **Step 1:** Failing tests: turn shape (root+generation+tool spans, parents/times), error tool → ERROR, unpaired close, Codex boundaries incl. aborted-rerun ordinals, Antigravity positional pairing, lifecycle spans (+ watched-provider session-id caveat), metadata mode asserts absence of every content attribute, masking each pattern, truncation, ts fallback flags
- [x] **Step 2:** Implement; green

### Task 6: Tailer + correlation (`src/langfuse/tail.rs`, `src/langfuse/correlate.rs`)

**Interfaces:** `Tailer` (offset, remainder buffer, truncation reset, `read_new_lines`); prime pass only for Known-resume (backfill_max_bytes cap, tail-biased); `CorrelationSpec { KnownClaude, KnownAntigravity, WatchCodex, WatchAntigravity }` with poll fns; Claude uuid-glob widening after ~10 s; Codex date-dir scan + session_meta cwd verification; Antigravity three-tier confirmation; claim registry keyed by provider+id, released on pipeline exit.

- [x] **Step 1:** Failing tests under tempdirs (`tests/langfuse_tail.rs`, `tests/langfuse_correlate.rs`): appends across polls, partial line held, late file, Known-resume primes without emitting, watch-adopted first lines ARE emitted, backfill cap, truncation reset; codex cwd mismatch rejected, claude glob fallback, agy tiers + heuristic tag, claim double-adoption prevented + release/re-adopt
- [x] **Step 2:** Implement; green

### Task 7: Exporter thread (`src/langfuse/export.rs`)

**Interfaces:** `spawn_exporter(resolved, status_tx) -> (SyncSender<Span>, ExporterHandle)`; std `sync_channel(2048)` + `try_send` (drop-count); batching (flush_interval / 256 spans / ~1 MB); ureq POST with headers per spec; retry ladder (3 attempts, 1 s → 4 s ±jitter), 429 Retry-After (≤60 s, ≤2), 2×401/403 → disabled, 400/413 drop; circuit breaker (5 consecutive → open 60 s → half-open); breaker-open skips final drain flush; completion signal for shutdown join.

- [x] **Step 1:** Failing tests against a std `TcpListener` stub (`tests/langfuse_export.rs`): auth/headers/body asserted, 429 honored, 401×2 disables, breaker opens/half-opens, queue-full drops counted, shutdown deadline respected with hanging server, breaker-open skips final flush, doctor empty-batch probe
- [x] **Step 2:** Implement; green

### Task 8: Runtime + wiring (`src/langfuse/mod.rs`, `session.rs`, `app.rs`, `events.rs`, `main.rs`, `src/langfuse/doctor.rs`)

**Interfaces:**
- `LangfuseRuntime::{new, plan_launch, start_session, shutdown}`; `LaunchPlan { launch_id, extra_args, extra_env, correlation, settings }`; per-profile merge inside `plan_launch` (`enabled`/`provider`/`content_mode`/`inject_session_id`, `provider="none"` → untraced); injection skip list (`--resume`/`-r`/`--session-id`/`--continue`/`-c`/`-p`/`--print`), explicit-id extraction → Known; markers `AGENT_MUX=1`, `AGENT_MUX_SESSION_ID`.
- `SessionTraceHandle { phase: watch::Sender<Phase> }`, `mark_exited(exit_code)`; pipeline selects on phase watch + runtime shutdown watch; grace sweep on exit; single final pass + `termination:"app_quit"` on shutdown; injected-args fast-failure status hint.
- `Session::spawn(.., extra_args: &[String], extra_env: &[(String, String)])` + `pub trace` field; env/args applied between the arg loop and `cmd.cwd`.
- `App`: `langfuse` field, plan+attach at all three spawn sites, `trace.mark_exited` in `handle_pty_exit` + `forward_bytes` failure arm, Antigravity resume `--conversation <id>`, `take_langfuse()`.
- `AppEvent::LangfuseStatus(String)` → `app.error`; `main`: `langfuse doctor` arg dispatch, runtime construction, `rt.shutdown(deadline)` after `kill_all`.
- `doctor`: resolved-config print (masked keys), pk/sk prefix checks, cwd secret warning, empty-resourceSpans POST expecting 200 `{}`, per-provider readiness (PATH, `claude --help` contains `--session-id`, data dirs).

- [x] **Step 1:** plan_launch unit tests (classification, every skip condition, provider override/none, markers, disabled → None)
- [x] **Step 2:** Implement runtime + wiring; update ALL `Session::spawn`/`App::new` call sites (src + tests) with the new parameters
- [x] **Step 3:** `cargo test` green; `cargo run -- langfuse doctor` prints a sane report with no config

### Task 9: End-to-end test (`tests/langfuse_session.rs`)

- [x] **Step 1:** Unix-gated e2e: fake profile = shell script that reads the injected `--session-id` argv and appends Claude-shaped JSONL into `<tmp claude_dir>/projects/<slug>/<uuid>.jsonl`; stub TCP server; drive via a copied `pump_until`; assert turn spans + both lifecycle spans arrive; assert flush completes after `kill_all` + `shutdown` within the deadline
- [x] **Step 2:** Full `cargo test` green, zero warnings
