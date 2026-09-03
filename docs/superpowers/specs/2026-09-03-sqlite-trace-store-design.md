# agent-mux Local Trace Store — replacing Langfuse with SQLite

**Date:** 2026-09-03
**Status:** Approved design, implemented (plan: `../plans/2026-09-03-sqlite-trace-store.md`)
**Supersedes:** the *sink* half of `2026-08-30-langfuse-integration-design.md` (endpoint, OTLP encoding, exporter thread, HTTP failure policy, `langfuse doctor`). That spec's **correlation, tailing, transcript parsing, and turn-assembly semantics stay authoritative** and are referenced, not restated, below.
**Builds on:** v1 (`2026-08-29-agent-mux-design.md`), session logs (`2026-08-30-session-logs-design.md`)

## Purpose

agent-mux currently observes every Claude Code, Codex, and Antigravity session it spawns and ships the result to Langfuse as OTLP spans. This design removes Langfuse and every network dependency from that path: the same pipeline (correlate → tail → parse → assemble) writes typed rows into **one local SQLite file**, and agent-mux itself provides the query surfaces Langfuse used to provide — a `trace` CLI and, in a second phase, a TUI trace browser. Nothing leaves the machine, nothing needs keys, and tracing works with zero configuration.

## Decision summary

**Keep the pipeline, swap the sink, move Langfuse's server-side logic into the client.** Everything upstream of `export.rs` is already provider-independent and well tested (`plan_launch`, the claim registry, `correlate.rs`, `tail.rs`, `transcript.rs`, and the turn assembler in `map.rs`). What changes:

1. The assembler emits **typed rows** (`StoreOp`) instead of OTLP spans with `langfuse.*` attribute bags.
2. The exporter OS thread becomes a **writer OS thread** owning the single SQLite connection: same bounded `try_send` queue, same batching, same joinable shutdown point, no HTTP.
3. Four pieces of logic Langfuse performed server-side move in-process, because there is no server: attribute decoding → typed columns; usage normalization → provider-aware (`usage.rs`); model matching + pricing → `pricing.rs` with a bundled, user-extensible price table; trace/session aggregations → SQL views.
4. The lifecycle "trace" (two point-in-time event spans) becomes a proper **`launches` row**, updated at start and end.
5. Deterministic ids stay exactly as they are (UUIDv5 under `AMX_NS`). Every write is an **idempotent upsert**, so re-parses, resumes, and explicit re-imports converge on the same rows — a property the Langfuse path could only hope for.

Alternatives rejected in review:

