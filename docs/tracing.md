# How tracing works

`agent-mux` traces the agent sessions it launches for four provider
implementations:

| Provider | Profile command | Primary transcript source |
| --- | --- | --- |
| Claude Code | `claude` | `~/.claude/projects/<project>/<session>.jsonl` |
| Codex | `codex` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` |
| Antigravity | `agy` | `~/.gemini/antigravity-cli/brain/<conversation>/.system_generated/logs/transcript_full.jsonl` (falling back to `transcript.jsonl`) |
| OpenCode | `opencode` | `~/.local/share/opencode/opencode.db` (`[tracing] opencode_db`): the session's rows in `session_message`, read in place |

Tracing is enabled by default. It is deliberately *fail-open*: a missing
transcript, unavailable database, malformed line, full queue, or failed
remote exporter must not block, slow, or prevent the agent session itself.

## Shared lifecycle

At application startup, `TraceRuntime` resolves `[tracing]` configuration and
opens the local SQLite trace store (by default
`~/.agent-mux/traces.db`). A profile launch is recognized as traceable from
its command name (`claude`, `codex`, `agy`, or `opencode`), or from
`[profiles.tracing].provider` for wrapper commands.

For each traceable launch, agent-mux:

1. Creates a launch id and chooses a provider-specific way to identify the
   CLI's native session/transcript.
2. Starts the CLI with any safe tracing arguments and environment markers.
3. Starts one asynchronous pipeline for the session.
4. Correlates the launch with exactly one transcript, using a process-wide
   claim registry so concurrent panes cannot adopt the same session.
5. Tails only completed JSONL lines, parses provider events, and assembles
   user turns, model generations, tool calls, subagents, token usage, errors,
   and timings into typed `StoreOp` rows.
6. Sends rows through bounded, non-blocking queues to the selected sink:
   local SQLite, Langfuse, or both. Launch rows are always retained locally.

The SQLite writer owns a single connection and performs batched, idempotent
upserts. This means open turns and tools can be updated when their closing
events arrive, and duplicate source events converge rather than creating
duplicates. On shutdown, the pipeline consumes a final partial line, polls
usage and hooks once more, closes pending rows, and performs a bounded flush.

Resumed sessions are treated specially: existing transcript content is
"primed" into the assembler without being emitted again, then tracing begins
at the current end of the file.

## Claude Code

Claude has the strongest correlation path. For a normal new launch,
agent-mux generates a UUID and appends:

```text
--session-id <uuid>
```

It can therefore look directly for the matching JSONL transcript in the
current project's Claude directory. If Claude's project-slug convention has
changed, it falls back to locating the UUID filename under any project
directory.

For `--resume`, `-r`, or an explicit `--session-id`, the supplied id is used
instead and the existing session is primed. Launch forms that cannot be
correlated safely, such as bare continue/print forms, are left untraced rather
than guessed.

With hooks enabled (the default `hooks = "auto"`), agent-mux passes an inline
`--settings` document. It registers Claude lifecycle hooks for session start
and end, prompt submission, tool start/result/failure, subagent start/stop,
stop/failure, compaction, and model switching. User hook groups found in the
usual Claude settings files are merged into this inline settings document;
nothing in `~/.claude` is modified. Hook announcements take precedence over
watch-based correlation and provide precise tool/subagent timing.

A session that forks or is continued ends its transcript with a
`continued-in` line naming a successor id, and every later message is
written to the successor's file instead. agent-mux follows the hand-off:
it closes the turn that was open, notes the successor on that turn and on
the session row, re-points the claim, the hook feed and the launch's
session key, and primes the successor's transcript so the conversation it
inherits is replayed for state without being recorded twice. Chains are
followed hop by hop, and a successor already visited is not re-entered.
Without this the launch would stay live against a file that had stopped
growing, and none of the successor's work would be recorded.

Claude transcript usage is read from assistant-message usage records. Its
uncached input, cache reads, cache writes (including 5-minute and 1-hour
breakdowns), and output are kept as separate billable buckets.

The tracing adapter merges assistant fragments sharing `message.id` into
one generation, including thinking-only and tool-only messages. Tools keep
their issuing generation as parent even if their result arrives after another
generation. A user row's `uuid` supplies the turn identity when available;
ordinals remain display positions. The history viewer retains its separate
per-block rendering.

Subagent sidecars are discovered by `toolUseId` and tailed independently of
the parent transcript. Their own turns, generations and tools appear below
the launching call, recursively up to eight child levels. Nested classic
agents share the parent's sidecar directory. Workflow children are read from
`subagents/workflows/<runId>/`, with per-agent completion/results from the
workflow journal. Delivered task notifications can complete background
agents without moving their work into a later user turn.

If an injected launch flag causes two immediate nonzero exits for the same
profile, agent-mux disables session-id and hook argument injection for that
profile for the rest of the run and reports the condition in the status bar.
Set `inject_session_id = false` or `hooks = "off"` in the profile when needed.

### Hook profiles

`hooks_profile` in a profile's `[profiles.tracing]` section chooses how much
of the lifecycle a Claude Code launch registers:

| Profile | Events registered | Use |
| --- | --- | --- |
| `off` | none | the same as `hooks = "off"` |
| `minimal` | `SessionStart`, `Stop`, `StopFailure`, `SessionEnd` | session rows and timings without per-tool announcements |
| `standard` | every event (the default) | full tracing |
| `strict` | every event, plus the write guard below with the default denylist | sessions that must not touch protected files |

The profile filters only the inline `--settings` document of a Claude Code
launch. Codex and Antigravity read the hook set installed once by
`agent-mux trace hooks install`, so they keep that set whatever the profile
says. A loop run needs the tool events for its guard, so `minimal` is
raised to `standard` for loop launches. An unknown word reads as
`standard`.

### Write guard

`write_denylist` lists globs, relative to the launch directory, that no
write tool may touch in that profile's sessions:

```toml
[profiles.tracing]
write_denylist = [".claude/**", "secrets/**", "**/.env"]
```

The list is recorded on the launch row as `metadata.write_policy` and
enforced by the same synchronous `PreToolUse` hook as the budget guard:
a `Write`, `Edit` or other write tool aimed at a listed path is refused
with `agent-mux guard: <path> is protected (<glob>)`, and the model sees
the reason. Reads, shell commands and paths off the list pass. Under
`hooks_profile = "strict"` an empty list means the default denylist the
loop gate uses (`gate.yaml` defaults: `.env` files, secrets, auth,
payments, migrations, keys, credentials, CI workflows, infra). The guard is enforced where the CLI's
`PreToolUse` can wait for an answer: Claude Code per launch, Codex after
`agent-mux trace hooks install codex`, not Antigravity; when it cannot be
enforced the status bar says so at launch. Unlike the loop guard it fails
open when the store is unreachable. Report-only, the file cap and the
no-push rule remain loop rules.

## Codex

Codex does not receive an injected native session id. agent-mux watches recent
date directories under `~/.codex/sessions/` and adopts a fresh rollout whose
first `session_meta.cwd` matches the pane's working directory. It scans dates
around the launch time to accommodate date-boundary and time-zone differences.

When hooks are enabled, agent-mux adds a per-launch `-c notify=[...]` override
which invokes `agent-mux trace hook codex-notify`. The override carries the
launch id and chains the user's existing Codex `notify` command, if one is
configured. This is not a persistent change to `~/.codex/config.toml`.
The hook announcement can name Codex's thread id and wins over the watched
heuristic; agent-mux then resolves that thread to its rollout file.

The Codex parser uses rollout records for user and assistant messages,
reasoning, function/shell calls and outputs, model context, explicit task turn
boundaries, and `token_count` events. Codex reports cached input as part of
input and reasoning as part of output, so the tracer normalizes them into
disjoint cost buckets before pricing.

Capture also reads execution and MCP lifecycle events, including exit codes,
errors and clean server/tool names. `collab_agent_spawn_end` and
`sub_agent_activity` with kind `started` discover child rollouts; reporting
the same child through both formats does not duplicate it. An interaction
with an existing child does not attach that child to the interacting turn.
Child files that appear later are retried while the launch remains active.
Each `token_count` closes a model step, rather than attaching all usage to
the final assistant response. Native `turn_id` values supply turn identity
when present.

Inconsistent token counts are retained as raw usage and marked
`usage_invalid`; normalized usage and calculated cost stay unknown. Cached
input and reasoning must not exceed the inclusive counts they belong to.

## Antigravity (`agy`)

Antigravity traces conversations stored below its `antigravity-cli` root. A
launch with `--conversation <id>` is deterministic; otherwise agent-mux
watches for a new `brain/<conversation>` directory. Because agy exposes no
equivalent exact launch marker, watched adoption uses available evidence
(presence-lock modification time and a working-directory check) and, after a
short delay, may adopt the sole unclaimed candidate. Such sessions are marked
as heuristic correlation in trace metadata.

An exact signal comes first when there is one. The trace plugin records the
payload's `conversationId` under the launch's `$AGENT_MUX_SESSION_ID` on the
first model call of each turn, and the launch adopts that conversation as
`announced`. A conversation announced by another launch is never a heuristic
candidate. A new conversation must also be seen for 3 s before a heuristic
adopts it, which gives its hook time to land.

The transcript parser maps user and planner/model steps, thinking, tool calls,
tool results, and `step_index` values. Antigravity's interactive transcript
does not carry token accounting. Once its transcript is adopted, agent-mux
also polls:

```text
<antigravity root>/conversations/<conversation-id>.db
```

It reads newly appended `gen_metadata` protobuf records, joins them to
transcript steps, and adds model, prompt/output/thought tokens, context/cache
information, latency, and time-to-first-token. Unknown, locked, or changed
database records simply yield no usage instead of interrupting the trace.

Antigravity hooks are not injected per launch, because agy loads them from its
customization roots. They are available as an explicit opt-in installation:

```text
agent-mux trace hooks install agy
```

## OpenCode (`opencode`)

OpenCode 2 writes no transcript file. Every session is a row of
`session_v2` and every message a row of `session_message` (`type` in
`user`, `assistant`, `idle`, `system`, `agent-switched`; `seq` increasing
per session; `data` JSON) in one SQLite database shared by every OpenCode
process, `~/.local/share/opencode/opencode.db` (`opencode debug paths`;
`[tracing] opencode_db` overrides it). `tracing::opencode_db` reads it
read-only and fails open; `transcript::opencode_row_events` turns one row
into transcript events, so the assembler is the same as for the file
providers (probed on opencode 2.0.32, 2026-10-31).

**Correlation.** `opencode --session <id>` names the conversation on both
command forms and creates it when it does not exist, so a fresh launch is
`deterministic` the way Claude's is: the plan injects `--session ses_…`
(an id in OpenCode's spelling) and adopts the session once its row exists
(`CorrelationSpec::KnownOpenCode`). A `--session <id>` the profile or a
resume already carries is a known resume: the rows so far are primed,
never re-recorded. `--continue`, `-c` or a `run … --fork` cannot name the
session up front: the pipeline watches for the newest top-level session of
the launch folder created since the launch (`WatchOpenCode`, `watched`).
`inject_session_id = false` disables the injection and leaves the watch.

**Rows to events.** A `user` row is the prompt; a `system` row (instruction
notices) is harness chatter on a user line; an `assistant` row is one
assistant message — its `text` parts joined, its `reasoning` parts as
thinking, `model` as `<providerID>/<id>`, `tokens.{input, output,
reasoning, cache.{read, write}}` as its usage (charged once by the row
id; `usage::normalize` keeps `cache_read` apart from `input`), then a
tool call and a result per `tool` part (`state.{status, input, content |
output | error, metadata}`; the part's `time.{created, completed}` date
them; `status = "error"` fails the result). An assistant row is rewritten
while its message streams and gets `time.completed` when done: the reader
hands rows over in `seq` order and stops at the first unfinished one until
the next poll. An `idle` row closes the turn (`outcome = succeeded`
completes it, anything else aborts it). The message `cost` OpenCode
reports is summed into the turn's `provided_cost` (`$` in the sidebar,
not `≈$`), so a free model shows `$0.00`.

**Backfill.** `agent-mux trace import --discover` imports every OpenCode
session that has messages, alongside the transcript files
(`--discover --provider opencode` for OpenCode alone). It reads the same
completed rows a live launch reads, dates the session by OpenCode's own
clock, and is idempotent: the launch id derives from the database path and
the session id, so running it again rewrites the same rows, and a session
still running imports up to its message in progress. A subagent's session
imports as a session of its own.

**No hooks.** OpenCode has plugins, not hooks; on 2.0.32 a plugin file in
its `plugins/` directory is not loaded by a standalone server and hangs
`opencode run --standalone`, so agent-mux registers none: the write guard,
the budget guard and the urgent-message injection do not reach OpenCode
(`trace doctor` lists `opencode on PATH` and `opencode session db`). Its
`parent_id` on subagent sessions and its `session_pending` (compaction)
rows are not read yet.

## Content, privacy, and destinations

`content_mode = "full"` (the default) retains prompts, responses, thinking,
and tool I/O after built-in secret-pattern masking, any configured
`redact_literals`, and per-field truncation. `content_mode = "metadata"`
stores no prompt, response, or tool bodies; names, timings, model, usage,
cost, and error information remain available. A profile can override the
global mode.

The default destination is local SQLite and needs neither keys nor network
access. Set `backend = "langfuse"` or `"both"` and configure
`[tracing.langfuse]` (or the `LANGFUSE_*` environment variables) to export the
same assembled rows as OTLP spans. If Langfuse is requested without usable
credentials, the launch safely falls back to local tracing and records the
requested backend on its launch row.

Useful inspection commands are:

```text
agent-mux trace doctor
agent-mux trace ls
agent-mux trace show <trace-id> --tree
agent-mux trace show <trace-id> --timeline
```

See `profiles.example.toml` for the complete configuration surface, including
custom storage locations, provider directories, hook controls, price overrides,
and retention.

## SQLite capture version 9

New captures use native turn/message/tool identities when the source supplies
them, so replaying a partial transcript does not renumber those identities.
Older source formats without native turn IDs retain the ordinal fallback.
An unfinished native turn already recorded locally can be reconstructed on
resume and extended under its existing ID. Hook-only and legacy sessions
retain the previous priming behavior.

The generation inspector labels `input_scope`: the first generation carries
the turn prompt; later generations carry the preceding tool results when
available. This is incremental input, not a claim to contain the model's full
context. If no such input is available, the field is absent. Thinking and
tool bodies continue to obey the configured content mode and redaction.

The store accepts late assignment of an unknown parent and rejects cycles
and cross-trace parent links. Trace latency includes recorded child work
that finishes after the turn, and is clamped to zero. Agent containers carry
no duplicated billable usage; their generations supply the totals.

Claude capture also retains Claude-authored `ai-title`, compaction boundaries,
PR/file-history facts, skill inventories, and remote-session links. The title
record replaces the prompt fallback when it arrives. Compaction is both an
event observation and turn metadata, so it is visible in the timeline and
the Loop context summary.

Each generation stores provider usage separately from agent-mux's normalized
usage and computed cost maps. A provider-supplied cost map suppresses local
pricing altogether; agent-mux never blends individual cost buckets from two
sources. Langfuse exports use its price-key vocabulary for cache lifetime and
reasoning buckets.

Migration preserves existing traces and score targets. `ordinal_salted` and
the persisted observation `is_error` flag have been removed; an error is
`level = ERROR` plus its status message. Scores now support numeric,
categorical, boolean, text, and correction data and can target an observation
as well as a turn, session, or launch. Sessions with pre-v5
rows are marked `legacy_capture`; importing one replaces only that session's
capture rows in a transaction, then rebuilds it with deterministic native or
timestamp fallback IDs. Other sessions and score records remain intact.

`trace score` accepts `--type numeric|categorical|boolean` and
`--source annotation|api|eval`; categorical values are retained as text and
are exported to Langfuse with their type and source.

All timestamps shown by the CLI, TUI, and SQLite summary views use local time.

Capture remains best effort. Child offsets are in memory; there is no
transactional ingestion checkpoint or write-acknowledged replay queue yet.
Work written after the launch stops requires a later import. A child file
that shrinks is marked incomplete and its capture is stopped for that launch;
reimport reconstructs it from the source. Missing child files contribute
unknown usage, not zero. Full conversation replay, media inspection, timeline
zoom and richer evaluation controls are later work; this version improves
the data used by the existing list/tree/timeline/loop/summary views.

## Code map

| Area | Main implementation |
| --- | --- |
| Launch planning, pipeline lifecycle, and sink routing | `src/tracing/mod.rs` |
| Provider/session correlation | `src/tracing/correlate.rs` |
| Safe incremental transcript tailing | `src/tracing/tail.rs` |
| Claude, Codex, and Antigravity JSONL parsers | `src/transcript.rs` |
| Turn/observation assembly and content policy | `src/tracing/map.rs` |
| Hook parsing, registration, and live feed | `src/tracing/hooks/` |
| Antigravity usage database reader | `src/tracing/agy_usage.rs` |
| Provider-aware usage normalization and pricing | `src/tracing/usage.rs`, `src/tracing/pricing.rs` |
| SQLite schema, writer, and trace queries | `src/tracing/store/` |
| Optional Langfuse OTLP mapping/export | `src/tracing/langfuse/` |
