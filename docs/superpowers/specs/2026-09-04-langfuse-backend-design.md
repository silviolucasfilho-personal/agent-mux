# Langfuse as a selectable trace backend

**Status:** Implemented (plan: `../plans/2026-09-04-langfuse-backend.md`, all tasks landed). One addition over the design: credentials resolve from the environment alone, so a shell that exports `LANGFUSE_PUBLIC_KEY` and `LANGFUSE_SECRET_KEY` gets the Langfuse choice in the dialog without a config section; the default backend stays `local` and nothing is sent unless a launch picks Langfuse.
**Request (2026-09-04):** "I want both, but optionally, the user when launch a session can choose between langfuse or local session with sqlite."
**Builds on:** `2026-09-03-sqlite-trace-store-design.md` (rows, writer, pricing) and `2026-09-03-hook-channel-design.md` (hook pins). Replaces nothing: the local store stays the default.

## Purpose

Bring Langfuse back as a destination for traces without reviving the old, separate pipeline. Every session keeps being correlated, tailed, assembled, priced, and pinned by hooks exactly once; what changes is *where the resulting rows go*. A launch chooses `local` (SQLite, the default), `langfuse`, or `both`.

## Decisions

1. **One assembler, two sinks.** The pipeline (plan → correlate → tail → parse → assemble → `StoreOp` rows) is unchanged. A second consumer, the Langfuse exporter, receives the same `StoreOp` values as the SQLite writer and maps them onto Langfuse's ingestion API. Transcript parsing, usage normalization, pricing, hook pins, and subagent nesting are written once and reach both backends.
2. **OTLP, the transport every Langfuse accepts.** Each `StoreOp` becomes one OTLP span at `POST {host}/api/public/otel/v1/traces`, with no buffering or state in the sink. Langfuse converts a span server-side into exactly the ingestion events its own API takes (`OtelIngestionProcessor`: `trace-create` plus `<observation type>-create`) and merges writes sharing an id field by field (`worker/src/services/IngestionService`, `mergeRecords`), so our upsert stream (a turn emitted open then closed, hook pins re-emitting tool rows, agent rows re-parented) updates rather than duplicates.

   This replaces the design's first choice, `POST /api/public/ingestion`, which was implemented and then failed against a real server: a Langfuse v4 deployment running `LANGFUSE_MIGRATION_V4_WRITE_MODE=events_only` accepts only score and log events there and rejects every trace and observation with "Event type not accepted", naming OTLP as the supported path. OTLP works on that deployment, on v4 in `dual` and `legacy` modes, on self-hosted since 3.22, and on Langfuse Cloud.
3. **Routing is per launch.** `backend = local | langfuse | both`. Session, trace, and observation rows go to the chosen sink(s). Launch rows always go to the local store: they carry no content, and they keep the registry of what ran complete (`trace doctor` counts launches per backend). Hook rows are a raw side channel written by the hook command and always land locally.
4. **Where the choice is made.** In order of precedence: the launch dialog's Backend field, the profile's `[profiles.tracing] backend`, the global `[tracing] backend`, then `local`. The `t` attach toggle uses the profile/global value. The dialog only offers Langfuse when credentials resolve; otherwise it shows the reason and stays on local.
5. **Credentials resolve as before, and never lose data.** `[tracing.langfuse]` holds `host`, `public_key`, `secret_key`, `flush_interval_ms`; keys fall back to `LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY`, `LANGFUSE_HOST`. A legacy `[langfuse]` section, or the deprecated `host`/`public_key`/`secret_key` keys inside `[tracing]`, are adopted as the credentials with a startup notice. A launch that asks for Langfuse when nothing resolves runs as `local` and posts one notice; a Langfuse outage during a `both` launch still leaves the local copy.
6. **Cost travels with the row.** The exporter prices generations with the same price table as the store and sends both `usageDetails` and `costDetails`. Langfuse shows provided costs as-is, so both backends agree to the cent.
7. **Failure policy is the proven one.** Bounded `try_send` queue (drop and count when full), bounded retries with jittered backoff, `Retry-After` on 429 (capped, twice), two consecutive 401/403 disable the exporter for the run, five consecutive failed batches open a 60 s breaker with one half-open probe, and the final drain at quit is skipped while the breaker is open. The exporter thread from 9173c23 is reused as is.
8. **Live stats without a store.** Each sink reports `TraceStats` for the launches it touched: the SQLite writer keeps its commit hook, and the Langfuse exporter keeps per-launch aggregates in memory (closed turns, tokens, cost, running tool) so a Langfuse-only session gets the same status-bar badge.
9. **Later export is a replay.** `agent-mux trace export --langfuse` reads stored rows back as `StoreOp` values and pushes them through the same mapper and exporter, so a session traced locally can be sent to Langfuse afterwards, idempotently.