| Alternative | Why not |
|---|---|
| Keep OTLP spans and store one row per span with a JSON attribute bag (the shape of Langfuse's own v4 `events` table) | Stringly-typed; every useful query needs `json_extract`; no way to show an in-progress turn; carries OTLP baggage (hex ids, `AnyValue` wrappers) for a consumer that no longer exists. Langfuse needs that shape because it ingests from dozens of instrumentors; agent-mux has exactly one. |
| Self-host Langfuse (docker compose: web, worker, Postgres, ClickHouse, Redis, MinIO) | Five services to keep a TUI's telemetry — not "only a SQLite db". |
| A generic local OTel backend (Jaeger, Tempo, otel-desktop-viewer) | No LLM semantics (usage, cost, sessions); still a server; still no cost inference. |
| Dual sinks (SQLite + optional Langfuse) | Doubles the surface to test and keeps `ureq`/`rustls` in the tree. The schema keeps deterministic ids and full fidelity, so a Langfuse *re-export from the DB* can be added later as a standalone command without touching the live pipeline. Out of scope for this design. |

## Requirements

1. **Zero setup.** With no config at all, tracing is on, and the database is created at `~/.agent-mux/traces.db` on first use. (Decision D1 below.)
2. **Same coverage as today.** Every session spawned from agent-mux (dialog, resume, respawn) and every session attached via the `t` toggle gets a `launches` row; once correlated, full per-turn traces and observations.
3. **Session identity** is the CLI's own conversation id, keyed by provider, so the store lines up with each CLI's native resume story and with the history viewer.
4. **Privacy.** The file is created `0600` on unix. Content modes (`metadata` / `full`), secret masking, `redact_literals`, and truncation are unchanged in mechanism. Default content mode becomes `full` (Decision D2).
5. **Fail-open invariant, unchanged.** No failure in the store (unwritable path, disk full, corruption, lock contention, schema from a newer binary) may break, block, or slow a session or the app. Quit latency is hard-capped (default 1 s, down from 3 s: local commits take milliseconds).
6. **Zero CLI-config mutation, unchanged.**
7. **Idempotent.** Writing the same row twice yields one row. Re-importing a transcript converges.
8. **Queryable by anything.** The schema is documented, versioned, and stable; `sqlite3`, DB Browser, datasette, or a Python script must be able to answer "what did this session cost" without agent-mux.
9. **Correct cost.** Cost is computed locally with provider-correct usage semantics (see Usage) from a bundled price table the user can override. Unpriced models are visible, never silently zero.
10. **No network dependency.** `ureq`, `rustls`, `ring`, and the base64 helper leave the dependency tree.

## Architecture

### What runs where

```
main() ── config::load() ── resolve [tracing] ──► Option<TraceRuntime>   (None only when enabled = false
                                                     │                    or the DB cannot be opened)
App (main loop, existing)                            │ owns
 │  spawn site (x3) + attach (t)                     ▼
 │   ├─ plan = rt.plan_launch(&profile, &dir)   TraceRuntime
 │   ├─ Session::spawn(.., extra_args, env)      ├─ claim registry                (unchanged)
 │   └─ session.trace = rt.start_session(plan)   ├─ op_tx: sync_channel<StoreOp>(8192, try_send)
 │                                               ├─ shutdown watch                (unchanged)
 │                                               └─ writer OS thread: one rusqlite Connection,
 │                                                  WAL, one transaction per flush, breaker
 │  per session: one tokio task (unchanged)                   ▲
 │   correlate ─► tail ─► parse ─► assemble ──── StoreOps ────┘
 │
 └─ exit: app.kill_all()  →  rt.shutdown(deadline ≤ 1 s)

Readers — each opens its own read-only connection; WAL means they never block the writer:
   `agent-mux trace …` CLI  ·  TUI trace browser (phase 2)  ·  sqlite3 / any GUI
```

- The per-session pipeline task is unchanged except for the type it sends: `StoreOp` instead of `otlp::Span`.
- The writer is one dedicated `std::thread`, for the same reasons the exporter was: a single place for batching and failure policy, and a join point that survives tokio runtime teardown after `kill_all`.
- SQLite's connection is `!Sync`; one thread owning it is also the simplest correct concurrency story. Readers use separate connections.

### The writer's input: `StoreOp`

```rust
pub enum StoreOp {
    /// Upsert. Sent at spawn (plan known) and again at end (termination,
    /// exit code, counters, correlation outcome, adopted session).
    Launch(LaunchRow),
    /// Upsert. Sent on adoption (session id known) and whenever session
    /// metadata is learned (Codex session_meta, first user prompt → title).
    Session(SessionRow),
    /// Upsert. Sent at turn OPEN (status 'open', name, input) and at turn
    /// CLOSE (output, thinking, skills, reported duration, final status).
    /// Also bumps sessions.last_seen_ns.
    Trace(TraceRow),
    /// Upsert. Generations at completion; tool/agent rows at ToolUse
    /// (end_ns NULL) and again at ToolResult / unpaired close. The writer
    /// computes model_id and cost before insert.
    Observation(ObservationRow),
}
```

Row structs mirror the tables 1:1 with `Option` for anything that can be unknown at first write. **Upsert rule:** `INSERT … ON CONFLICT(id) DO UPDATE SET col = COALESCE(excluded.col, col)` for every column — a later write with more information wins, a later write with less never erases (a turn goes `open → closed`, `end_ns NULL → value`; nothing in the pipeline ever needs to clear a value).

### Per-session lifecycle — deltas from the current spec

Steps 1–4 (plan, spawn, attach, correlate) are byte-for-byte the current behavior. The differences:

| Step | Today (Langfuse) | This design |
|---|---|---|
| Attach | `session_started` event span | `StoreOp::Launch` with plan facts; `StoreOp::Session` too when the id is already Known (Claude injection/resume, `agy --conversation`). |
| Adoption | assembler learns session id | plus `StoreOp::Session` (transcript path, cwd, correlation outcome) and `StoreOp::Launch` (`session_key`, `correlation`). |
| Turn open (User event / Codex `task_started`) | nothing emitted until close | `StoreOp::Trace { status: "open", name, input, start_ns }` — the browser can show a turn in progress. |
| ToolUse | nothing until result | `StoreOp::Observation { end_ns: None }` — "Bash running for 12 s" is visible; the prime pass still emits nothing. |
| Turn close | root span | `StoreOp::Trace` with `status: closed|aborted`, `end_ns`, output, rollup-free (aggregates are views). |
| Usage that no generation took (tool-only turn, Codex per-turn count with buffered generations) | parked on the root span, uncosted | a generation row named `assistant (usage only)`, `kind = 'usage_only'`, model = the assembler's last known model, so it is priced. Usage never sits on a trace. |
| End | `session_ended` event span | `StoreOp::Launch` update: `ended_ns`, `termination`, `exit_code`, `parse_errors`, `dropped_ops`, `reported_*` from Claude `cost-state`. |
| App shutdown | ≤ 3 s, skipped when breaker open | ≤ 1 s. The final drain is skipped only when the writer is disabled (open failure / breaker). |
| Startup | — | **Recovery sweep** (below) closes rows left `open` by a crash. |

`Phase`, `SessionTraceHandle`, the phase/shutdown watches, the grace sweep, the fast-failure `--session-id` heuristic, `plan_attach`, and the claim registry are untouched.

### Writer thread

Open sequence (once, on the writer thread; failure ⇒ runtime disabled + one notice):

1. Create parent dir if missing. Open `db_path` read-write-create. On unix, create the file with mode `0600` (`OpenOptions` + `mode(0o600)` before SQLite touches it, so the WAL/SHM siblings inherit the umask-independent restriction as far as SQLite allows).
2. `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;` and, for a fresh file only, `PRAGMA auto_vacuum = INCREMENTAL` before the first table.
3. Migrations: `PRAGMA user_version` vs the binary's `SCHEMA_VERSION`. Lower ⇒ apply the missing migrations in one transaction. Higher ⇒ **refuse** (a newer agent-mux wrote it; downgrades are unsupported) — runtime disabled, notice names both versions.
4. Seed/sync the `models` table (see Pricing). Insert a `runs` row for this process.
5. Recovery sweep.
6. Enter the loop.

Loop: `recv_timeout` on the op queue; ops accumulate into a batch that is committed as **one transaction** when `flush_interval_ms` (default 250) elapses since the first queued op, or at 512 ops. Every commit also updates `runs.heartbeat_ns`. On `Disconnected` (all senders gone at shutdown): final drain, `runs.ended_ns`, done. Cost computation (`pricing.rs`) runs on this thread just before the observation upsert — it is a hash-map lookup plus four multiplications.

Failure policy per batch:

| Result | Behavior |
|---|---|
| `SQLITE_BUSY` after `busy_timeout` (another process holds the write lock) | retry the batch up to 3× (50 / 200 / 800 ms), then drop it, count, one notice. |
| `SQLITE_FULL`, `SQLITE_IOERR`, `SQLITE_READONLY`, `SQLITE_CORRUPT`, `SQLITE_NOTADB` | drop the batch, count; 5 consecutive failed batches open the breaker (60 s, then one probe batch) — same `Breaker` struct as today. Notice: `tracing paused: <sqlite message>`. |
| Constraint / programming error | our bug: drop that batch, note once with the SQLite message. |
| Queue full (`try_send` fails) | drop the op, count, once-per-run notice — unchanged. |

The queue is a conduit, not a durable buffer: no unbounded memory, no spill file. Dropped counts land on the launch row at end.

### Recovery sweep

Runs at open, guarded by heartbeats so that two agent-mux processes sharing a DB never close each other's live turns:

```sql
-- runs whose writer stopped heartbeating (crash, SIGKILL) and never ended
UPDATE runs SET ended_ns = heartbeat_ns, termination = 'crash'
 WHERE ended_ns IS NULL AND id <> :me AND heartbeat_ns < :now - 120e9;
UPDATE launches SET termination = 'crash',
       ended_ns = COALESCE((SELECT MAX(COALESCE(o.end_ns, o.start_ns)) FROM traces t
                            JOIN observations o ON o.trace_id = t.id WHERE t.launch_id = launches.id),
                           started_ns)
 WHERE ended_ns IS NULL AND run_id IN (SELECT id FROM runs WHERE termination = 'crash');
UPDATE traces SET status = 'closed', closed_by = 'recovery',
       end_ns = COALESCE((SELECT MAX(COALESCE(end_ns, start_ns)) FROM observations WHERE trace_id = traces.id), start_ns)
 WHERE status = 'open' AND launch_id IN (SELECT id FROM launches WHERE termination = 'crash');
UPDATE observations SET end_ns = start_ns, status_message = COALESCE(status_message, 'no result observed (recovery)')
 WHERE end_ns IS NULL AND trace_id IN (SELECT id FROM traces WHERE closed_by = 'recovery');
```

Committed batches survive a crash (WAL); only the open turn's close is lost, and the sweep makes that loss visible instead of leaving a phantom in-progress row.

### UI feedback

`AppEvent::LangfuseStatus(String)` is renamed `AppEvent::TraceStatus(String)`; same routing to the status bar, same once-per-class discipline. The class list becomes: (1) DB could not be opened (path, permission, corruption, newer schema); (2) `[langfuse]` section found (deprecation, see Config); (3) injected-`--session-id` fast-failure hint (unchanged); (4) writer paused (breaker); (5) dropped-ops warning; (6) DB on a 9p/NTFS mount (`/mnt/*` under WSL — SQLite locking is unreliable there; suggest a path on the Linux filesystem).

## Config

### TOML

```toml
# Local session tracing. ON by default; the database is created on first use.
# Everything stays on this machine — no keys, no network.
[tracing]
# enabled = true
# db_path = "~/.agent-mux/traces.db"      # or $AGENT_MUX_TRACE_DB; parent dir is created
# content_mode = "full"                   # "full" (default): prompts, responses, thinking, tool I/O, masked and
#                                         # truncated. "metadata": names, timings, models, usage, errors only.
# user_id = "me"                          # default: $USER / $USERNAME
# tags = ["agent-mux"]                    # extra labels stored on every launch
# release = "rollout-1"                   # free-form labels, kept for continuity
# environment = "development"
# content_max_bytes = 65536               # per-field truncation cap (full mode)
# redact_literals = []                    # extra literal substrings masked in full mode
# backfill_max_bytes = 4194304            # prime-pass cap for huge resumed transcripts
# poll_interval_ms = 500
# flush_interval_ms = 250                 # batch commit cadence
# shutdown_flush_ms = 1000                # 0 = never wait at quit
# retention_days = 0                      # 0 = keep forever; otherwise pruned at startup
# claude_dir = "/custom/.claude"          # dir overrides (mostly for tests)
# codex_dir = "/custom/.codex"
# antigravity_dir = "/custom/.gemini/antigravity-cli"

# Price overrides / additions. USD per 1M tokens. `match` entries are
# lowercase; a trailing '*' is a prefix match. Overrides a builtin by id.
# [[tracing.models]]
# id = "claude-sonnet-5"
# provider = "anthropic"
# match = ["claude-sonnet-5", "claude-sonnet-5-*"]
# input = 2.0
# output = 10.0
# cache_read = 0.20
# cache_write = 2.50           # 5-minute cache writes
# cache_write_1h = 4.00        # 1-hour cache writes (Claude only)
# reasoning = 14.0             # optional; when absent reasoning tokens are billed as output

[[profiles]]
name = "Claude Code"
command = "claude"
args = []

# Per-profile overrides (all optional):
# [profiles.tracing]
# enabled = true
# provider = "claude"          # force CLI-kind detection for wrapper commands; "none" = never trace
# content_mode = "metadata"    # dial one profile down
# inject_session_id = false    # never add --session-id (needs claude >= 1.0.34)
```

### Rust shapes

```rust
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct TracingConfig {
    pub enabled: Option<bool>,                 // default true
    pub db_path: Option<String>,
    pub content_mode: Option<String>,          // "full" (default) | "metadata"
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
    pub retention_days: Option<u32>,
    pub claude_dir: Option<String>,
    pub codex_dir: Option<String>,
    pub antigravity_dir: Option<String>,
    #[serde(default)] pub models: Vec<ModelPriceConfig>,
    // Deprecated Langfuse keys, still parsed so an old [langfuse] section
    // loads; presence of any of them triggers the one-time notice.
    pub host: Option<String>,
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelPriceConfig {
    pub id: String,
    pub provider: Option<String>,
    #[serde(default)] pub r#match: Vec<String>,
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub cache_write_1h: Option<f64>,
    pub reasoning: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct ProfileTracing {                    // was ProfileLangfuse; same fields
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub content_mode: Option<String>,
    pub inject_session_id: Option<bool>,
}

// Config:  #[serde(default, alias = "langfuse")] pub tracing: Option<TracingConfig>
// Profile: #[serde(alias = "langfuse")]          pub tracing: Option<ProfileTracing>

pub struct ResolvedTracing {                   // replaces ResolvedLangfuse
    pub db_path: PathBuf,
    pub content_mode: ContentMode,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub tags: Vec<String>,
    pub environment: Option<String>,
    pub content_max_bytes: usize,
    pub redact_literals: Vec<String>,
    pub backfill_max_bytes: u64,
    pub poll_interval_ms: u64,
    pub flush_interval_ms: u64,
    pub shutdown_flush_ms: u64,
    pub retention_days: u32,
    pub claude_dir: Option<PathBuf>,
    pub codex_dir: Option<PathBuf>,
    pub antigravity_dir: Option<PathBuf>,
    pub models: Vec<ModelPrice>,               // config overrides, already validated
    pub legacy_langfuse_keys: bool,            // drives the deprecation notice
}
```

### Resolution semantics

- `resolve_tracing(cfg: Option<&TracingConfig>, env)` returns `None` only when `enabled = Some(false)`. An absent section resolves to defaults — tracing is on (D1). It never errors.
- `db_path`: config → `$AGENT_MUX_TRACE_DB` → `~/.agent-mux/traces.db` (`~` expanded via `HOME`/`USERPROFILE`, matching `config::load`). Relative paths are resolved against the cwd at startup and then frozen.
- Per-profile merging stays in `plan_launch` exactly as today (`enabled`, `provider`, `content_mode`, `inject_session_id`). The launch dialog's "Tracing" and "Content mode" toggles keep writing `profile.tracing` overrides.
- `LANGFUSE_*` environment variables are no longer read. A `[langfuse]` section, or `[profiles.langfuse]`, loads through the serde alias with its network keys ignored, and produces one notice: `tracing: [langfuse] is now [tracing] — local SQLite store; host/keys ignored`. The alias is removed in a later release.
- The `config.rs` acceptance gate (`profiles.is_empty() || langfuse.is_some()`) becomes `|| tracing.is_some()`; the documented cwd-shadowing precedence is unchanged.

## Storage

### File, size, concurrency

- One file, `traces.db`, plus SQLite's `-wal` and `-shm` siblings while open. WAL is checkpointed automatically (default 1000 pages); `trace prune --vacuum` runs `PRAGMA wal_checkpoint(TRUNCATE)` and `VACUUM`.
- Size expectations: metadata mode ≈ 300–500 B per observation; full mode is dominated by tool output, capped by `content_max_bytes` (64 KiB) per field, typically 1–5 KB per observation. A heavy day (≈ 10 k observations) is 5–50 MB in full mode. `retention_days` and `trace prune` are the controls; the doctor reports the file size.
- Timestamps are `INTEGER` unix **nanoseconds** (`i64`; the pipeline's `i128` is clamped). Views expose ISO-8601 via `datetime(x / 1e9, 'unixepoch')`.
- Money is `REAL` USD. For a personal store the f64 error on sums is irrelevant (≈1e-10 relative over a million rows); Langfuse uses `Decimal(18,12)` because it bills tenants.
- Two agent-mux processes on one DB are supported by WAL + `busy_timeout`; each has its own claim registry (pre-existing limitation: both may adopt the same Codex rollout), and because ids are deterministic their duplicate writes converge instead of duplicating.
- WSL2 note: SQLite on `/mnt/c` (9p / drvfs) has unreliable locking. The default path is under `$HOME` on the Linux filesystem; the doctor and startup notice warn when `db_path` is under `/mnt/`.

### Schema v1

`PRAGMA user_version = 1`. `traces` and `observations` carry an `INTEGER PRIMARY KEY` rowid plus a `UNIQUE` text id: an implicit rowid can be renumbered by `VACUUM`, which would corrupt external-content FTS tables, and a stable integer key is cheaper to join on.

```sql
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL                       -- schema_version, created_at, created_by_version, last_opened_by_version
);

CREATE TABLE runs (                         -- one row per agent-mux process that opened the DB for writing
  id                TEXT PRIMARY KEY,       -- UUIDv4
  pid               INTEGER,
  agent_mux_version TEXT NOT NULL,
  started_ns        INTEGER NOT NULL,
  heartbeat_ns      INTEGER NOT NULL,       -- bumped on every commit and at least every 30 s
  ended_ns          INTEGER,
  termination       TEXT                    -- 'quit' | 'crash'
);

CREATE TABLE sessions (
  key             TEXT PRIMARY KEY,         -- "{provider}:{session_id}" == correlate::claim_key
  provider        TEXT NOT NULL CHECK (provider IN ('claude','codex','antigravity')),
  session_id      TEXT NOT NULL,            -- the CLI's own conversation id
  user_id         TEXT,
  cwd             TEXT,
  project_slug    TEXT,
  transcript_path TEXT,
  title           TEXT,                     -- full mode: first real user prompt, masked, ≤ 120 chars; set once
  first_seen_ns   INTEGER NOT NULL,
  last_seen_ns    INTEGER NOT NULL,
  extra           TEXT NOT NULL DEFAULT '{}', -- JSON object: Codex session_meta / turn_context facts (cli_version, model_provider, git, model…)
  UNIQUE (provider, session_id)
);
CREATE INDEX sessions_last_seen ON sessions (last_seen_ns DESC);
CREATE INDEX sessions_project   ON sessions (project_slug, last_seen_ns DESC);

CREATE TABLE launches (                     -- replaces the lifecycle trace (session_started / session_ended)
  id                     TEXT PRIMARY KEY,  -- launch_id (UUIDv4), unchanged
  run_id                 TEXT NOT NULL REFERENCES runs (id),
  agent_mux_session      INTEGER NOT NULL,  -- App session ordinal within the run
  profile                TEXT NOT NULL,
  provider               TEXT NOT NULL,
  cwd                    TEXT NOT NULL,
  project_slug           TEXT NOT NULL,
  content_mode           TEXT NOT NULL CHECK (content_mode IN ('metadata','full')),
  correlation_plan       TEXT NOT NULL,     -- 'deterministic' | 'watched' | 'none'
  correlation            TEXT,              -- outcome: 'deterministic' | 'watched' | 'heuristic' | 'none'
  session_key            TEXT REFERENCES sessions (key),
  injected_session_id    INTEGER NOT NULL DEFAULT 0,
  attached               INTEGER NOT NULL DEFAULT 0,   -- started via the `t` toggle on a running session
  started_ns             INTEGER NOT NULL,
  ended_ns               INTEGER,
  termination            TEXT,              -- 'exit' | 'stopped' | 'app_quit' | 'crash' | 'import'
  exit_code              INTEGER,
  parse_errors           INTEGER NOT NULL DEFAULT 0,
  dropped_ops            INTEGER NOT NULL DEFAULT 0,
  reported_cost_usd      REAL,              -- Claude `cost-state` total_cost_usd: the CLI's own figure
  reported_lines_added   INTEGER,
  reported_lines_removed INTEGER,
  agent_mux_version      TEXT NOT NULL,
  user_id                TEXT,
  release                TEXT,
  environment            TEXT,
  tags                   TEXT NOT NULL DEFAULT '[]'   -- JSON array; ["agent-mux", provider] + config tags
);
CREATE INDEX launches_started ON launches (started_ns DESC);
CREATE INDEX launches_session ON launches (session_key);

CREATE TABLE traces (                       -- one row per user turn
  rid                    INTEGER PRIMARY KEY,
  id                     TEXT NOT NULL UNIQUE, -- 32 lowercase hex; UUIDv5("amx1|{provider}|{session_id}|turn|{ordinal}[|{launch_id}]")
  session_key            TEXT NOT NULL REFERENCES sessions (key),
  launch_id              TEXT REFERENCES launches (id),
  ordinal                INTEGER NOT NULL,
  name                   TEXT NOT NULL,     -- "turn N" (metadata) | "{profile}: {first 80 chars}" (full)
  status                 TEXT NOT NULL CHECK (status IN ('open','closed','aborted')),
  start_ns               INTEGER NOT NULL,
  end_ns                 INTEGER,
  input                  TEXT,              -- full mode
  output                 TEXT,              -- full mode: last assistant text
  thinking               TEXT,              -- full mode: turn-level thinking not attached to a generation
  skills                 TEXT NOT NULL DEFAULT '[]', -- JSON array of attributionSkills seen, in order
  reported_duration_ms   INTEGER,           -- Claude `turn_duration`
  reported_message_count INTEGER,
  session_cost_usd       REAL,              -- Claude running total at close
  timing_approx          INTEGER NOT NULL DEFAULT 0,
  ordinal_salted         INTEGER NOT NULL DEFAULT 0, -- backfill truncated: id carries the launch salt
  closed_by              TEXT,              -- NULL | 'recovery'
  metadata               TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX traces_session ON traces (session_key, ordinal);
CREATE INDEX traces_launch  ON traces (launch_id);
CREATE INDEX traces_start   ON traces (start_ns DESC);
CREATE INDEX traces_open    ON traces (status) WHERE status = 'open';

CREATE TABLE observations (
  rid                INTEGER PRIMARY KEY,
  id                 TEXT NOT NULL UNIQUE, -- 16 lowercase hex; span-id derivation unchanged
  trace_id           TEXT NOT NULL REFERENCES traces (id),
  parent_id          TEXT,                 -- NULL = child of the turn; reserved for nesting (subagents)
  type               TEXT NOT NULL CHECK (type IN ('generation','tool','agent','event','span')),
  name               TEXT NOT NULL,        -- display name: "assistant", "Bash", "mcp: server/tool", "agent: role", "skill: name"
  kind               TEXT,                 -- 'mcp_tool' | 'agent_invocation' | 'skill_load' | 'usage_only' | NULL
  start_ns           INTEGER NOT NULL,
  end_ns             INTEGER,              -- NULL while a tool call is in flight
  level              TEXT NOT NULL DEFAULT 'DEFAULT' CHECK (level IN ('DEBUG','DEFAULT','WARNING','ERROR')),
  status_message     TEXT,                 -- 'tool error' | 'no result observed' | 'unpaired result'
  model              TEXT,                 -- provided model name, verbatim (including '<synthetic>')
  model_id           TEXT,                 -- matched models.id; NULL = unpriced (no FK: models can be replaced)
  input              TEXT,                 -- full mode: user text (generation) / args JSON (tool)
  output             TEXT,                 -- full mode: assistant text / tool result
  thinking           TEXT,                 -- full mode
  usage              TEXT,                 -- JSON object: raw integer usage keys exactly as observed
  input_tokens       INTEGER,              -- normalized, billable, DISJOINT buckets (see Usage)
  output_tokens      INTEGER,
  cache_read_tokens  INTEGER,
  cache_write_tokens INTEGER,              -- all TTLs
  cache_write_1h_tokens INTEGER,           -- subset of cache_write_tokens billed at the 1h rate (Claude)
  reasoning_tokens   INTEGER,              -- informational subset of output_tokens (Codex)
  total_tokens       INTEGER,
  input_cost_usd     REAL,
  output_cost_usd    REAL,
  cache_read_cost_usd  REAL,
  cache_write_cost_usd REAL,
  total_cost_usd     REAL,                 -- NULL = unpriced, never 0 by default
  tool_id            TEXT,                 -- tool_use_id / call_id ('' for Antigravity)
  tool_name          TEXT,                 -- raw name before display renaming (e.g. mcp__server__tool, Task)
  skill              TEXT,                 -- attributionSkill of the issuing message
  mcp_server         TEXT,
  path               TEXT,                 -- file/dir argument when present (structure, both modes)
  is_error           INTEGER NOT NULL DEFAULT 0,
  ts_approx          INTEGER NOT NULL DEFAULT 0,
  metadata           TEXT NOT NULL DEFAULT '{}' -- JSON object: everything else (see Data mapping)
);
CREATE INDEX observations_trace ON observations (trace_id, start_ns);
CREATE INDEX observations_start ON observations (start_ns DESC);
CREATE INDEX observations_model ON observations (model) WHERE type = 'generation';
CREATE INDEX observations_tool  ON observations (tool_name) WHERE type IN ('tool','agent');
CREATE INDEX observations_open  ON observations (trace_id) WHERE end_ns IS NULL;

CREATE TABLE models (                       -- price table; seeded from the bundled list, overlaid by config
  id                 TEXT PRIMARY KEY,     -- canonical name, e.g. 'claude-sonnet-4-6'
  provider           TEXT NOT NULL,        -- 'anthropic' | 'openai' | 'google' | 'other'
  match              TEXT NOT NULL,        -- JSON array of lowercase patterns; trailing '*' = prefix match
  input_per_m        REAL NOT NULL,        -- USD per 1M tokens
  output_per_m       REAL NOT NULL,
  cache_read_per_m   REAL,                 -- NULL = 0
  cache_write_per_m  REAL,                 -- NULL = 0 (5-minute TTL rate)
  cache_write_1h_per_m REAL,               -- NULL = same as cache_write_per_m
  reasoning_per_m    REAL,                 -- NULL = reasoning tokens are billed inside output
  source             TEXT NOT NULL CHECK (source IN ('builtin','config','user')),
  updated_at         TEXT NOT NULL         -- ISO date of the price data
);

-- Full-text search over content (full mode). External-content FTS5 kept in
-- sync by triggers; costs ~30 % of the text size.
CREATE VIRTUAL TABLE observations_fts USING fts5 (input, output, content = 'observations', content_rowid = 'rid', tokenize = 'unicode61');
CREATE VIRTUAL TABLE traces_fts       USING fts5 (input, output, content = 'traces',       content_rowid = 'rid', tokenize = 'unicode61');
-- + the standard six AFTER INSERT / AFTER DELETE / AFTER UPDATE OF input, output triggers
```

Views (read-time aggregates; Langfuse never persists these on traces either):

```sql
CREATE VIEW trace_stats AS
SELECT t.*,
       datetime(t.start_ns / 1e9, 'unixepoch') AS started_at,
       (COALESCE(t.end_ns, MAX(COALESCE(o.end_ns, o.start_ns)), t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
       COUNT(o.rid)                                   AS observation_count,
       SUM(o.type = 'generation')                     AS generation_count,
       SUM(o.type IN ('tool','agent'))                AS tool_count,
       SUM(o.is_error)                                AS error_count,
       SUM(o.end_ns IS NULL)                          AS open_count,
       SUM(o.input_tokens)                            AS input_tokens,
       SUM(o.output_tokens)                           AS output_tokens,
       SUM(o.cache_read_tokens)                       AS cache_read_tokens,
       SUM(o.cache_write_tokens)                      AS cache_write_tokens,
       SUM(o.total_tokens)                            AS total_tokens,
       SUM(o.total_cost_usd)                          AS total_cost_usd,   -- NULL when nothing was priced
       SUM(o.type = 'generation' AND o.usage IS NOT NULL AND o.total_cost_usd IS NULL) AS unpriced_generations,
       GROUP_CONCAT(DISTINCT o.model)                 AS models
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;

CREATE VIEW session_stats AS
SELECT s.*,
       datetime(s.last_seen_ns / 1e9, 'unixepoch')    AS last_seen_at,
       COUNT(ts.rid)                                  AS turn_count,
       SUM(ts.status = 'open')                        AS open_turns,
       MIN(ts.start_ns)                               AS first_turn_ns,
       MAX(COALESCE(ts.end_ns, ts.start_ns))          AS last_turn_ns,
       (MAX(COALESCE(ts.end_ns, ts.start_ns)) - MIN(ts.start_ns)) / 1000000 AS duration_ms,
       SUM(ts.observation_count)                      AS observation_count,
       SUM(ts.tool_count)                             AS tool_count,
       SUM(ts.error_count)                            AS error_count,
       SUM(ts.input_tokens)                           AS input_tokens,
       SUM(ts.output_tokens)                          AS output_tokens,
       SUM(ts.cache_read_tokens)                      AS cache_read_tokens,
       SUM(ts.cache_write_tokens)                     AS cache_write_tokens,
       SUM(ts.total_tokens)                           AS total_tokens,
       SUM(ts.total_cost_usd)                         AS total_cost_usd,
       SUM(ts.unpriced_generations)                   AS unpriced_generations,
       (SELECT MAX(reported_cost_usd) FROM launches l WHERE l.session_key = s.key) AS reported_cost_usd
FROM sessions s LEFT JOIN trace_stats ts ON ts.session_key = s.key
GROUP BY s.key;
```

`session_stats.total_cost_usd` (computed from usage × prices) next to `reported_cost_usd` (Claude's own `cost-state`) is the built-in sanity check for the price table.

### Migrations

`store/schema.rs` holds `const MIGRATIONS: &[&str]` (one SQL script per version). Rules: additive only where possible; a migration that rewrites data runs inside the same transaction and is tested against a fixture DB from the previous version; `user_version` is bumped as the last statement. The doctor prints both versions.

### Retention

When `retention_days > 0`, the writer runs once at open, after the recovery sweep: delete observations, then traces, then launches whose `start_ns` is older than the cutoff, then sessions with no remaining traces or launches, then `PRAGMA incremental_vacuum`. `trace prune` is the manual form.

## Data mapping — every current attribute has a home

| Today (`langfuse.*` / `gen_ai.*` attribute) | Now |
|---|---|
| `langfuse.session.id` | `sessions.session_id`; `traces.session_key` |
| `langfuse.user.id` | `launches.user_id`, `sessions.user_id` |
| `langfuse.trace.metadata.{agent_mux_session, launch_id, profile, cwd, project_slug, provider, agent_mux_version, correlation}` | `launches.*` (per launch, not repeated per row) |
| `langfuse.trace.metadata.transcript_path` | `sessions.transcript_path` |
| session extras (Codex `session_meta` keys) | `sessions.extra` JSON |
| `langfuse.trace.name` | `traces.name` |
| `langfuse.trace.tags`, `langfuse.release`, `langfuse.environment` | `launches.tags/release/environment` |
| `langfuse.trace.input` / `.output` / `.metadata.thinking` | `traces.input/output/thinking` |
| `langfuse.trace.metadata.skills` (comma-joined) | `traces.skills` JSON array |
| `…turn_duration_ms`, `…turn_message_count`, `…session_cost_usd` | `traces.reported_duration_ms`, `.reported_message_count`, `.session_cost_usd` |
| `…timing = "approximate"` | `traces.timing_approx = 1` |
| `…terminated = "aborted"` | `traces.status = 'aborted'` |
| `gen_ai.usage.*` on the root span (no generation took it) | a `generation` row, `kind = 'usage_only'`, name `assistant (usage only)` |
| `langfuse.observation.type = generation` | `observations.type = 'generation'`, name `assistant` / `assistant (tool use)` |
| `gen_ai.request.model`; `langfuse.observation.metadata.model` for `<synthetic>` | `observations.model` verbatim in both cases; the matcher never prices `<synthetic>` so `model_id` stays NULL |
| `gen_ai.usage.<key>` | `observations.usage` JSON + normalized token columns + cost columns (new) |
| `langfuse.observation.input/output`, `.metadata.thinking` | `observations.input/output/thinking` |
| `.metadata.skill` | `observations.skill` |
| `.metadata.tool_calls` (comma-joined) | `metadata.tool_calls` JSON array |
| `.metadata.ts = "approx"` | `observations.ts_approx = 1` |
| `langfuse.observation.type = tool|agent`, span name | `observations.type`, `.name` (display), `.tool_name` (raw), `.tool_id` |
| `.metadata.kind` | `observations.kind` |
| `.metadata.mcp_server`, `.metadata.mcp_tool` | `observations.mcp_server`; `metadata.mcp_tool` |
| `.metadata.agent_role/agent_type/agent_model/agent_prompt` | `metadata.agent_role/…` (prompt full mode only) |
| `.metadata.skill_name/skill_path` | `metadata.skill_name/skill_path` |
| `.metadata.summary/action/command/query` (full mode, masked) | `metadata.summary/action/command/query` |
| `.metadata.path` | `observations.path` |
| structured-result facts: `stdout_bytes, stderr_bytes, interrupted, return_code, lines_added, lines_removed, user_modified, agent_id, agent_model, agent_status, agent_async, agent_output_file, workflow_name, workflow_run_id, workflow_status` | `metadata.<same key>` |
| OTLP status 2 + `"tool error"` | `level = 'ERROR'`, `is_error = 1`, `status_message = 'tool error'` |
| `statusMessage = "no result observed"` (unpaired at close) / `"unpaired result"` (orphan) | `status_message`, `end_ns` = turn end / result time |
| `session_started` event span | `launches` row at start |
| `session_ended` event span: `termination, exit_code, parse_errors, dropped_spans, total_cost_usd, total_lines_added/removed` | `launches.termination/exit_code/parse_errors/dropped_ops/reported_cost_usd/reported_lines_*` |

Content-mode rules are unchanged: in `metadata` mode every column marked "full mode" is NULL and `sessions.title` is NULL; the metadata-mode test that asserts the absence of every content attribute becomes an assertion that those columns are NULL across all rows.

The `metadata` JSON columns are for the long tail. Anything that becomes a filter in practice gets promoted to a column in a later schema version (SQLite generated columns over `json_extract` are the cheap intermediate step).

## Usage normalization and cost

Langfuse's generic OTel path assumes OpenAI semantics: `input = max(input_tokens − cache_read − cache_creation, 0)`. Anthropic's `usage.input_tokens` already **excludes** cache tokens, so today's Claude generations lose their uncached input in Langfuse (usually a small share of a heavily cached Claude Code turn, but wrong, and often clamped to zero). The local store normalizes per provider, in `tracing/usage.rs`, into **disjoint billable buckets** whose sum is `total_tokens`:

| Provider / source | input_tokens | cache_read | cache_write (5m / 1h) | output_tokens | reasoning (subset of output) | total |
|---|---|---|---|---|---|---|
| Claude `message.usage` | `input_tokens` (already uncached) | `cache_read_input_tokens` | `cache_creation_input_tokens`; split from `cache_creation.ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens` when present | `output_tokens` | not reported | sum of the four buckets |
| Codex `token_count.last_token_usage` | `input_tokens − cached_input_tokens` (floored at 0) | `cached_input_tokens` | `cache_write_input_tokens` when present | `output_tokens` (includes reasoning) | `reasoning_output_tokens` | `total_tokens` as reported, else the sum |
| Antigravity `conversations/<id>.db` → `gen_metadata` (protobuf, per request; see below) | `prompt_tokens` (uncached) | `context_tokens − prompt_tokens` (derived, flagged `cache_read_derived`) | — | `output_tokens` (thoughts + text) | `thoughts_tokens` | `context_tokens + output_tokens` |

**Antigravity side channel.** agy's transcript carries no usage, but agy keeps one protobuf record per model request in `<antigravity-cli root>/conversations/<conversation-id>.db`, table `gen_metadata` (verified on agy 1.1.25; no public schema). `tracing/agy_usage.rs` decodes the fields that matter — the transcript `step_index` values the request produced, prompt tokens, output tokens split into thoughts and text, the context size and window, latency, time to first token, and the real model id (`gemini-3.8-flash` where the transcript only says "Gemini 3") — with a raw protobuf walker, no schema dependency. The pipeline polls that database read-only on every tick after adoption (skipping existing records on a resume) and `trace import` reads it whole; the assembler attaches each record to the generation it emitted for that step by upserting the same observation id, so usage arriving before or after the transcript line both work. The last record of a conversation embeds the entire context, so the decoder only walks the paths it needs. Everything fails open: a missing, locked, or undecodable record yields no usage. The field map is pinned by a fixture built from real records (`tests/fixtures/agy_gen_metadata.hex`).

The raw keys are kept verbatim in `observations.usage` so a future correction can re-normalize without re-parsing transcripts (`trace recost` does exactly this). The Claude parser already flattens nested usage objects into dotted integer keys (`cache_creation.ephemeral_5m_input_tokens`, `cache_creation.ephemeral_1h_input_tokens`, `server_tool_use.web_search_requests`); `usage.rs` reads the cache buckets from those and excludes non-token counters such as `server_tool_use.*` from `total_tokens` while leaving them in the `usage` JSON.

### Model matching (no regex crate)

`normalize(name)`: lowercase → strip provider prefixes (`anthropic/`, `openai/`, `models/`, Bedrock `us.`/`eu.`/`apac.`/`global.` + `anthropic.`) → strip a trailing `[1m]` → strip a Bedrock `-v1:0` / `-v1` suffix → strip a date suffix (`-yyyymmdd`, `@yyyymmdd`, `-yyyy-mm-dd`). Then, over the `models` table loaded into memory at open: exact match against any pattern in `match`; else the longest matching prefix pattern (`claude-sonnet-4-5*`). Ties are impossible by construction (ids are unique, patterns validated non-overlapping at load; overlap is a doctor warning). `<synthetic>` and empty names never match.

### Cost

```
input_cost       = input_tokens        × input_per_m       / 1e6
cache_read_cost  = cache_read_tokens   × cache_read_per_m  / 1e6
cache_write_cost = (cache_write_tokens − cache_write_1h_tokens) × cache_write_per_m / 1e6
                 + cache_write_1h_tokens × COALESCE(cache_write_1h_per_m, cache_write_per_m) / 1e6
output_cost      = reasoning_per_m IS NULL
                   ? output_tokens × output_per_m / 1e6
                   : (output_tokens − reasoning_tokens) × output_per_m / 1e6 + reasoning_tokens × reasoning_per_m / 1e6
total_cost       = sum of the four
```

Cost is computed once, at write time, against the price table in effect — a snapshot, which is the correct semantics when prices change. `trace recost` recomputes every observation from `usage` + the current table (idempotent, reports how many rows changed). Provided costs never exist in our sources, so there is no "provided cost is authoritative" branch; Claude's `cost-state` is stored separately as `reported_cost_usd` for comparison.

Not modeled in v1 (documented, doctor-visible): Anthropic's 1M-context tier above 200 k input tokens, Opus fast-mode pricing (`speed` is not in transcripts), batch discounts. These are per-request price *tiers*; Langfuse handles them with priority-ordered tier conditions. If a `[[tracing.models]]` row cannot express a case, the user can still set `reported_cost_usd`-style truth via the CLI's own figures.

### Bundled price table

`src/tracing/pricing.toml`, `include_str!`ed, inserted with `source = 'builtin'` on DB creation and re-synced at every open (builtin rows are replaced when the bundled `updated_at` is newer; `config` rows come from `[[tracing.models]]` and override builtin rows by id; `user` rows are anything someone inserts by hand and are never touched). USD per 1M tokens. **Every value must be re-verified against the providers' pricing pages when the file is first committed; the table is data, not code, and carries its own date.** Seed, as of 2026-06 (Anthropic API reference snapshot; Langfuse `default-model-prices.json` at v4.24.0 for cross-checking and for the cache-write ratios of 1.25× / 2× and cache-read ratio of 0.1×):

| id | match | input | output | cache read | cache write 5m | cache write 1h |
|---|---|---|---|---|---|---|
| `claude-fable-5-1` | `claude-fable-5-1`, `claude-mythos-5-1` | 10.00 | 50.00 | 0.25 | 12.50 | 20.00 |
| `claude-fable-5` | `claude-fable-5`, `claude-mythos-5` | 10.00 | 50.00 | 1.00 | 12.50 | 20.00 |
| `claude-opus-5` | `claude-opus-5` | 5.00 | 25.00 | 0.50 | 6.25 | 10.00 |
| `claude-opus-4-8` / `-4-7` / `-4-6` | same | 5.00 | 25.00 | 0.50 | 6.25 | 10.00 |
| `claude-sonnet-5` | `claude-sonnet-5` | 2.00 | 10.00 | 0.20 | 2.50 | 4.00 |
| `claude-sonnet-4-6` | `claude-sonnet-4-6` | 3.00 | 15.00 | 0.30 | 3.75 | 6.00 |
| `claude-sonnet-4-5` | `claude-sonnet-4-5*` | 3.00 | 15.00 | 0.30 | 3.75 | 6.00 |
| `claude-opus-4-1` | `claude-opus-4-1*` | 15.00 | 75.00 | 1.50 | 18.75 | 30.00 |
| `claude-haiku-4-5` | `claude-haiku-4-5*` | 1.00 | 5.00 | 0.10 | 1.25 | 2.00 |
| `gpt-5.3-codex` | `gpt-5.3-codex` | 1.75 | 14.00 | 0.175 | — | — |
| `gpt-5` | `gpt-5`, `gpt-5-codex` | 1.25 | 10.00 | 0.125 | — | — |

The doctor lists every distinct `observations.model` with `model_id IS NULL` (excluding `<synthetic>`), with the count of generations affected, so a new model shows up as "unpriced: claude-x (412 generations)" rather than as a quietly cheaper month.

## Failure posture

**Invariant, unchanged: tracing can never break, block, or slow a session.** Structural guarantees are the same as today (file reads only, bounded `try_send` channels, all I/O on the writer thread, errors swallowed into at most one status-bar line, bounded shutdown).

| Failure | Behavior |
|---|---|
| DB path unwritable / parent dir uncreatable | Runtime disabled for the run; TUI starts untraced; one notice; `trace doctor` explains. |
| File is not a database / corrupt header | Same. The user's file is **never** renamed, deleted, or recreated automatically; the doctor prints the `quick_check` result and the recovery command (`sqlite3 traces.db ".recover"`). |
| `user_version` newer than the binary | Disabled + notice naming both versions. |
| Disk full / I/O error during a commit | Batch dropped and counted; 5 consecutive ⇒ breaker (60 s, one probe). |
| Write-lock contention (second agent-mux, a GUI mid-write) | `busy_timeout` 5 s, then 3 retries, then drop. Readers never block the writer under WAL. |
| Queue full | Drop + count + once-per-run notice (unchanged). |
| Transcript never appears / schema drift / torn tail | Unchanged: `launches` row with `correlation = 'none'`; lenient parsing; `parse_errors` counter. |
| App crash / panic | Every committed batch is durable. The open turn is closed by the next startup's recovery sweep, flagged `closed_by = 'recovery'`. |
| Quit with queued ops | Bounded flush (default 1 s, `0` = skip); local commits make the deadline a formality. |
| DB on `/mnt/*` (WSL 9p) | Works, but a notice recommends a Linux-filesystem path. |
| Second process closes my live turns | Prevented by the `runs.heartbeat_ns` guard (120 s staleness). |

## `agent-mux trace …` CLI

One-shot subcommands dispatched in `main` before terminal setup, exactly like `langfuse doctor` today; hand-rolled argument parsing in `tracing/cli.rs` (no `clap`, consistent with the repo). Every listing command accepts `--json`; human output is a fixed-width table. All reads open the DB `?mode=ro` (`OpenFlags::SQLITE_OPEN_READ_ONLY`) so a CLI can never take the writer's lock.

| Command | Purpose |
|---|---|
| `trace doctor` | Resolved config (file, `db_path`, content mode); DB exists / size / `journal_mode` / `user_version` vs binary / `PRAGMA quick_check`; row counts; open turns and stale runs; **unpriced models**; overlapping `match` patterns; FTS5 present (`PRAGMA compile_options`); `/mnt/*` warning; per-provider correlation readiness (unchanged from today's doctor). |
| `trace path` | Prints the resolved DB path — `sqlite3 "$(agent-mux trace path)"`. |
| `trace ls [--all] [--project <dir>] [--since 7d] [--limit 50]` | Sessions from `session_stats`: provider, title, turns, tokens, cost (computed / reported), last seen. Defaults to the current project, like the history viewer. |
| `trace show <session-id \| trace-id> [--full]` | For a session: its turns (ordinal, name, status, latency, tokens, cost, tools, errors). For a trace: the observation list in time order with model/usage/cost/level; `--full` prints input/output bodies. Prefix match on ids. |
| `trace search "<fts5 query>" [--limit 20]` | FTS5 over `traces_fts` and `observations_fts`; prints the trace, the observation name, and a snippet. Full-mode data only. |
| `trace import <path>… \| --discover [--provider …] [--content-mode …]` | Backfill transcripts through the same assembler (emitting from byte 0). `--discover` walks `history::discover_sessions` for Claude and Antigravity plus `<codex_dir>/sessions/**/rollout-*.jsonl`. Provider from `transcript::detect_provider`, id from filename / `session_meta` / brain dir. The launch row is synthetic: `id = UUIDv5("amx1|import|{abs path}")`, `termination = 'import'`, so re-importing the same file converges. Refuses files currently claimed by a live pipeline. |
| `trace export [--session <id>] [--since 30d] [--out file.jsonl]` | JSON Lines dump — one object per row with a `table` field — a portable backup and the input for any future Langfuse re-export tool. |
| `trace prune [--older-than 90d] [--vacuum] [--dry-run]` | Retention by hand; `--vacuum` also checkpoints the WAL and runs `VACUUM`. |
| `trace recost` | Recompute cost columns from `usage` and the current price table; reports rows changed and models still unpriced. |
| `trace sql "<select …>"` | Read-only passthrough for ad-hoc questions without installing `sqlite3`; rejects anything that is not a single `SELECT`/`WITH` statement. |

`agent-mux langfuse doctor` prints one line pointing at `trace doctor` and exits 0 for one release, then is removed.

## TUI

### Phase 1 (ships with the store)

- Every user-facing "Langfuse" string becomes "tracing": the help row `t  toggle tracing`, the launch dialog label `Tracing: [●] Enabled` and `Content: full | metadata`, the `cannot trace` / `tracing is not configured` notices. `[● TRACE]` badges are unchanged.
- The `t` toggle and the dialog defaults follow the resolved config (on / `full`).
- Notices listed under UI feedback.

### Phase 2 — trace browser (`T` in Control mode)

`Mode::TraceBrowser(TraceBrowserState)`, a three-pane modal in the style of the session history viewer, reading the DB through a read-only connection owned by the state (queries are indexed, `LIMIT`ed to 500 rows, and take milliseconds, so they run synchronously on the main thread like the history viewer's file reads):

| Pane | Content | Source |
|---|---|---|
| Sessions (left) | provider badge, title / session id, turns, tokens, cost, last seen; live sessions marked `●` | `session_stats`, current project by default, `a` toggles all |
| Turns (middle) | ordinal, name, status (`open` turns pulse), latency, tokens, cost, tool count, error count | `trace_stats WHERE session_key = ?` |
| Detail (right) | the selected turn's observations as a timeline — type glyph, name, duration, model, tokens, cost, level; `Enter` expands one observation to scroll its input / output / thinking / metadata | `observations WHERE trace_id = ? ORDER BY start_ns` |

Keys mirror the history viewer: `Tab`/`BackTab` cycle panes, `j`/`k` `↑`/`↓` move, `Enter` drills in, `Esc`/`q` backs out then closes, `PgUp`/`PgDn`/`Home`/`End` and the wheel scroll, `r` resumes the selected session (builds a `SessionSummary` from the `sessions` row and calls the existing `resume_history_session`), `/` opens the FTS search prompt (full mode). The browser refreshes its queries on `Tick` while a live session is selected (1 Hz), which is how in-progress turns and running tools appear without any push machinery.

**Live badges.** The writer publishes `AppEvent::TraceStats { session: usize, turns: u64, total_tokens: i64, cost_usd: Option<f64>, running_tool: Option<String> }` after any commit that touched a launch, throttled to one per launch per second, computed from the same views. `App` stores the latest per session; the main-pane title shows `[● TRACE t12 $0.42 ▸ Bash]`, the session list `[● 12t $0.42]`. Metadata mode shows tokens and cost without content, as today.

**Future (not designed here):** the history viewer (`l`) could use the DB as its index — `session_stats` is far cheaper than re-scanning every JSONL on open — and the two viewers could merge. Both keep working independently in this design.

## Dependencies

| Change | Crate | Why |
|---|---|---|
| **add** | `rusqlite = { version = "0.40", features = ["bundled"] }` | Embedded SQLite compiled into the binary (no system library, works on Windows/WSL2 alike). `bundled` compiles SQLite with `SQLITE_ENABLE_FTS5` and `SQLITE_ENABLE_JSON1` (verified in libsqlite3-sys 0.38.2's build script); the doctor still prints `compile_options` for non-bundled builds. Adds `libsqlite3-sys` + a C compile (~30 s cold, ~1.5 MB binary). |
| **remove** | `ureq` | No HTTP. Drops `rustls`, `ring`, `webpki`, `rustls-pki-types`, `rustls-webpki` and the rest of the TLS tree — a net shrink. |
| keep | `uuid` (v4 + v5), `time` (RFC3339), `serde_json`, `tokio`, `tempfile` (dev) | unchanged roles. |

Explicitly rejected: `sqlx` (async pool, compile-time query checking, large — a single-writer thread needs none of it); `sea-orm` / `diesel` (ORMs for four tables); `regex` (still unnecessary: model matching is exact + prefix); `notify` (unchanged reasoning); `clap` (repo has none).

## Implementation plan

### Files

| File | Change |
|---|---|
| `Cargo.toml` | add `rusqlite`; remove `ureq`. |
| `src/config.rs` | `TracingConfig` (+ deprecated network keys), `ModelPriceConfig`, `ProfileTracing`, `ResolvedTracing`, `resolve_tracing` with enabled-by-default and `db_path` resolution; serde aliases; gate condition. |
| `src/tracing/mod.rs` (was `src/langfuse/mod.rs`) | `TraceRuntime` (was `LangfuseRuntime`): construction opens the store on the writer thread and may come back disabled; `start_session` sends `StoreOp::Launch`; pipeline sends `StoreOp`s; `finalize` sends the launch update; `shutdown` unchanged in shape. |
| `src/tracing/correlate.rs`, `src/tracing/tail.rs` | moved, unchanged. |
| `src/tracing/ids.rs` | `AMX_NS`, `trace_id_for`, `span_id_for`, `hex` lifted from `otlp.rs`; stability vectors unchanged. |
| `src/tracing/map.rs` | assembler emits `StoreOp`s: `open_turn` → `Trace(open)`, `gen_span` → `Observation`, `tool_span` → `Observation`, `close_turn` → `Trace(closed)`; `usage_only` generation rule; `session_title`; `SessionEnd` → `LaunchRow`. Every classification, dedup, masking, ordinal and id rule is untouched. |
| `src/tracing/usage.rs` (new) | per-provider normalization → `NormalizedUsage`. |
| `src/tracing/pricing.rs` + `pricing.toml` (new) | `ModelPrice`, load/merge (builtin/config), `normalize_model_name`, `match_model`, `cost_for`. |
| `src/tracing/store/mod.rs`, `schema.rs`, `writer.rs`, `query.rs`, `model.rs` (new) | open + pragmas + migrations + seed + recovery + retention; writer thread with breaker; read queries used by CLI and TUI; row structs and upsert statements. |
| `src/tracing/cli.rs` (replaces `doctor.rs`) | all `trace` subcommands. |
| `src/transcript.rs` | No change: `integer_usage` already flattens nested Claude usage objects into dotted keys, which `usage.rs` consumes. |
| `src/main.rs` | dispatch `trace …` (and the `langfuse doctor` stub); construct `TraceRuntime`; shutdown deadline default 1 s. |
| `src/events.rs` | `LangfuseStatus` → `TraceStatus`; phase 2: `TraceStats`. |
| `src/app.rs`, `src/session.rs`, `src/ui.rs` | renames and strings; `spawn_traced`/`toggle_selected_tracing` unchanged in logic; phase 2: `Mode::TraceBrowser`, badges. |
| `src/lib.rs` | `pub mod tracing` replaces `pub mod langfuse`. |
| `src/langfuse/otlp.rs`, `export.rs`, `doctor.rs` | deleted. |
| `tests/langfuse_*.rs` | renamed `tests/trace_*.rs`; `langfuse_export.rs` → `trace_store.rs` (tempdir DB replaces the stub HTTP server); `langfuse_session.rs` → `trace_session.rs` asserting rows instead of request bodies. |
| `profiles.example.toml` | `[tracing]` documentation as above; Langfuse paragraph replaced by the migration note. |

### Order of work — each stage lands green and is independently abandonable

1. **Config**: structs, resolver, aliases, deprecation flag, gate; tests. Nothing consumes it yet.
2. **`ids.rs` + `store/`** (schema, open, migrations, recovery sweep, retention, writer thread, breaker) with tempdir tests. Pure; no pipeline changes.
3. **`usage.rs` + `pricing.rs` + `pricing.toml`**: normalization vectors per provider, name normalization, matching, cost vectors, builtin/config merge.
4. **`map.rs` → `StoreOp`**: mechanical conversion of emission and tests (attribute assertions become field assertions, one for one).
5. **Runtime wiring** (`mod.rs`): launch/session ops, pipeline, finalize, shutdown.
6. **CLI**: doctor, path, ls, show, import, export, prune, recost, search, sql.
7. **App/UI/main/events** renames; delete `otlp.rs`/`export.rs`/`doctor.rs`; drop `ureq`; e2e test asserts rows after `kill_all` + shutdown.
8. **Phase 2**: `TraceStats` events and badges; trace browser.

Rough scale: ~2.5 k lines including tests for phases 1–7 (roughly the size of the code it replaces plus the CLI); ~1 k for phase 2.

### Tests (repo conventions: inline `#[cfg(test)]` for pure logic, `tests/*.rs` + `tempfile` for integration)

- **config**: absent section ⇒ enabled with defaults; `enabled = false` ⇒ `None`; `[langfuse]` alias loads and sets `legacy_langfuse_keys`; `db_path` precedence (config → env → home) via the injected env seam; `[[tracing.models]]` parse + validation errors (negative price, empty match) are reported, not fatal.
- **store/schema**: fresh file ⇒ `user_version == SCHEMA_VERSION`, pragmas as specified, mode `0600`; opening a v(N−1) fixture upgrades; a v(N+1) fixture refuses; upsert idempotency (same row twice ⇒ one row; less-informative second write erases nothing; `open → closed` transitions); FTS triggers keep `observations_fts` in sync; views return the documented columns.
- **writer** (`tests/trace_store.rs`): batches commit on interval and on the 512 cap; `finish(deadline)` respects a hanging lock (a second connection holding `BEGIN IMMEDIATE`); breaker opens after 5 failed batches against a read-only file and half-opens; queue-full drops are counted; recovery sweep closes only stale-heartbeat runs; retention deletes cascade in order; concurrent readers see committed rows under WAL.
- **usage**: Claude with cache buckets (input stays uncached; 5m/1h split); Codex inclusive input/cached and output/reasoning; missing keys; negative/absent totals; Antigravity NULL.
- **pricing**: name normalization vectors (`anthropic/claude-sonnet-4-5-20250929` → `claude-sonnet-4-5`; `us.anthropic.claude-opus-4-1-20250805-v1:0` → `claude-opus-4-1`; `claude-opus-4-6[1m]` → `claude-opus-4-6`); exact beats prefix; `<synthetic>` never matches; cost formula vectors incl. 1h cache writes and separately priced reasoning; config overrides builtin; `recost` idempotent.
- **map**: every existing assembler test converted to rows: turn shape (open trace → generation → tool open → tool closed → closed trace); metadata mode ⇒ content columns NULL and `sessions.title` NULL; Codex boundaries and usage-only generation; Antigravity positional pairing; prime pass emits nothing; salted ids; masking and truncation; MCP/agent/skill classification; structured results.
- **tail / correlate**: unchanged files, renamed.
- **cli**: `ls`/`show`/`search`/`sql` against a fixture DB; `import` of a Claude and a Codex fixture converges on a second run; `--discover` finds tempdir trees; `prune --dry-run`.
- **e2e** (`tests/trace_session.rs`, unix): the existing fake-`claude` script pipeline, asserting after `kill_all` + `shutdown` that the DB holds one launch (`termination = 'exit'`), one session, the turn traces with `status = 'closed'`, the generation and tool observations with costs, and that the `t` toggle attach/stop path writes `attached = 1` / `termination = 'stopped'`.

### Pre-release checks

One manual run per provider on a real install: open turn visible in `trace show` while the agent works; costs within a few percent of Claude's own `cost-state` (`session_stats.total_cost_usd` vs `reported_cost_usd`); `trace import --discover` over a real `~/.claude/projects` completes and is idempotent; a `sqlite3` session open in another terminal never stalls the TUI.

## Migration notes for existing users

- Rename `[langfuse]` → `[tracing]` and `[profiles.langfuse]` → `[profiles.tracing]`; delete `host`, `public_key`, `secret_key`. Until then the alias keeps the section working and prints one notice per run.
- `LANGFUSE_*` environment variables are ignored.
- Tracing is now on by default with `content_mode = "full"`; set `enabled = false` or `content_mode = "metadata"` to get the previous posture.
- History that only exists in Langfuse Cloud is not migrated. Local history is: `agent-mux trace import --discover` backfills every transcript the CLIs still have on disk.
- `agent-mux langfuse doctor` → `agent-mux trace doctor`.

## Langfuse feature parity

| Langfuse | Local equivalent |
|---|---|
| Sessions list with cost / tokens / duration | `trace ls`, `session_stats`, browser sessions pane |
| Trace list, trace detail tree | `trace show`, `trace_stats`, browser turns + detail panes |
| Observation input/output/metadata | columns + `metadata` JSON; `trace show --full` |
| Model-based cost inference | `pricing.rs` + bundled table + `[[tracing.models]]`; plus Claude's own `reported_cost_usd` for cross-checking |
| Filtering by tag / user / environment / metadata | SQL (`trace sql`, any GUI); browser: project + FTS |
| Full-text search | FTS5 (`trace search`, `/` in the browser) |
| Real-time visibility | open turns and running tools are rows; live badges |
| Scores, evals, annotations, prompt management, datasets, dashboards, sharing, multi-user | **not replaced** (out of scope; the schema does not preclude a `scores` table later) |

## Out of scope

- Re-exporting the store to Langfuse (or any OTLP backend). The deterministic ids and complete rows make it a standalone command later; nothing in the live path depends on it.
- Scores/annotations, evals, prompt management, datasets.
- Encryption at rest (`bundled-sqlcipher`) — the file has the same exposure as the CLIs' own transcripts; a `db_key` option is a possible follow-up.
- Pricing tiers by request size / speed; batch discounts.
- Cross-process claim sharing for Codex/Antigravity adoption (pre-existing limitation).
- Merging the history viewer with the trace browser; tracing sessions started outside agent-mux (covered partially by `trace import`).

## Risks

1. **Transcript schema drift** — unchanged, highest likelihood; the shared parser and `parse_errors` counters remain the mitigation, and `trace import` now makes re-processing cheap once a fix lands.
2. **Stale price table.** Bundled prices age; new models arrive unpriced. Mitigated by the doctor's unpriced list, `[[tracing.models]]`, `trace recost`, and the `reported_cost_usd` cross-check for Claude.
3. **Database growth** in full mode with heavy tool output. Mitigated by `content_max_bytes`, `retention_days`, `trace prune`, and size reporting.
4. **SQLite on network/9p filesystems** — documented, detected, defaulted away from.
5. **Two agent-mux processes** writing one file: correct under WAL; duplicate adoption converges by id; recovery sweep guarded by heartbeats. Residual: a machine sleeping > 120 s makes a live run look crashed and its open turn gets closed by a second process — the next real commit reopens nothing but the data is intact and flagged `closed_by = 'recovery'`.
6. **Assembler conversion scope** — `map.rs` is 2.7 k lines with the most valuable tests; the conversion is mechanical but wide. Mitigated by doing it as one stage with one-for-one test translation and no semantic changes.
7. **Full mode by default** stores prompts and outputs on disk. The transcripts already sit next to it under the same user with the same masking gaps; `metadata` remains one line away.

## Decisions to confirm

- **D1 — enabled by default with no config.** Rationale: no keys, no network, no cost; the only side effect is a file under `~/.agent-mux`. Alternative: keep opt-in (`enabled = true` required) — one line in the resolver.
- **D2 — `content_mode = "full"` by default.** Rationale: the value of a local store is having the content; the data already exists in plaintext in the CLIs' own directories; masking, `redact_literals`, and per-profile `metadata` still apply. Alternative: keep `metadata` — same one-line change.
- **D3 — `content_max_bytes` default 64 KiB** (was 16 KiB, sized for an ingestion API). Alternative: keep 16 KiB.
- **D4 — trace browser bound to `T`** (Shift+t) as a separate mode, rather than folded into the `l` history viewer. Alternative: a tab inside the history modal.
