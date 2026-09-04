# How tracing works

`agent-mux` traces the agent sessions it launches for three provider
implementations:

| Provider | Profile command | Primary transcript source |
| --- | --- | --- |
| Claude Code | `claude` | `~/.claude/projects/<project>/<session>.jsonl` |
| Codex | `codex` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` |
| Antigravity | `agy` | `~/.gemini/antigravity-cli/brain/<conversation>/.system_generated/logs/transcript_full.jsonl` (falling back to `transcript.jsonl`) |

Tracing is enabled by default. It is deliberately *fail-open*: a missing
transcript, unavailable database, malformed line, full queue, or failed
remote exporter must not block, slow, or prevent the agent session itself.

## Shared lifecycle

At application startup, `TraceRuntime` resolves `[tracing]` configuration and
opens the local SQLite trace store (by default
`~/.agent-mux/traces.db`). A profile launch is recognized as traceable from
its command name (`claude`, `codex`, or `agy`), or from
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

Claude transcript usage is read from assistant-message usage records. Its
uncached input, cache reads, cache writes (including 5-minute and 1-hour
breakdowns), and output are kept as separate billable buckets.

If an injected launch flag causes two immediate nonzero exits for the same
profile, agent-mux disables session-id and hook argument injection for that
profile for the rest of the run and reports the condition in the status bar.
Set `inject_session_id = false` or `hooks = "off"` in the profile when needed.

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

## Antigravity (`agy`)

Antigravity traces conversations stored below its `antigravity-cli` root. A
launch with `--conversation <id>` is deterministic; otherwise agent-mux
watches for a new `brain/<conversation>` directory. Because agy exposes no
equivalent exact launch marker, watched adoption uses available evidence
(presence-lock modification time and a working-directory check) and, after a
short delay, may adopt the sole unclaimed candidate. Such sessions are marked
as heuristic correlation in trace metadata.

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