## Architecture

```
                    ┌─ SQLite writer thread ── traces.db ─┐
pipeline ── StoreOp ┤                                     ├─ TraceStats → status bar
   (per launch)     └─ Langfuse exporter thread ── HTTPS ─┘
      │
      └─ backend: local | langfuse | both   (Launch rows: always the writer)
```

- `TraceRuntime` owns both handles. The exporter thread is spawned at startup when credentials resolve and idles otherwise; nothing is spawned when they do not.
- `PipelineCtx` carries the launch's `Backend` and both senders; `send_ops` fans out (`both` clones the op). Drops on either queue count toward the launch's `dropped_ops`.
- Shutdown: signal pipelines, join them for half the deadline, then finish the writer and the exporter in parallel within what remains. The exporter's final drain is bounded by the same rule as before.

## Mapping: rows → ingestion events

Envelope: one `ExportTraceServiceRequest` per batch, `{"resourceSpans":[{"resource":…,"scopeSpans":[{"scope":{"name":"agent-mux"},"spans":[…]}]}]}`, with `Authorization: Basic base64(pk:sk)` and `Content-Type: application/json`. Batches close at 256 spans or 1 MiB. An accepted export may still report `partialSuccess.rejectedSpans`, which is surfaced once as a status line and counted as dropped.

| Row | Span | Fields |
|---|---|---|
| `SessionRow` | none | Langfuse sessions are implicit: the turn's root span carries `session.id`. |
| `TraceRow` | the trace's **root span** | `traceId` = trace id (32 hex), `spanId` = a deterministic id derived from it, no parent, `name`, start/end nanos; attributes `langfuse.trace.name`, `langfuse.trace.input`/`.output`, `session.id`, `user.id`, `langfuse.trace.tags` (JSON array string: configured tags + provider), `langfuse.trace.metadata` and `langfuse.observation.metadata` (JSON object string: provider, ordinal, status, launch_id, session_key, start/end nanos, latency_ms, thinking, skills, reported counters, timing_approx, plus the row's own metadata), `langfuse.observation.type` = `span`. An aborted turn sets span status ERROR and `langfuse.observation.level` = ERROR. |
| `ObservationRow` | a child span | `traceId`, `spanId` = observation id (16 hex), `parentSpanId` = the parent observation or the turn root; attributes `langfuse.observation.type` (`generation`/`tool`/`agent`/`event`/`span`), `.level`, `.status_message`, `.input`, `.output`, `.metadata`; generations add `.model.name`, `.usage_details`, `.cost_details`. |
| `LaunchRow` | none | local only. |

Details:

- Ids are the store's deterministic ids, so re-exporting a session (Decision 9) merges rather than duplicates.
- 64-bit nanos are decimal **strings** and ids are lowercase hex strings, per OTLP/JSON. Attributes are a KeyValue list of `AnyValue` wrappers: Langfuse rejects an entire export whose attributes are a plain object.
- A row still in flight (an open turn, a running tool) needs a valid span, so its end time equals its start until the closing row arrives and merges over it.
- `usage_details` uses Langfuse's flat integer record: `input`, `output`, `total`, plus `input_cache_read`, `input_cache_write`, `input_cache_write_1h`, and `output_reasoning` when present. `cost_details` carries `input`, `output`, `input_cache_read`, `input_cache_write`, `total` in USD.
- Content policy is upstream: rows are already masked, truncated, or emptied per the launch's content mode, so the mapper passes input and output through unchanged. Thinking rides in metadata (Langfuse has no field for it).
- `Level` maps one-to-one (`DEFAULT`, `WARNING`, `ERROR`); `is_error` also sets `level = ERROR` and the span's status to ERROR.

## Config

```toml
[tracing]
backend = "local"                 # "local" (default) | "langfuse" | "both"

[tracing.langfuse]
host = "https://cloud.langfuse.com"   # or $LANGFUSE_HOST; trailing "/" and "/api/public" are stripped
public_key = "pk-lf-…"                # or $LANGFUSE_PUBLIC_KEY
secret_key = "sk-lf-…"                # prefer $LANGFUSE_SECRET_KEY: this file is easy to commit
flush_interval_ms = 3000

[[profiles]]
name = "Claude Code"
command = "claude"
[profiles.tracing]
backend = "langfuse"              # this profile's default; the dialog can still change it
```

