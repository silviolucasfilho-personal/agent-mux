# agent-mux Langfuse Session Tracing

**Date:** 2026-08-30
**Status:** Approved design, implemented (plan: `../plans/2026-08-31-langfuse-integration.md`)
**Builds on:** v1 (`2026-08-29-agent-mux-design.md`), terminal UX (`2026-08-30-terminal-ux-design.md`), session logs (`2026-08-30-session-logs-design.md`)

## Purpose

When agent-mux starts an agent session — Claude Code (`claude`), Codex (`codex`), or Antigravity (`agy`) — that session becomes observable in [Langfuse](https://langfuse.com): one Langfuse session per CLI conversation, one trace per user turn, with generations (model + token usage where available), tool spans, timings, and error levels. The integration is opt-in, mutates no CLI's own configuration, and can never break, block, or slow a session.

## Decision summary

**agent-mux owns the whole pipeline in-process.** At spawn it deterministically correlates the session with the CLI's own on-disk transcript; a per-session tokio task poll-tails the live-written transcript JSONL; lines are parsed through a shared `src/transcript.rs` layer (refactored out of `history.rs`, extended with token usage and structured tool I/O, plus a net-new Codex rollout parser); a turn assembler emits hand-built OTLP/JSON spans; and one dedicated blocking exporter OS thread POSTs them to Langfuse's OTLP endpoint.

This was chosen over two alternatives after a design review:

1. **CLI-native OTel** (point each CLI's exporter at Langfuse) — rejected. Langfuse's OTel endpoint accepts the **traces signal only** (no logs, no metrics). Claude Code's content-bearing signal is its logs/events stream, which Langfuse cannot ingest; its traces signal is beta with unverified span semantics. Codex's `[otel]` export can be enabled per-launch via `-c` config overrides without touching `~/.codex/config.toml`, but its data is event/metric-shaped, not turn-structured, and it passes collector credentials into the child process. Antigravity has no OTel export at all (verified on agy 1.1.22; open upstream issue #366). Native OTel covers at most two of three CLIs, poorly, via three divergent mechanisms — and puts Langfuse credentials into child environments where the agent's own shell tool can read them.
2. **Hook scripts** (Claude `--settings` hooks, Codex `hooks.json`, Antigravity global hooks) — rejected as the primary mechanism. Hooks carry at most content fragments (last assistant message, the prompt, tool args) — full turn structure (complete assistant text, thinking, usage) still requires the transcript. They also require shipping a helper program, a Windows story, and writing into the user's global CLI config (Antigravity's workspace-local hooks verifiably do not load on 1.1.22) or a repo-local `.codex/hooks.json` that needs an interactive trust step.
3. **Uniform in-process transcript pipeline** — chosen. A transcript pipeline is mandatory anyway for Antigravity, and it is the shape Langfuse itself endorses: the official `langfuse/codex-observability-plugin` works exactly this way (reads the rollout JSONL, rebuilds turns). Once the pipeline exists, running it for all three CLIs is one code path, one config surface, one failure model — independent of CLI versions, feature flags, plugin installs, or missing telemetry. agent-mux already parses two of the three transcript formats in `src/history.rs`.

## Requirements

1. **Opt-in**: tracing runs only when a `[langfuse]` config section is enabled and keys resolve (config or `LANGFUSE_*` env vars). Absent/invalid config never prevents the TUI from starting.
2. **Coverage**: every session spawned from agent-mux (new-session dialog, history resume, respawn) for a recognized provider is visible in Langfuse — at minimum as a lifecycle trace with spawn/exit events, and, once correlated with its transcript, as full per-turn traces.
3. **Session identity**: the Langfuse `sessionId` is the CLI's own conversation/session id (Claude session UUID, Codex `session_id`, Antigravity conversation UUID), so Langfuse lines up with each CLI's native resume story and with agent-mux's history viewer.
4. **Privacy by default**: `content_mode = "metadata"` (the default) sends **no prompt, response, or tool bodies** — only names, timings, models, token usage, and error levels. `content_mode = "full"` is an explicit (per-profile-overridable) opt-in and passes all content through secret-pattern masking and size truncation.
5. **Fail-open invariant**: no failure in the tracing path (network, auth, rate limit, schema drift, missing transcript, missing CLI data dir) may break, block, or slow a session or the app. Quit latency is hard-capped.
6. **Zero CLI-config mutation**: agent-mux never writes into `~/.claude`, `~/.codex`, or `~/.gemini` — transcripts are read read-only; the only spawn-time changes are additive CLI arguments and inert marker env vars.
7. **Current API**: use Langfuse's OTLP endpoint. The legacy `/api/public/ingestion` API is deprecated and removed from Langfuse Cloud on 2026-11-16; it must not be targeted.

## Architecture

### What runs where

```
main() ── config::load() ── resolve [langfuse] ──► Option<LangfuseRuntime>
                                                     │
App (main loop, existing)                            │ owns
 │  spawn site (x3: dialog / resume / respawn)       ▼
 │   ├─ plan = rt.plan_launch(&profile, &dir)   LangfuseRuntime
 │   ├─ Session::spawn(.., &plan.extra_args,     ├─ claim registry (adopted transcript ids)
 │   │                     &plan.extra_env)      ├─ span_tx: bounded std sync_channel (2048, try_send)
 │   └─ session.trace = rt.start_session(plan)   ├─ shutdown watch (Receiver cloned into pipelines)
 │                                               └─ exporter OS thread (blocking ureq + breaker)
 │                                                            ▲
 │  per session: one tokio task                               │ completed OTLP spans
 │   correlate ─► tail (500ms poll) ─► parse ─► assemble turns┘
 │                        (src/transcript.rs, shared with history viewer)
 │
 └─ exit: app.kill_all()  →  rt.shutdown(deadline ≤ 3s, flush skipped if breaker open)
```

- **Per-session pipeline = one `tokio::spawn` task** (the runtime is ambient at all three spawn call sites and in `#[tokio::test]`s). It reads the transcript **file** only — never the PTY byte stream — so it is structurally outside the input, output, render, and exit paths.
- **Exporter = one dedicated `std::thread`** doing blocking HTTP (`ureq`), consuming spans via `blocking_recv()`. One thread for the whole app: a single place for batching, 429 backoff, the circuit breaker, and — critically — a **joinable flush point that survives tokio runtime teardown** (spawned tokio tasks are aborted when the runtime drops at the end of `main`; an OS thread is not). This matches the repo's existing blocking-threads-bridged-to-tokio idiom (PTY reader and exit-watcher threads).

### Per-session lifecycle

1. **Plan (pre-spawn, App layer).** `rt.plan_launch(profile, dir)` classifies the profile into a `CliKind` (Claude / Codex / Antigravity; command-basename match with a per-profile `provider` override for wrapper scripts), merges the profile's `[profiles.langfuse]` overrides over the runtime's resolved global settings, inspects the args, and returns `Option<LaunchPlan>` where `LaunchPlan { launch_id: Uuid, extra_args, extra_env, correlation, settings }`. `None` (profile disabled via `enabled = false`, or `provider = "none"`, or unrecognized command with no override) means the session is fully untraced: no extras, no markers, no pipeline, no lifecycle trace.
   - Claude, new session: `extra_args = ["--session-id", <uuid-v4>]` → `correlation = Known { session_id, expected_path }`. Injection is **skipped** when the args already contain `--resume`/`-r`, `--session-id`, `--continue`/`-c`, `-p`/`--print`, or when the profile sets `inject_session_id = false`. When a skipped launch carries an explicit id (`--resume <id>` or `--session-id <id>` in the args), the id is parsed out and correlated as `Known`; skipped launches with no extractable id (bare `-r`, `--continue`, `-p`) correlate as `none` and get lifecycle traces only.
   - Codex, and Antigravity new sessions: `extra_args = []`, `correlation = Watch { kind, t0, cwd }`.
   - Antigravity resume: `resume_history_session` builds `--conversation <id>` into the resume profile's args (exactly as the Claude arm builds `--resume` today, app.rs:1152–1153); `plan_launch` detects the flag and correlates as `Known { conversation_id, brain_path }` — no watch.
   - All traced providers: `extra_env = [("AGENT_MUX", "1"), ("AGENT_MUX_SESSION_ID", launch_id)]` — inert markers, a documented escape hatch for users' own tooling. **No credentials ever enter a child environment.**
2. **Spawn.** `Session::spawn` gains `extra_args: &[String]` and `extra_env: &[(String, String)]` parameters, applied at the documented insertion point between the arg loop (session.rs:173–175) and `cmd.cwd` (session.rs:176). Extras are **never baked into `profile.args`** — `respawn_selected` clones the old profile, so persisting a `--session-id` would make a respawned tab collide with the dead session's UUID; keeping extras separate means respawn re-plans a fresh one. A `claude` binary predating `--session-id` spawns fine and then exits immediately with a usage error — spawn failure cannot detect it. Instead: when a session launched with injected `extra_args` exits within 5 s with a nonzero code and its transcript never appeared, the runtime posts a one-time status-bar hint naming `inject_session_id = false` and the minimum `claude` version; `langfuse doctor` checks the version proactively.
3. **Attach (post-spawn).** `rt.start_session(...)` emits the lifecycle `session_started` event span (see Data mapping) and spawns the pipeline task, handing it a clone of the runtime's shutdown-watch `Receiver`; it returns a `SessionTraceHandle { phase: tokio::sync::watch::Sender<Phase> }` stored as a new `Session.trace: Option<SessionTraceHandle>` field. The phase `Sender` is owned solely by the handle, so dropping the `Session` closes the channel; the runtime signals app shutdown through its own separate watch. Each pipeline `select!`s on both.
4. **Correlate** (pipeline phase 1). Loops on a 500 ms interval until the transcript is found and claimed in the runtime's claim registry, or the phase becomes `Exited`. Claims are keyed by transcript id and **released when the claiming pipeline exits** (after its grace sweep), so exiting a session and then resuming or respawning the same conversation within one app run re-adopts cleanly. Per-CLI logic below.
5. **Tail + export** (pipeline phase 2). Each tick: `stat` the file; if grown, read new bytes, split complete newline-terminated lines (partial trailing line held in a remainder buffer — all three CLIs append-and-flush whole lines), parse via `src/transcript.rs`, feed the turn assembler, `try_send` completed spans to the exporter. The **prime pass** — parse the existing content to rebuild assembler state (turn ordinals, open tool calls) *without emitting*, bounded by `backfill_max_bytes` (tail-biased, flagged in metadata when truncated), then tail from EOF — runs **only for `Known` resume correlations** (Claude `--resume`/explicit id, Antigravity `--conversation`), so resumed sessions never re-export history. Watch-adopted files and new-session files found mid-write are parsed **from byte 0 and emitted** — the head of a fresh Codex/Antigravity/Claude session (which by construction already contains its first lines at adoption) is exported, not swallowed.
6. **End.** `App::handle_pty_exit` calls `session.trace.mark_exited()` — a `watch::send` of `Phase::Exited`, naturally idempotent against the documented duplicate `PtyExit`s and safe against PtyExit/PtyOutput reordering (the pipeline reads the file, not the event stream). The `forward_bytes` write-failure path (app.rs:1015–1026), which marks a session Exited without any PtyExit, gets the same one-line call. On `Exited` the pipeline does a grace sweep (≈3 more polls over ~1 s for the CLI's final flushes), closes the open turn, emits the lifecycle `session_ended` event span (exit code, correlation outcome), releases its claim, and exits. Dropping the `Session` (RemoveSelected, respawn replacement) closes the phase channel; the pipeline treats that like `Exited`.
7. **App shutdown / flush.** `App::kill_all` (app.rs:1255) runs on every main-loop exit path (main.rs:124) and stays untouched. The main loop has already exited by then, so `PtyExit` events from the kills are never processed — the runtime's shutdown watch is the pipelines' only exit signal. Immediately after `kill_all`:
   ```rust
   app.kill_all();
   if let Some(rt) = app.take_langfuse() {
       rt.shutdown(Duration::from_millis(shutdown_flush_ms /* default 3000 */)).await;
   }
   ```
   `shutdown` flips the runtime-owned shutdown watch; each pipeline, on seeing it, skips the multi-poll grace sweep and instead does **one** final tail pass, closes the open turn with `end` = the last-seen event timestamp, emits its `session_ended` event span with `termination: "app_quit"` (exit code unknown), and exits. `shutdown` awaits the pipeline JoinSet with ~half the deadline, drops the last `span_tx` so the exporter drains, and waits on the exporter's completion oneshot for the remainder. **If the circuit breaker is open, the final flush is skipped entirely** — a dead network costs milliseconds at quit, not the deadline. On timeout, remaining spans (including those `session_ended` events) are abandoned; quitting is never blocked by telemetry. Panic paths do not flush.

### UI feedback

One new `AppEvent::LangfuseStatus(String)` variant routed to the existing `app.error` status-bar field. Emitted at most once per class per run; this list is the canonical class enumeration: (1) enabled-without-resolvable-keys at startup; (2) secret key found in the cwd-relative `./profiles.toml` (commit hazard); (3) injected-`--session-id` fast-failure hint (step 2); (4) permanent auth disable; (5) breaker-open notice; (6) dropped-spans warning.

## Config

### TOML

Appended to `profiles.example.toml`:

```toml
# Optional Langfuse tracing. Off unless enabled = true and keys resolve.
[langfuse]
enabled = true
host = "https://cloud.langfuse.com"    # or https://us.cloud.langfuse.com / self-hosted (needs Langfuse >= v3.22.0)
public_key = "pk-lf-..."               # optional; falls back to $LANGFUSE_PUBLIC_KEY
secret_key = "sk-lf-..."               # optional; falls back to $LANGFUSE_SECRET_KEY (env strongly recommended —
                                       # ./profiles.toml is cwd-relative and easy to commit)
content_mode = "metadata"              # "metadata" (default): no prompt/response/tool bodies leave the machine.
                                       # "full": content included, secret-pattern-masked and truncated.
# user_id = "silvio"                   # default: $USER / $USERNAME
# release = "rollout-3"                # -> langfuse.release
# tags = ["agent-mux"]                 # extra tags on every trace
# environment = "development"
# content_max_bytes = 16384            # per-field truncation cap (full mode)
# redact_literals = ["hunter2"]        # extra literal substrings masked in full mode
# backfill_max_bytes = 4194304         # prime-pass cap for huge resumed transcripts
# poll_interval_ms = 500
# flush_interval_ms = 3000
# shutdown_flush_ms = 3000
# claude_dir = "/custom/.claude"       # dir overrides (also used by tests)
# codex_dir = "/custom/.codex"
# antigravity_dir = "/custom/.gemini/antigravity-cli"

[[profiles]]
name = "Claude Code"
command = "claude"
args = []

[profiles.langfuse]                    # per-profile override, all-Option
enabled = true
provider = "claude"                    # force CLI-kind detection for wrapper commands
# content_mode = "full"                # dial one profile up
# inject_session_id = false            # escape hatch: never add --session-id for this profile
```

### Rust shapes (existing serde conventions: Deserialize-only, `Option` scalars, `#[serde(default)]` collections, same derive set)

```rust
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct LangfuseConfig {
    #[serde(default)] pub enabled: bool,
    pub host: Option<String>,
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub content_mode: Option<String>,        // "metadata" | "full"
    pub user_id: Option<String>,
    pub release: Option<String>,
    #[serde(default)] pub tags: Vec<String>,
    pub environment: Option<String>,
    pub content_max_bytes: Option<usize>,
    #[serde(default)] pub redact_literals: Vec<String>,
    pub backfill_max_bytes: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub flush_interval_ms: Option<u64>,
    pub shutdown_flush_ms: Option<u64>,
    pub claude_dir: Option<String>,
    pub codex_dir: Option<String>,
    pub antigravity_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct ProfileLangfuse {                 // all-Option so one key can override
    pub enabled: Option<bool>,
    pub provider: Option<String>,            // "claude" | "codex" | "antigravity" | "none"
    pub content_mode: Option<String>,
    pub inject_session_id: Option<bool>,
}

// Config gains:  #[serde(default)] pub langfuse: Option<LangfuseConfig>
// Profile gains: pub langfuse: Option<ProfileLangfuse>
```

Adding `Profile.langfuse` forces edits at every `Profile` struct literal (`config.rs` default_profiles, the resume fallbacks at app.rs:1146/1165, test fixtures) — the compiler enumerates them; all get `langfuse: None`.

**Resolution semantics (who merges what, when):**

- At startup, `main` resolves the **global** section only: `LangfuseRuntime` is constructed iff `[langfuse]` is present, `enabled = true`, and both keys resolve (config or env). Otherwise `App::new` receives `None` and nothing below exists. Per-profile `enabled = true` cannot resurrect a missing or disabled global section — keys and the exporter are global.
- Per-profile merging happens **per launch, inside `rt.plan_launch(profile, dir)`**, which overlays the profile's `Some` fields (`enabled`, `provider`, `content_mode`, `inject_session_id`) on the runtime's resolved global settings. The `Profile` is already in hand at every spawn site, so no resolved-config map needs threading through `App` (the codebase has no merge machinery; this is the one merge point).
- `provider = "none"` means: skip CliKind detection and trace nothing for this profile — equivalent to `enabled = false` (no extras, no markers, no lifecycle trace). Use it for profiles whose command shadows a known basename but isn't actually that CLI.

### Secrets and env fallback

Resolution order per field: config value → `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST` → default host `https://cloud.langfuse.com`. The env lookup is injected as a function parameter so tests never mutate process env (same seam style as `resolve_command_with_path`). A host ending in `/api/public` is normalized before the OTLP path is appended. `enabled = true` with unresolvable keys does **not** abort startup — the TUI runs untraced with one status-bar line. Keys travel only in the `Authorization` header to the configured host; they are never written to disk, never injected into child environments, never logged (diagnostics render `pk-lf-...abcd` prefix+last4 only). A one-time status-bar warning fires when a `secret_key` is found in the **cwd-relative** `./profiles.toml` (commit hazard).

### The config.rs:59 acceptance-gate fix

Today a file containing only `[langfuse]` is silently discarded (acceptance requires `!cfg.profiles.is_empty()`). Fix (note the binding at config.rs:58 must become `let mut cfg = parse(...)`, and the fallback `Config` literal at config.rs:64–66 gains `langfuse: None`):

```rust
let mut cfg = parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
if !cfg.profiles.is_empty() || cfg.langfuse.is_some() {
    if cfg.profiles.is_empty() {
        cfg.profiles = Config::default_profiles();   // langfuse-only file: keep section, default profiles
    }
    return Ok(cfg);
}
```

**Deliberate precedence change, documented as intended:** today a `./profiles.toml` with zero profiles is skipped and `~/.agent-mux/profiles.toml` wins; with this fix, a cwd file containing only `[langfuse]` wins the search — its section applies and the *built-in default* profiles are used, shadowing any home-dir profiles and home-dir `[langfuse]`. This keeps first-match-wins semantics simple (no cross-file merging) at the cost of that shadowing, which is called out in `profiles.example.toml` next to the section. Users who customize profiles should put `[langfuse]` in the same file as their profiles.

## Per-CLI integration

Common to all three: transcripts are read read-only; a claim registry prevents two concurrent sessions from adopting the same transcript; and if correlation never succeeds, the lifecycle trace (below) still records the session with `correlation: "none"` — every agent-mux session is visible in Langfuse even when its content isn't.

### Claude Code — deterministic pre-assignment

- **Correlation:** `plan_launch` generates a UUID v4 and appends `--session-id <uuid>` (flag verified present on claude v2.1.251). The transcript path is then known up front: `<claude_dir>/projects/<project_slug(dir)>/<uuid>.jsonl`, reusing `history::project_slug`. `claude_dir` resolves: config override → `$CLAUDE_CONFIG_DIR` → `~/.claude`. If the computed path hasn't appeared after ~10 s, the watcher widens to a `projects/*/<uuid>.jsonl` filename glob — the UUID is the invariant, so slug-scheme drift (the documented 200-char+hash cap) cannot defeat correlation.
- **Resume:** `resume_history_session` already passes `--resume <session_id>`; plan detects it, injects nothing, correlates on the known id, and runs the prime pass. Hand-written profiles behave the same way: an explicit id in `--resume <id>` / `--session-id <id>` is parsed and correlated as `Known`; skipped-injection launches with no extractable id (bare `-r`, `--continue`, `-p`) correlate as `none` — lifecycle traces only, since the uuid-glob fallback needs a known uuid.
- **Parsing:** the existing Claude parser lifted into `transcript::claude` and extended to capture `message.usage` (input/output/cache_read/cache_creation tokens — currently discarded by history.rs), `message.model`, thinking blocks (→ observation metadata), and structured tool I/O (`tool_use.input` kept as a `serde_json::Value`; array-form `tool_result` content preserved instead of today's `""`). Tool pairing by `tool_use_id`.
- **Absence:** `claude` missing → dialog spawn error as today, no pipeline. Transcript never appears (e.g. `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1` in the user's env) → lifecycle trace only. Anthropic labels the JSONL format internal/unstable — but agent-mux already ships a parser for it (the history viewer); this design consolidates that drift surface into one module rather than adding a second.

### Codex — newest-rollout watch, cwd-verified

- **Correlation:** no pre-assign flag exists. Watch: each tick, scan `<codex_dir>/sessions/<YYYY>/<MM>/<DD>/` for the dates {t0, today} (midnight rollover) for unclaimed `rollout-*.jsonl` with mtime ≥ t0 − 2 s. For each candidate read its first line — a `session_meta` payload carrying `session_id` and, decisively, `cwd`. Adopt iff `cwd` equals the session's canonicalized working directory (agent-mux sets the child's cwd; Codex records it — an exact match). Rollout file creation is deferred until the first persisted item, so the watcher may legitimately wait minutes. zstd compression applies only to rollouts ≥ 7 days old and not in active use (in `sessions/` and `archived_sessions/` alike) — a live session's file is never compressed; if `.jsonl.zst` is ever encountered, degrade to lifecycle-only rather than adding a zstd dependency.
- **Parser (net-new, `transcript::codex`):** `RolloutLine {timestamp, ordinal?, type, payload}`:
  - `session_meta` → correlation + trace metadata (cwd, cli_version, model_provider, git).
  - `response_item`: `message` role `user` → User (turn boundary) / role `assistant` → Assistant; `reasoning` → thinking metadata; `function_call` / `custom_tool_call` / `local_shell_call` / `web_search_call` → ToolUse (real `call_id`s — clean pairing; `function_call.arguments` is a JSON *string*, parsed to a Value when possible); `*_output` → ToolResult by `call_id`.
  - `event_msg`: `token_count` → per-turn usage from `info.last_token_usage` (`input_tokens`, `cached_input_tokens`, `cache_write_input_tokens`, `output_tokens`, `reasoning_output_tokens`, `total_tokens`); `task_started` / `task_complete` / `turn_aborted` → explicit turn boundaries (Codex is the only CLI that provides these; the assembler prefers them, falling back to user-message boundaries).
  - Everything unrecognized is skipped, mirroring history.rs's lenient posture.
- **Injected at spawn:** nothing (markers aside). No `[otel]`, no hooks, no plugin install.
- **Absence:** codex is not installed on the dev machine — the parser is built from source-derived schema (openai/codex rollout crate, verified 2026-08-30) with fixtures, and ships fail-open behind the lifecycle-trace floor. First real-world run may need fixture corrections. The history *viewer* is not extended to Codex in v1 (non-goal); only the export path uses this parser. Users wanting deeper Codex fidelity (subagent nesting, per-model-call usage) can install the official `langfuse/codex-observability-plugin` instead/in addition — documented in the README with the explicit warning that it requires Langfuse keys in the child environment, where the agent's own tools can read them.

### Antigravity — brain-dir watch, heuristic but bounded

- **Directory convention:** the `antigravity_dir` config value (and its default `~/.gemini/antigravity-cli`) is the **antigravity-cli root**; the exporter derives `<root>/brain` and `<root>/presence` from it. The history viewer's `default_antigravity_dir()` (history.rs:87) currently returns the brain dir itself — it is refactored to compose from the same root helper so the override cannot yield `brain/brain` or point discovery one level too high (a one-line adjustment, noted in the Files table).
- **Correlation:** no pre-assign flag exists (`--conversation` only resumes; verified on agy 1.1.22). Watch: snapshot `<root>/brain/` subdirs at t0; each tick look for new unclaimed subdirs (the dir + transcript appear within ~2–4 s of activity). Confirmation, strongest first: (1) `presence/<uuid>.lock` mtime within [t0, now]; (2) early transcript lines contain the session's cwd as a substring (the heuristic history.rs already uses); (3) if after 15 s exactly one unclaimed candidate exists, adopt it. Heuristic adoptions are tagged `correlation: "heuristic"` in trace metadata so a human can discount misattribution (possible if an agy session is started outside agent-mux in the same cwd at the same moment).
- **File choice:** tail `.system_generated/logs/transcript_full.jsonl` when present (the file agy's own hooks point at; avoids the condensed file's `truncated_fields` content truncation), else `transcript.jsonl`. Same line schema.
- **Parsing:** existing `load_antigravity_log` logic lifted into `transcript::antigravity`, extended to keep `tool_calls[].args` as structured JSON and `thinking` alongside content. `USER_INPUT` (with `<USER_REQUEST>` extraction) → User; `PLANNER_RESPONSE` → Assistant + ToolUse per `tool_calls` entry; `GENERIC` → ToolResult with the existing is_error heuristic. Tool pairing is **positional** (no call ids); a mismatch closes spans with `statusMessage: "unpaired result"`.
- **Token usage:** none exists in interactive transcripts (usage appears only in `-p`/stream-json results). Antigravity traces show turn structure, content (in full mode), and timing — no token/cost numbers; trace metadata notes `usage: "unavailable"`.
- **Resume fix (bundled bug fix):** `resume_history_session` currently spawns bare `agy`, which starts a *new* conversation. It becomes `agy --conversation <id>` — fixing resume and making resume correlation deterministic.
- **Absence:** `agy` missing → dialog error as today. Brain dir absent or no conversation created → lifecycle trace only.

## Data mapping

### Endpoint, auth, headers

**`POST {host}/api/public/otel/v1/traces` — OTLP/HTTP, JSON encoding.** One wire format, one code path. Self-hosted requires Langfuse ≥ v3.22.0. The legacy `/api/public/ingestion` API is not used, not even as a fallback.

| Header | Value |
|---|---|
| `Authorization` | `Basic base64("pk-lf-…:sk-lf-…")` (base64 is a ~15-line encode-only local helper, RFC 4648 test-pinned) |
| `Content-Type` | `application/json` |
| `x-langfuse-ingestion-version` | `4` (real-time v4 ingestion; without it, up to 10 min delay) |
| `User-Agent` | `agent-mux/<CARGO_PKG_VERSION>` |

Success = 200 `{}`. No gzip (avoids flate2; batches are capped small against the 5 MB API limit).

**JSON, not protobuf**: protobuf needs prost + opentelemetry-proto codegen for a payload we can hand-write (~150 lines of serde structs). The OTLP/JSON sharp edges are pinned by golden tests: `traceId` = 32-char lowercase hex, `spanId`/`parentSpanId` = 16-char lowercase hex (OTLP's special case overriding proto3-JSON base64), lowerCamelCase field names, `startTimeUnixNano`/`endTimeUnixNano` as decimal **strings**, attributes as `[{"key": k, "value": {"stringValue"|"intValue"|"boolValue"|"arrayValue": …}}]`, status code integers (0 unset / 2 error). Resource block: `service.name = "agent-mux"`, `service.version = CARGO_PKG_VERSION`.

### Trace granularity

- **One trace per user turn**, grouped into a Langfuse session via `langfuse.session.id` = the CLI's own conversation id. Matches Langfuse's data model ("trace = one request/operation"; sessions group traces) and the official Codex plugin's shape. Traces appear near-real-time during long sessions; crash loss is bounded to the open turn's root span.
- **One lifecycle trace per session** (trace id derived from the launch id): a point-in-time `session_started` EVENT span emitted at spawn (profile, cwd, provider, correlation plan — complete on arrival, no reliance on OTLP re-send semantics) and a `session_ended` EVENT span in the same trace at exit (exit code — or `termination: "app_quit"` at shutdown — correlation outcome, adopted CLI session id, parse/drop counters). This makes every spawn visible in Langfuse immediately, including zero-turn sessions and sessions whose transcript never appeared. **Session-id caveat for watched providers:** the CLI session id does not exist at spawn time for Codex/Antigravity, so `session_started` carries `langfuse.session.id` only when the correlation is `Known` at spawn (Claude); `session_ended` carries the adopted id whenever adoption happened. Both spans always share the `launch_id` metadata attribute, which is the join key between the lifecycle trace and the per-turn traces regardless.
- **Emission timing:** child spans (generations, tool spans) export as soon as they individually complete; the turn's root span exports when the turn closes. Every span emitted after correlation redundantly carries `langfuse.session.id`, `langfuse.user.id`, and trace metadata attributes, so grouping and filtering survive a lost root span.

### Identifiers — deterministic via UUIDv5

- `trace_id` (16 bytes) = `Uuid::new_v5(AMX_NS, "amx1|{provider}|{cli_session_id}|turn|{turn_ordinal}")`; lifecycle trace id = `Uuid::new_v5(AMX_NS, "amx1|{launch_id}|lifecycle")`.
- `span_id` (8 bytes) = first 8 bytes of `Uuid::new_v5(AMX_NS, "{trace_key}|{event_index}|{kind}")`.
- `AMX_NS` is a fixed namespace UUID constant; stability tests pin exact vectors that must never change across releases.
- `turn_ordinal` = count of **assembler turn boundaries** since file start — whatever delimits turns for that provider (Codex `task_started`/`task_complete` events where present, else user messages) — not raw User-event count, since a Codex turn can be aborted and re-run without a new user message. The prime pass counts history on resume, so a resumed conversation's new turns continue the sequence and never collide with a prior run's trace ids. When the prime pass was truncated by `backfill_max_bytes`, the since-file-start count is unknowable, so the trace-id key is additionally salted with the `launch_id` for that run (turn traces still group into the same Langfuse session; only cross-run id determinism is sacrificed, flagged as `backfill_truncated`).
- Deterministic ids make retries and accidental re-parses converge on the same server-side objects; but correctness never *depends* on server-side replay semantics — the prime pass guarantees history is not re-emitted (OTLP replay behavior is a verify-at-implementation item, not a foundation).

### Field mapping

| Langfuse concept | Source | OTLP encoding |
|---|---|---|
| Trace name | metadata mode: `"turn <n>"`; full mode: `"{profile.name}: {first 80 chars of user text}"` | `langfuse.trace.name` on root span |
| `sessionId` | CLI conversation/session id | `langfuse.session.id` on every span emitted after correlation (lifecycle `session_started` carries it only for `Known`-at-spawn; see the caveat above) |
| `userId` | config `user_id` → `$USER`/`$USERNAME` → `"agent-mux"` | `langfuse.user.id` |
| `tags` | `["agent-mux", "{provider}"] + config.tags` | `langfuse.trace.tags` (arrayValue) |
| `release` | config `release` | `langfuse.release` |
| Trace metadata | `agent_mux_session`, `launch_id`, `profile`, `cwd`, `project_slug`, `transcript_path`, `agent_mux_version`, `correlation` (`deterministic`\|`watched`\|`heuristic`\|`none`), provider extras (Codex: cli_version, git), `parse_errors`, `dropped_spans` | `langfuse.trace.metadata.<key>` |
| Trace input/output | full mode only: turn's user text / last assistant text | `langfuse.trace.input` / `.output` on root at close |
| Turn root span | name `turn <n>`; start = user event ts, end = last event ts | plain span |
| Generation | Assistant event; type flips to generation via the model attribute | `gen_ai.request.model` (Claude `message.model` — skipped and routed to metadata when it is `"<synthetic>"`, which would otherwise mint bogus generations and defeat cost inference; Codex from `turn_context` where present — `session_meta.model_provider` is a provider label, not a model name; Antigravity absent); full mode: output = assistant text; thinking → `langfuse.observation.metadata.thinking` |
| Usage | Claude: per-assistant-line `message.usage` → that generation. Codex: per-turn `token_count.last_token_usage` → the turn's **final generation** (or the root span when the turn has none). Antigravity: none | `gen_ai.usage.<key>` — **integer-valued keys only**, verbatim → Langfuse `usageDetails`. Claude's `message.usage` also carries non-integer members (`cache_creation`, `output_tokens_details`, `server_tool_use` objects; `service_tier` etc. strings) — these are dropped or routed to observation metadata, never emitted as usage. A key named `cost` is never emitted (Langfuse special-cases `gen_ai.usage.cost` into costDetails) |
| Cost | not computed client-side; Langfuse infers from model + usageDetails where it knows the model | — |
| Tool call | ToolUse+ToolResult pair (Claude/Codex by id; Antigravity positional); one span, name = tool name | `langfuse.observation.type = "tool"`; full mode: input = structured args JSON, output = result content; `level = ERROR` + status 2 on `is_error`; unpaired at turn close → `statusMessage = "no result observed"` |
| Environment | config `environment` | `langfuse.environment` (verified: the documented attribute, alongside `deployment.environment[.name]`) |

**Timestamps.** Every **content-bearing** transcript event (user/assistant/tool lines) carries one ISO-8601 string (`timestamp` / `created_at` / rollout `timestamp`), parsed to unix nanos with `time` (RFC3339 incl. offsets and fractional seconds); Claude transcripts also contain auxiliary line types with no timestamp field at all. A missing timestamp — not just a parse failure — falls back to receive time with metadata `ts: "approx"`. A generation's start is approximated as the previous event's timestamp (transcript lines are written at completion) — flagged once per trace as `timing: "approximate"`; latency/TTFT in Langfuse is indicative, not measured.

**Batching.** The exporter flushes on any of: `flush_interval_ms` (3 s) since first queued span, 256 spans, or ~1 MB estimated body. Spans from different sessions share a batch (each carries its own trace id).

### Content modes and redaction

- **`metadata` (default):** no user text, assistant text, thinking, tool args, or tool output is exported. Structure, names, models, usage, timings, error levels, and all metadata still flow — useful traces with nothing sensitive by construction.
- **`full`:** content included, passed through (1) a built-in secret-pattern scanner — plain substring matching, no regex dependency — masking `sk-`, `pk-lf-`, `AKIA`, `ghp_`, `xox`, `Bearer <token>`, and `-----BEGIN …-----` blocks; (2) user-supplied `redact_literals`; (3) UTF-8-boundary-safe truncation at `content_max_bytes` with a marker. The residual risk — agents echoing secrets into transcripts in forms the scanner misses — is documented next to `content_mode` in `profiles.example.toml`.

## Failure posture

**Invariant: tracing can never break, block, or slow a session.** Structural guarantees: the pipeline reads files, not the PTY stream; every cross-component channel is bounded with `try_send` (full ⇒ drop span, count, one status-bar notice); all network I/O lives on the exporter thread; all errors in langfuse code are swallowed into at most a status-bar line; shutdown flush is hard-capped and breaker-aware.

| Failure | Behavior |
|---|---|
| Network down / DNS / 5xx / timeout | Per-batch: 3 attempts (connect 5 s, total 15 s each), backoff 1 s → 4 s ±25 % jitter, then drop the batch. After 5 consecutive failed batches the **circuit breaker** opens for 60 s, then a single half-open probe. Queue is a conduit, not a durable buffer — no unbounded memory, no disk spool. |
| 429 | Honor `Retry-After` (capped 60 s), at most 2 waits per batch, then drop. Does not count toward the breaker. Steady state is ~1 request per flush tick for the whole app — far under even the Hobby-tier ingestion limits. |
| 401/403 | 2 consecutive ⇒ exporter disabled for the run; one status-bar message. |
| 400/413 | Drop that batch (our bug; retrying can't help), note once. |
| Transcript never appears | Lifecycle trace records the session with `correlation: "none"`. |
| Transcript schema drift | Lenient `serde_json::Value` parsing (house style): unknown types/fields skipped; per-file `parse_errors` counters exported in metadata; zero recognized events ⇒ lifecycle-only. The shared `transcript.rs` module means drift breaks the history viewer visibly and gets fixed once for both consumers. |
| Torn tail / truncation | Only newline-terminated lines parse; remainder buffer carries partials across polls; `len < offset` resets the tailer and re-primes without emitting. |
| App crash / panic | Turn-complete data already shipped; the open turn's root span and final flush are lost. Accepted. |
| Quit with unflushed spans | Bounded flush (default 3 s, configurable, 0 = skip); skipped instantly when the breaker is open; on timeout, drop and exit. |
| Antigravity misattribution | cwd + lock-mtime + claim-registry bounds it; residual risk tagged `correlation: "heuristic"`. |

## `agent-mux langfuse doctor`

A one-shot subcommand (arg dispatch in `main` before the TUI starts) attacking the silent-no-trace failure mode of any opt-in integration:

- Resolves the effective config (which file, which env fallbacks) and prints it with masked keys (`pk-lf-...abcd`).
- Warns on `pk-lf-`/`sk-lf-` prefix mismatches and on a secret key found in the cwd-relative `./profiles.toml`.
- POSTs an empty `resourceSpans` batch to the resolved endpoint with the exact headers the exporter uses, expecting `200 {}` — end-to-end host+auth validation.
- Prints per-provider correlation readiness: command on PATH; `claude` version supports `--session-id`; `~/.claude` / `~/.codex/sessions` / `~/.gemini/antigravity-cli/brain` present.

## Dependencies

Current deps: anyhow, arboard, crossterm, portable-pty, ratatui, serde, serde_json, tokio, toml, tui-term, vt100 — no HTTP client. Additions (3):

| Crate | Config | Why |
|---|---|---|
| `ureq` | `default-features = false, features = ["rustls"]` | Blocking HTTP/1.1 + TLS on the one exporter OS thread — the repo's blocking-threads-bridged idiom, with a flush join point independent of tokio teardown. `reqwest` would drag the hyper/tower async stack for a sequential POST loop. rustls/ring is the one heavy transitive cost — the floor price of HTTPS without system-OpenSSL coupling (Windows-first + WSL2 targets). |
| `uuid` | `features = ["v4", "v5"]` | v4 for `--session-id` pre-assignment and launch ids; v5 for deterministic trace/span id derivation (well-tested SHA-1 hashing instead of hand-rolled FNV constants). |
| `time` | `default-features = false, features = ["parsing"]` | Correct RFC3339 → epoch-nanos across three providers' timestamp styles (offsets, 0–9 fractional digits). `chrono`/`jiff` are bigger; hand-rolled calendar math is where silent timestamp bugs come from. |

**Explicitly rejected:** `opentelemetry`/`opentelemetry-otlp`/`opentelemetry-langfuse` (large SDK tree whose batch/flush/id-generation semantics fight our drop/breaker/deadline policy and deterministic ids); `langfuse-ergonomic`/`langfuse-client-base` (built on the OpenAPI spec ⇒ the ingestion API that dies on Cloud 2026-11-16); `notify` (inotify is unreliable on WSL2/9p — polling matches the repo idiom and step-granularity transcripts); `prost`/protobuf; `zstd` (live rollouts are never compressed); `regex` (literal masking suffices); `base64` (15-line local helper).

## Implementation plan

### Files

| File | Responsibility |
|---|---|
| `src/config.rs` (edit) | `LangfuseConfig`, `ProfileLangfuse`, `Profile.langfuse`, global-section resolution with env-fallback seam, acceptance-gate fix (per-profile merging lives in `plan_launch`). |
| `src/transcript.rs` (new) | Shared parsers: `TranscriptEvent` (User / Assistant{model, thinking, usage} / ToolUse{id, name, args: Value} / ToolResult{id, content: Value, is_error} / TurnBoundary / SessionMeta, each with raw ISO ts + parsed nanos), `detect_provider`, `parse_claude_line`, `parse_antigravity_line`, `parse_codex_line` (net-new). Lenient, Value-based. |
| `src/history.rs` (edit) | `load_claude_log` / `load_antigravity_log` become adapters over `transcript::` producing today's `LogEntry` **bit-for-bit** (existing tests pin the parity). Discovery/slug reused by the exporter; `default_antigravity_dir` refactored to compose `<root>/brain` from a shared root helper (one line) so the `antigravity_dir` override serves both consumers. |
| `src/langfuse/mod.rs` (new) | `LangfuseRuntime` (settings, claim registry, span_tx, exporter handle, JoinSet), `plan_launch`, `start_session`, `shutdown`; `SessionTraceHandle`. |
| `src/langfuse/correlate.rs` (new) | `CorrelationSpec` (Known / Watch); Claude path + uuid-glob widening; Codex date-dir scan + `session_meta` cwd verification; Antigravity brain-dir diff + lock/cwd heuristics. |
| `src/langfuse/tail.rs` (new) | Poll tailer: offsets, remainder buffer, prime pass (+ `backfill_max_bytes`), truncation reset, grace sweep. |
| `src/langfuse/map.rs` (new) | Turn assembler: TranscriptEvent stream → OTLP spans; ordinals; tool pairing (id / positional); content modes, masking, truncation. |
| `src/langfuse/otlp.rs` (new) | Serde structs for `ExportTraceServiceRequest` (hex ids, string nanos, AnyValue), UUIDv5 id derivation, base64 helper. |
| `src/langfuse/export.rs` (new) | Exporter thread: `blocking_recv` loop, batching, ureq POST, retry/429/auth policy, circuit breaker, completion oneshot, drop counters. |
| `src/langfuse/doctor.rs` (new) | The `langfuse doctor` subcommand. |
| `src/session.rs` (edit) | `spawn(.., extra_args, extra_env)`; `pub trace: Option<SessionTraceHandle>`. |
| `src/app.rs` (edit) | `App::new(profiles, langfuse, tx)`; plan+attach at the three spawn sites; injected-args fast-failure hint (step 2); `mark_exited` in `handle_pty_exit` and the `forward_bytes` failure arm; Antigravity resume `--conversation <id>`; `take_langfuse()`. |
| `src/events.rs`, `src/main.rs` (edit) | `AppEvent::LangfuseStatus`; arg dispatch for `langfuse doctor`; runtime construction; `rt.shutdown(deadline)` after `kill_all`. |
| `profiles.example.toml` (edit; the repo has no README) | `[langfuse]` documentation incl. env-var recommendation, content_mode warning, Codex plugin note (with its child-env credential warning), minimum-`claude`-version note for `--session-id`. |

### Order of work — each stage lands green and is independently abandonable

1. **Config**: structs + resolver + gate fix + tests. Nothing consumes it yet — zero-risk landing.
2. **`src/transcript.rs` extraction** with strict parity: move Claude/Antigravity parsing behind `TranscriptEvent`; all existing history/viewer tests pass unchanged. *This lands as its own change before any exporter code exists.*
3. **Codex parser** + source-derived fixtures.
4. **`otlp.rs`**: types, UUIDv5 ids (stability vectors), base64, golden-JSON request tests.
5. **`map.rs`** turn assembler (pure, fixture-driven; both content modes; masking).
6. **`tail.rs` + `correlate.rs`** (tempdir-driven via the `*_dir` overrides).
7. **`export.rs`** with a std-`TcpListener` stub server in tests (no new dev-deps).
8. **Wiring**: session/app/main/events; doctor; example config; status-bar surfacing.
9. **End-to-end test** in the `tests/pty_session.rs` style.

Rough scale: ~2.3k lines including tests.

### Tests (repo conventions: inline `#[cfg(test)]` for pure logic, `tests/*.rs` + tempfile for integration)

- **config**: full/partial/absent section; per-profile merge; gate fix (langfuse-only file → default profiles + section, including the documented cwd-shadowing precedence); env fallback via injected fn; enabled-without-keys → untraced.
- **plan_launch**: CliKind basename classification + `provider` override + `provider = "none"`; every injection-skip condition (`--resume`/`-r`, `--session-id`, `--continue`/`-c`, `-p`/`--print`, `inject_session_id = false`); explicit-id extraction → `Known`; no-id skip → `none`; marker `extra_env`; disabled profile → no plan.
- **transcript**: Claude assistant line with usage + thinking + array tool_result + `"<synthetic>"` model + non-integer usage members routed correctly; Antigravity PLANNER_RESPONSE with tool_calls; Codex session_meta / message / function_call(+output) / token_count / task_complete; timestamp-less Claude auxiliary lines; malformed-line skipping; `detect_provider`.
- **history parity**: existing tests unchanged — the pin for stage 2.
- **otlp**: UUIDv5 stability vectors (must never change across releases); golden serialized request (hex ids, string nanos, arrayValue tags); base64 RFC vectors.
- **map**: user→assistant→tool→result→user yields turn root + generation + tool span with correct parents/times; error result ⇒ ERROR level; unpaired tool close; Codex `task_started`/`task_complete` boundaries incl. aborted-then-rerun ordinal behavior; Antigravity positional pairing; lifecycle `session_started`/`session_ended` spans (id caveat for watched providers); missing-timestamp `ts: "approx"` and once-per-trace `timing: "approximate"` flags; **metadata mode asserts the absence of every content attribute**; masking hits each built-in pattern + literals; truncation.
- **tail** (`tests/langfuse_tail.rs`): appends across polls; partial line held then completed; late-appearing file; `Known`-resume file primes without emitting; **watch-adopted file's first lines ARE emitted**; backfill cap + launch-salted ordinals; truncation reset; grace sweep closes the open turn on phase `Exited`; shutdown watch triggers the single final pass.
- **correlate** (`tests/langfuse_correlate.rs`): fabricated `sessions/` and `brain/` trees under tempdir; claim registry prevents double adoption **and releases on pipeline exit** (exit-then-resume re-adopts); Codex cwd mismatch rejected; Claude glob fallback; Antigravity three-tier confirmation (lock mtime, cwd substring, 15 s single-candidate) and the `correlation: "heuristic"` tag.
- **export** (`tests/langfuse_export.rs`): stub server asserts auth/headers/body; 429 `Retry-After` honored; two 401s disable; breaker opens after 5 failures and half-opens; queue-full drops counted; shutdown deadline respected with a hanging server; breaker-open skips the final flush; once-per-class status emission; the doctor empty-batch probe against the same stub.
- **e2e** (`tests/langfuse_session.rs`, unix-gated): fake profile whose command is a script appending Claude-shaped JSONL into `<tmp claude_dir>/projects/<slug>/<uuid>.jsonl` (uuid taken from the injected `--session-id` argv), driven via a copy of the `pump_until` helper in the style of `tests/pty_session.rs` / `tests/app_flow.rs` (the repo has no shared test module; a `tests/common/mod.rs` may be introduced here) against a stub server; asserts turn spans **and both lifecycle spans** arrive and flush completes after `kill_all` within the deadline.

### Pre-release live spike (gating)

Before the feature is called done, one manual run against a real Langfuse project must verify the items research could not: hex id + string-nano encoding acceptance; `x-langfuse-ingestion-version: 4` real-time behavior; `sessionId` grouping renders as expected; the doctor's empty-`resourceSpans` probe response; OTLP replay/duplicate-span behavior (informs whether deterministic-id re-sends are safe or must stay avoided); Codex `turn_context` model extraction against a real install when available.

## Out of scope (v1)

- Langfuse scores/evals, prompt management, datasets; client-side cost computation.
- The legacy ingestion API, OTLP protobuf/gzip, OTLP logs/metrics signals.
- Hook-based or CLI-native-OTel paths; installing the Codex Langfuse plugin (documented as a user option only).
- Codex in the history viewer UI (`AgentProvider::Codex`) — only the export parser ships; the viewer extension is a natural follow-up on `transcript.rs`.
- Durable offline span spooling; per-tab live trace indicator; per-content-kind redaction switches (content_mode is binary in v1); sampling; tracing sessions started outside agent-mux.

## Risks

1. **Transcript schema drift** (highest likelihood; Anthropic explicitly labels Claude's format internal). Mitigated: one consolidated parser module shared with the shipped viewer (drift breaks visibly, gets fixed once), lenient parsing, `parse_errors` counters, lifecycle-trace floor. Residual: silent semantic drift could under-report until noticed.
2. **Codex parser unverifiable locally** — source-derived fixtures only until a real install exists; ships fail-open.
3. **Antigravity correlation is heuristic** and contributes no token usage in interactive mode; bounded and flagged, not solved.
4. **Timing fidelity**: one timestamp per transcript event ⇒ generation start times and TTFT are approximations, flagged in metadata.
5. **Exit races**: `kill_all` can beat the CLI's final flush; grace sweep + per-line flushing makes loss rare, but a killed session's last assistant message can be lost; panic paths lose the open turn.
6. **Dependency weight**: rustls/ring via ureq is the single heavy addition — accepted as the floor cost of HTTPS.
7. **Privacy in full mode**: masking is literal-pattern-based; an agent echoing a secret in an unlisted shape exports it. `metadata` remains the default precisely for this reason.