Resolved shapes: `ResolvedTracing.backend: Backend` and `ResolvedTracing.langfuse: Option<ResolvedLangfuse { host, public_key, secret_key, flush_interval_ms, secret_from_file }>`. `user_id`, `release`, `tags`, `environment`, and content settings remain the shared `[tracing]` values and apply to both backends. The existing cwd-secret warning stays: a secret key inside `./profiles.toml` is reported by doctor and at startup.

## UI

- **Launch dialog** gains a `Backend` field between Tracing and Content Mode: `[Local SQLite]`, `[Langfuse]`, `[Both]`; Space, Left, Right, or `b` cycle it. When Langfuse is not configured the field reads `[Local SQLite]  (Langfuse: not configured, see agent-mux trace doctor)` and does not cycle. Submitting stores the choice as the profile override for that launch, like tracing and content mode today.
- **Badge**: `[● 3t $0.12]` stays for local; Langfuse-only launches use `◆` and `both` uses `◈`. The verbose help line explains the glyphs.
- **Trace browser** is the local store and is unchanged; a Langfuse-only session is found in Langfuse under its CLI session id.

## CLI

- `trace doctor` gains a Langfuse section: whether credentials resolved and from where (file, env), the host, a live probe (`POST /api/public/otel/v1/traces` with an empty export), the default backend, and launches per backend in the last 7 days from launch metadata.
- `trace export --langfuse [--session KEY] [--since 30d] [--dry-run]` replays stored session, trace, and observation rows through the mapper and exporter and prints event and batch counts; `--dry-run` prints the counts and the first event without sending.

## Failure modes

| Situation | Behavior |
|---|---|
| Backend `langfuse`/`both` requested, no credentials | Launch runs as `local`; one notice names `[tracing.langfuse]`; launch metadata records `backend_requested`. |
| Langfuse unreachable mid-session | Exporter retries, then opens the breaker; rows drop and are counted; `both` keeps the local copy; one notice. |
| Wrong keys | Two consecutive 401/403 disable the exporter for the run; notice; local copy unaffected. |
| Quit while Langfuse is slow | Final drain bounded by `shutdown_flush_ms` shared with the writer; skipped entirely while the breaker is open. |
| Same session exported twice | Deterministic ids: Langfuse merges, nothing duplicates. |

## Testing

- **Config**: backend parsing and precedence; `[tracing.langfuse]`, legacy `[langfuse]`, and legacy in-`[tracing]` keys; env fallbacks; missing keys resolve to `None`.
- **Mapper** goldens: the turn's root span with trace attributes, generation with usage and cost, tool and agent nesting, parentless observations hanging under the turn root, level and status promotion on `is_error`, decimal-string nanos, the KeyValue attribute shape, batch closing on count and bytes.
- **Exporter** against an in-process fake HTTP server (as `tests/langfuse_export.rs` did at 9173c23): 200 success, `partialSuccess` surfaced once, 401×2 disables, 5xx retries then breaker, 429 honors `Retry-After`, queue-full drops, bounded final drain, probe outcomes.
- **Runtime**: plan resolves backend by precedence and downgrades with a notice; routing sends launch ops to the writer only, everything else per backend, `both` duplicates; exporter stats reach the app; shutdown finishes both sinks.
- **Dialog**: cycling, disabled when unavailable, submit sets the override.
- **End to end** (`tests/trace_langfuse.rs`): fake claude + fake Langfuse server; `both` lands rows in SQLite and events on the server with matching ids; `langfuse` leaves no trace rows locally and a launch row with `backend = "langfuse"`; `trace export --langfuse` replays a local session and the server sees one `trace-create` per turn.

## Out of scope

Scores, datasets, and prompts; importing from Langfuse into the store; changing backend mid-session; per-turn routing.

## Files

| File | Change |
|---|---|
| `Cargo.toml` | `ureq` 3 with `rustls` (as at 9173c23). |
| `src/config.rs` | `Backend`, `LangfuseConfig`, `ResolvedLangfuse`, precedence and legacy adoption. |
| `src/tracing/langfuse/mod.rs` (new) | Exporter thread, config, probe (from 9173c23 `export.rs`). |
| `src/tracing/langfuse/map.rs` (new) | Rows → OTLP spans, batch builder, usage/cost records. |
| `src/tracing/mod.rs` | `LaunchPlan.backend`, sinks in `PipelineCtx`, fan-out, exporter stats, shutdown. |
| `src/tracing/store/mod.rs` | `read_session_ops` for replay. |
| `src/tracing/cli.rs` | doctor section, `export --langfuse`. |
| `src/app.rs`, `src/ui.rs` | dialog field, badge glyphs, help. |
| `profiles.example.toml` | documented keys. |
| `tests/trace_langfuse.rs` (new) | exporter and end-to-end suites. |
