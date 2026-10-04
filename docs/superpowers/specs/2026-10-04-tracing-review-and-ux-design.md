# Tracing: review and a better UX — design

Status: review and proposal, 2026-10-04. Done since: phase 1 (prices from LiteLLM, provenance marks, the unpriced notice, the trailing stats push), phase 2 (the trace strip, `T` cycling off → line → rows → browser), and of phase 3 the hook wake socket, a 250 ms poll default, the change-feed trim, the FTS trigger guard (schema v15) and the history rescan off the main thread; still open from phase 3: filesystem watchers, flush on wake, the agy lock adoption and the snapshot gating; phase 4 done except the off-thread browser refresh and the CLI naming pass; of phase 5, Codex `token_usage_record` is read as the exact per-response usage (verified on a real rollout: 148 of 148 responses, where the delta path had 147 and 1,152 fewer output tokens); still open: Claude `PreCompact`/`Notification`, Codex resume detection and the per-launch hooks probe, the Antigravity tool-id probe. Covers capture for the three harnesses (Claude Code 2.1.289, Codex CLI 0.159.2, Antigravity 1.2.16, the versions installed when this was written), the pipeline's speed, and every surface where the user meets a trace. Facts cite `file:line` at commit c4c884f; the store numbers come from this machine's `~/.agent-mux/traces.db` (355 MB, schema 14, 567 sessions, 969 traces, 41,222 observations, 708 launches, 6,498 hook rows). Nothing here is implemented yet.

## 1. The verdict in five lines

1. **Capture is broad and the pipeline is sound**: three harnesses, hooks on two of them, a WAL store with a change feed, a bounded writer, ~0.75 s from a transcript line to a store row. The architecture does not need replacing.
2. **Precision is weaker than the UI lets on.** Every generation on the current models is unpriced, so cost is missing or wrong for all recent work; nearly every turn carries approximate timing; Antigravity pairs tool results by order; Claude's own reported cost is parsed and then never used.
3. **Near real time stops at the store.** The badge misses the trailing update, the Trace Browser is a modal that hides the session it describes, and the user cannot watch a trace and type at the same time.
4. **The UX is a browser bolted on the side**, with three names for one key, five detail views the help lists as four, and columns the store has but the screen never shows (tool names, arguments, errors, turn output, depth).
5. **The fix is mostly surface and plumbing**, not a rewrite: a live trace strip under the session, honesty markers for precision, a refreshed price table with an "unpriced" warning, event-driven wakeups instead of polling, and one vocabulary.

## 2. How capture works today

### 2.1 Per harness

| | Claude Code | Codex CLI | Antigravity |
| --- | --- | --- | --- |
| **Transcript** | `~/.claude/projects/<slug>/<uuid>.jsonl`, tailed (`tail.rs:90-129`) | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (`transcript.rs:951-1179`); the 128 rollouts here are all "already paginated" (`codex migrate-rollouts`) | `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript_full.jsonl`, else the condensed one (`correlate.rs:130-141`) |
| **Session id** | injected `--session-id` unless resuming or print mode (`mod.rs:589-635`) | none exists | `--conversation <id>` only when the user passes it |
| **Hooks** | 12 events via inline `--settings`, async except `SessionEnd` (`register.rs:139-151`); `PreToolUse` sync only with a guard | per-launch `notify` = `TurnComplete` only (`hooks/mod.rs:382-414`); 10-event `hooks.json` needs a manual install and trust (`install.rs:18-31`) | plugin with 4 events, installed by hand; `PostToolUse` has no tool id, so it is dropped (`map.rs:2492-2494`) |
| **Usage / cost** | transcript usage per message; `cost-state` parsed (`map.rs:2322-2331`) | `token_count.last_token_usage` (`transcript.rs:1149-1159`) | protobuf rows in `conversations/<id>.db`, field paths derived from agy 1.1.25 (`agy_usage.rs:1-22`) |
| **Tool pairing** | by `tool_use_id` | by call id; errors only from `*_end` events (`transcript.rs:1130-1141`) | **by order**, oldest open tool first (`map.rs:2085-2093`) |
| **Correlation (this store)** | 164 announced by hook, 132 deterministic | 79 deterministic, 34 watched, 6 none | 205 deterministic, 62 watched, 22 heuristic, 4 none |
| **Hook rows here** | 6,457 across 11 events | 40 `TurnComplete` | 0 |
| **Subagents** | Task/Agent tools, sidecar files, `SubagentStop` (`map.rs:2873-2969`) | collab spawn events (`map.rs:2665-2802`) | none |

### 2.2 The pipeline

Nothing watches the filesystem. Transcripts, hook rows and agy usage are polled every `poll_interval_ms` = 500 (`config.rs:622`, floor 50 at `mod.rs:976`); the writer batches up to 512 ops or `flush_interval_ms` = 250 (`writer.rs:28, 230-248`); WAL with `synchronous = NORMAL` (`store/mod.rs:250-253`); a `trace_changes` feed written by triggers (`schema.rs:547+`) lets the browser skip re-queries when nothing moved (`app.rs:1976-2009`). The hook process writes straight into `hook_events`, but the pipeline only notices by polling (`hooks/feed.rs:128-157`).

| From the harness writing it to… | Claude tool call | Codex turn | Antigravity step |
| --- | --- | --- | --- |
| the store row | 0–750 ms | 0–750 ms | the step line is written only when the step ends, so never in flight; usage arrives on a later poll |
| the sidebar second line | +0–1 s, **and no trailing update**: a commit within 1 s of the last stats push is skipped and never rescheduled (`mod.rs:288-305`) | same | same |
| the Trace Browser | +0–500 ms, auto, **paused while a search is open** (`app.rs:1979`) | same | same |
| first adoption of a new session | 1–2 polls | the `cwd` watch, usually before the end-of-turn hook | up to **15 s** for a sole candidate (`correlate.rs:71-73`) |

Exit: 3 grace ticks (≤ 1.05 s), then finalize; a late exit code or row after that is not attached to the launch (`mod.rs:1600-1625`). Quit: `shutdown_flush_ms` ≥ 1500 (`main.rs:195-199`).

### 2.3 What the user sees

| Surface | Reached by | Live? |
| --- | --- | --- |
| Session second line: `⚙ tool` while working, else `model · Nt · tokens · $c` | always, under each session (`ui.rs:758-786`) | 1 s throttle, trailing update lost |
| `[● Nt $c ▸ tool]` badge | main pane title only (`ui.rs:990`) | same |
| `t` start/stop tracing on the selected session | list focused (`app.rs:651-655`) | — |
| Trace Browser: Sessions / Turns / Detail, `v` cycles List → Tree → Timeline → Loop → Summary | `T`, modal, 96 % of the screen (`ui.rs:2756-2766`) | 500 ms |
| Briefing preview (Heimdall card) | a `trace.read` skill row selected (`ui.rs:1306-1600`) | 1 s |
| `agent-mux trace ls / show / search / doctor / recost / …`, 20 subcommands | CLI (`cli.rs:17-52`) | per call |
| MCP: 10 `agent_mux_*` tools; Heimdall's dossier | agents (`mcp/server.rs:38-84`) | per call |

## 3. Findings

### 3.1 Precision

Ranked by how much of the store they touch.

| # | Finding | Evidence | Scale here |
| --- | --- | --- | --- |
| P1 | **Current models are unpriced**, so every recent generation has tokens but no cost. The bundled table (`pricing.toml`, 16 rows) ends at claude-fable-5-1, opus-5 and the 4.x line; `claude-opus-5-5`, `gemini-3.8-flash`, `gemini-3.7-flash`, `gpt-5.6-*` and `gpt-6-*` all miss. An unknown model gets no cost and no `model_id` (`store/mod.rs:634-646`); the budget guard sums those costs (README:911). | `SELECT provider, model … WHERE type='generation' AND total_cost_usd IS NULL` | 10,159 of 10,159 agy generations, 1,594 of 8,191 Claude, 1,765 of 1,765 Codex; 101 M tokens unpriced |
| P2 | **Claude's own reported cost is never used.** `cost-state` lands in the *turn's* metadata (`map.rs:2328-2331`); the store reads `provided_cost` from the *observation's* metadata (`store/mod.rs:889-894`); `gen_row` never copies it (`map.rs:725-757`). README:785 says reported cost wins. | code | every Claude session |
| P3 | **Timing is approximate almost everywhere.** `timing_approx` is set when any timestamp was approximate or when a closed turn has a generation (`map.rs:678`); a generation's start is back-dated to the previous event (`map.rs:1850-1854`). | `SELECT provider, SUM(timing_approx)` | 425/459 Claude, 189/198 Codex, 309/312 agy traces |
| P4 | **Antigravity tool results pair by order** because agy gives tools `id:""` (`transcript.rs:850, 867`); turn keys are `at-<start ns>`; the model is scraped from prose until the DB row lands; adoption falls back to a cwd substring (`/proj` matches `/proj2`) or "sole candidate after 15 s" (`correlate.rs:481-507`). | code | 210 sessions, 22 heuristic + 4 unlinked launches |
| P5 | **Codex usage is lost or merged.** A `token_count` with no pending generation is dropped (`map.rs:2168-2175`); consecutive assistant messages merge until usage arrives (`map.rs:1874-1889`); `*_output` is always `is_error:false` (`transcript.rs:1130-1141`); correlation is cwd + mtime, first claim wins, no resume detection (`correlate.rs:363-416`). | code | 6 unlinked launches, 34 watched |
| P6 | **Hooks pin by wall clock.** Async hook timestamps are the hook process's clock (`hooks/mod.rs:73`) and are assigned to turns by time windows, 60 s before to 2 s after a prompt (`map.rs:3147-3157`). | code | all Claude hook rows |
| P7 | **`mark_backfill_truncated` is empty** (`map.rs:435`) while `tail.rs:44-45` says its caller must salt trace ids after a truncated prime: a resumed session primed past `backfill_max_bytes` can collide ids with the earlier run. | code | resumes only |
| P8 | **Subagents left open**: 24 Claude `agent` observations have no end (3 `agent: Explore` invocations and bare subagent turns), so their latency and cost roll up wrong. | `SELECT … WHERE type='agent' AND end_ns IS NULL` | 24 rows |
| P9 | Smaller: skill loads guessed from tool paths (`map.rs:3297-3324`); `invoke_subagent` keeps only the first of several agents (`map.rs:3343-3345`); user declines detected by four fixed sentences (`map.rs:3172-3192`); `try_send` drops rows under backpressure and counts them (`mod.rs:1111-1117`); import guesses the provider by substring (`transcript.rs:162-170`). | code | — |

The precision a session actually has is **never shown**. `correlation` (deterministic / announced / watched / heuristic / none), `timing_approx`, `provided_cost` versus estimated, `parse_errors`, `dropped_ops` are all in the store and absent from every screen.

### 3.2 Speed

| # | Finding | Evidence |
| --- | --- | --- |
| S1 | **Polling everywhere, no FSEvents/kqueue/inotify.** `notify` is not a dependency. Each live session costs a `stat` every 500 ms, a SQL poll of `hook_events`, and for agy a protobuf read. | `Cargo.toml`; `tail.rs:90-128`; `feed.rs:128` |
| S2 | **Hooks are not a push channel.** The hook process already has the event in hand and writes it to SQLite; the TUI learns of it on the next poll. The information is in the process ~0 ms after the harness fired it and reaches the screen 0.5–1.5 s later. | `cli.rs:2670`; `feed.rs:128-157` |
| S3 | **The badge drops the trailing update** (1 s throttle, no deferred push), so a turn's final tokens and cost can wait for the next commit or the exit. | `mod.rs:288-305` |
| S4 | **Every re-upsert re-indexes FTS** (`AFTER UPDATE OF input, output` deletes and reinserts), and in-flight → result, hook pins and agy usage each re-upsert the same row. | `schema.rs:319-322`; `map.rs:2464` |
| S5 | **`trace_changes` is never pruned**: 99,059 rows here and growing with every write; `prune` skips it. | `store/mod.rs:524-541` |
| S6 | **Commit hook aggregates a view per commit** (`launch_stats` over `trace_stats`, `GROUP BY t.rid`); whether SQLite pushes the `launch_id` filter into the aggregate is unverified; `EXPLAIN QUERY PLAN` would settle it. | `query.rs:660-681`; `schema.rs:411-433` |
| S7 | **Main-thread work on PTY exit**: `reload_history_sessions` rereads every project's transcripts, "seconds per exit, twice per session", on the UI thread. | `app.rs:5600-5607` |
| S8 | **Browser and briefing queries run on the main thread**; the session-group query (`json_extract` over `launches LIMIT 5000`) is called "the browser's heaviest". The live snapshot is written to disk every second unconditionally. | `app.rs:1429-1432, 1600, 2601-2624` |

### 3.3 UX

| # | Finding | Evidence |
| --- | --- | --- |
| U1 | **Modal.** The browser takes the whole screen and every key; the chord layer is off inside it. You cannot watch the trace of the session you are typing into. | `app.rs:697, 3327-3328` |
| U2 | **The store knows more than the screen.** `ObservationView` carries `kind`, `tool_name`, `tool_id`, `skill`, `mcp_server`, `path`, per-kind tokens; none is drawn. A turn's output is never shown; its input only when it has no observations. `hook_events`, `experiments`, `workflow_steps` have no surface. The Timeline indents by `depth`, which the query leaves at 0. | `query.rs:367-400`; `app.rs:1791-1817`; `ui.rs:2968-2978, 3537` |
| U3 | **Three names for the verdict key**: README says `s` twice, the panel map, code, footer, help and keyboard guide say `+`, `scores.rs:10` says `s`. The `v` cycle has five views; help and the doc comment list four. | README:518, 543, 610; `ui.rs:2205` |
| U4 | **Vocabulary drifts**: "Turns" pane versus `trace show <trace>`; "Session [launch or key]"; `t` labelled "trace" meaning "tracing on/off"; tokens printed raw in the briefing and formatted in the browser; state printed in Rust `{:?}`. | `ui.rs:1383-1385`, `cli.rs` |
| U5 | **Widths and hints**: names cut at 28/48/22/12, the footer hint is ~140 columns and is not fitted, the session-row hint is 119 columns while README:389 promises 100. | `ui.rs:3057, 1960` |
| U6 | **Keys clash with the keymap**: `v` means About, validate and detail view in three places; expanded detail scrolls with `PgUp/PgDn/Home/End`, which the guide says nothing needs; `g`/`G` move `scroll_offset`, which list views ignore. | `docs/keyboard.md:3-5`; `app.rs:4438-4446` |
| U7 | **No honesty.** Nothing says "cost estimated", "model unpriced", "matched by folder, not by id", "timing approximate", "4 rows dropped". A user reading `$0.00` on a Gemini session believes it. | §3.1 |
| U8 | Live-refresh pins row 0 and turn 0 regardless of focus, falls back to the last observation, and stops entirely during a search. The Tree view re-nests observations on every draw. | `app.rs:2034-2064, 1979`; `ui.rs:3432` |
| U9 | Doc drift: README still describes sidebar badges that moved to the pane title; two different briefing paths; `view.rs` says "tree order", the query returns chronological. | README:345-350, 292; `docs/heimdall-dossier.md:3` |

## 4. The proposal: a trace you can watch while you type

### 4.1 Principles

1. **The trace lives with the session.** The first place a trace appears is under the session it describes, live, without leaving the pane. The browser is where you go for history and comparison, not for "what is happening now".
2. **Say how good the number is.** Every cost, latency and attribution carries its provenance: reported or estimated, exact or approximate, matched by id or by heuristic. Unknown is shown as unknown, never as zero.
3. **Push, don't poll.** An event the hook process already holds should reach the screen in tens of milliseconds, not after two timers.
4. **One vocabulary**: *session* (one harness process), *turn* (a prompt and its answer), *call* (a tool, agent or model call inside a turn), *tracing* (the capture). "Trace", "launch", "observation" and "key" stay in the code and the SQL.
5. **One keymap**: the trace surfaces use the shared verbs and the chord layer; no key gains a second meaning.

### 4.2 The trace strip

A strip at the bottom of the main pane, under the session's screen, one to eight rows tall, toggled with `T` from the list and the chord layer from the pane (`⌘T` is the host's: use `⌘/`-style precedent and give it `Ctrl+Shift+T` / a list key only; see §6). The session keeps the keyboard; the strip only displays.

```
┌─ Claude Code · ~/proj · working ─────────────────────────────────────────┐
│ … the session's screen …                                                 │
├─ turn 12 · 8 s · 3 calls · 12.4k tok · ≈$0.04 · matched by id ───────────┤
│ ▸ Read src/app.rs:3600-3700                     0.1 s   1.2k ▸ ok        │
│ ▸ Bash cargo test --lib keymap                  2.4 s     -   ▸ ok       │
│ ● Edit src/keymap.rs                            running                  │
└──────────────────────────────────────────────────────────────────────────┘
```

- **Header line** (always, even when the strip is folded to one row): turn ordinal, elapsed, calls, tokens, cost with its mark (`$` reported, `≈$` estimated, `?` unpriced), and the session's precision tag (`matched by id` / `matched by folder` / `heuristic` / `not traced`).
- **Rows**: the current turn's calls in order, each with its kind glyph, tool name and first argument (the path, the command, the agent name), state (running / ok / error / declined), duration and tokens. Errors in red with the first line of the message. Subagents indent one level with their own running total.
- **Folds**: `←`/`→` on the strip are not available while the pane has the keyboard; the strip folds with the same `T` (cycle: off → one row → eight rows → off) and remembers its height per session.
- **Antigravity** shows what agy writes: finished steps only, with "step pending" while nothing has arrived, and usage as it lands.
- **Live**: the strip redraws from the `TraceStats` event and the change feed (§4.5), not from a timer; it never blocks the pty.
- **Empty states** say why: `tracing off (t)`, `no hooks: tool state arrives from the transcript only`, `model gemini-3.8-flash unpriced (C: tracing.models)`.

The strip replaces the pane-title badge, which becomes the folded strip's header. The session's second line in the sidebar stays and gains the trailing update (S3).

### 4.3 The browser, non-modal and honest

The browser keeps its three panes and its views, with these changes:

- **A drawer, not a modal.** `T` from the strip, or `Enter` on the strip's header via the list, opens the browser **in the main pane** with the sidebar still there; the chord layer still works; `Esc` returns to the session. Full-screen stays available (`b` hides the sidebar as everywhere).
- **Show what the store has.** Calls render `tool_name`, the first argument, `kind`, `skill`, `mcp_server`, per-kind tokens (input / output / cache read / cache write / reasoning) and `level`. Turns show their output, folded, under the input. The Timeline indents by the depth the query now computes from `parent_id`.
- **Provenance column.** Sessions: correlation tag and `parse_errors`/`dropped_ops` when non-zero. Turns: `≈` on approximate timing, `$`/`≈$`/`?` on cost. Calls: `paired by order` on agy results.
- **Search keeps live refresh**; results are a filter on the live list, not a frozen copy.
- **Five views, named once**: List · Tree · Timeline · Loop · Summary, in the footer, the help and the README.
- **The verdict key is `+`**, everywhere, and `scores.rs` and README say so.
- **Refresh runs off the main thread** (`spawn_blocking` with the read-only connection the briefing already uses), with the result applied on the next tick; the Tree is nested once per data change, not per draw.
- **Widths** come from the pane: names take what is left after the fixed columns; the footer goes through `fit_hints`.

### 4.4 A precision panel

`trace doctor` already knows about unpriced models and provider readiness. The same facts belong in the TUI:

- The **About** view (`v`) gains a *Tracing* block: store size, change-feed size, unpriced models seen in the last 7 days with token counts, hooks installed per harness, agy usage schema version.
- A **one-time status notice** when a session's first generation hits an unpriced model: `gemini-3.8-flash is unpriced: cost shows ? until [[tracing.models]] has a row (C)`.
- The **New session** dialog shows, next to Tracing, what the harness will give: `hooks: tools + prompts` (Claude), `hooks: turn end only; install hooks.json for tools` (Codex), `transcript + usage db; tools paired by order` (agy).

### 4.5 Near real time

| Change | Effect |
| --- | --- |
| **Hook process pokes the TUI.** The hook writes its row, then sends one byte on a Unix socket under `<runtime>/` (or a `SIGUSR1` to the pid in the launch row). The pipeline wakes, reads `hook_events` and the transcript at once. | tool start/end on screen in ~50 ms instead of 0.5–1.5 s |
| **Filesystem notifications** for transcripts and rollouts via the `notify` crate (FSEvents on macOS, inotify on Linux, polling fallback), with the 500 ms poll kept as the safety net. | transcript lines land in ≤ 100 ms |
| **Flush on signal.** A hook or fs wake flushes the writer at once; the 250 ms batch stays for the bulk path. | store row within ~10 ms of the wake |
| **Trailing stats.** The 1 s throttle schedules a deferred push instead of dropping. | the sidebar line and the strip always end on the final numbers |
| **Agy adoption** reads `presence/<id>.lock` first and adopts on its creation, not after 15 s; the sole-candidate rule stays as fallback. | agy sessions appear in seconds |
| **Change feed pruning**: keep the last 24 h or the last 50k rows in `prune` and on startup. | S5 |
| **FTS only on text change**: the trigger compares `OLD.input IS NOT NEW.input`; usage-only upserts skip the index. | S4 |
| **History rescan off the main thread** (`spawn_blocking`, result applied on tick), and the live snapshot written only when the change feed moved. | S7, S8 |

### 4.6 Capture precision, per harness

**All**: refresh `pricing.toml` with the current models (`claude-opus-5-5`, `gemini-3.x`, `gpt-5.6-*`, `gpt-6-*`), and ship a `trace doctor --unpriced` that prints the `[[tracing.models]]` rows to paste; a `trace recost` run afterwards fills the history. Make `mark_backfill_truncated` do what its callers expect, or drop the contract and document the collision. Close subagent observations on `SubagentStop`/`Stop` with the turn, flagged `closed_by = turn`.

**Claude Code**: copy `provided_cost` from the turn into its generations (or compute the per-turn delta of `cost-state` and store it on the trace as reported), and prefer it over the table, as README promises. Register `PreCompact` and `Notification` (context pressure and permission prompts are events the strip should show; `PermissionRequest` once the parser accepts it). Use `UserPromptSubmit`'s own `prompt_id` instead of a 60 s window when the transcript carries it.

**Codex**: fix the dropped `token_count`; split merged assistant messages on `message.id` when present; read `*_end` errors into the tool row's `level` at pairing time; detect resume (`codex resume`, `--last`) and prime like Claude. Probe (per the repository rule) whether `-c hooks=…` can register the tool hooks per launch the way `notify` is, which would make the installed `hooks.json` unnecessary; and whether `codex app-server` exposes a per-thread event stream that an attached session can subscribe to.

**Antigravity**: probe the plugin API for a tool id in `PostToolUse`; if none, keep order pairing but mark it. Use the `conversations/<id>.db` row's step index as the turn key instead of `at-<ns>`. Pin the usage schema to the agy version in the About block and fail loud when the field paths stop matching.

## 5. Phases

1. **Honesty and price** (small, high value): pricing refresh and the unpriced notice; provenance marks on cost and timing; Claude reported cost; the trailing stats push; the `+`/five-views doc fixes. Store and schema untouched.
2. **The strip**: the strip under the pane, fed by `TraceStats` and the change feed; the pane-title badge becomes its header; the sidebar line keeps working.
3. **Push**: hook socket wake, `notify` watchers, flush on wake, agy lock adoption, feed pruning, FTS trigger guard, rescans off the main thread.
4. **The browser**: non-modal drawer, the missing columns, provenance column, live search, off-thread refresh, width rules, naming pass, README and `docs/keyboard.md`.
5. **Harness depth**: Codex `token_count`/errors/resume, per-launch Codex hooks if the probe allows, agy ids if the plugin API allows, Claude `PreCompact`/`Notification`.

Each phase ships on its own and leaves the store schema compatible; phase 3 adds no columns, phase 4 adds a computed `depth` to the observation query only.

## 6. Open questions and probes

- **The strip's chord.** `⌘T` is new-tab in every host terminal. Candidates: a list key only (`T`, as today, now a toggle) plus `Ctrl+Shift+T` where the host leaves it (Windows Terminal and GNOME Terminal bind it to new tab). Probe with `agent-mux keys` before choosing; a fallback that needs the list is acceptable since the strip stays open while typing.
- **Codex per-launch hooks**: does `-c hooks=[…]` or an equivalent exist in 0.159.2? `codex --help` shows only `notify`. Probe `codex app-server generate-json-schema` for a thread event stream.
- **agy tool ids**: does the plugin payload of `PostToolUse` carry any id in 1.2.16? The handler drops it today; capture one payload with `AGENT_MUX_HOOK_DEBUG`.
- **`launch_stats` plan**: `EXPLAIN QUERY PLAN` on the commit-hook query with a 40k-observation store; if it scans, add a per-launch rollup table maintained by the writer.
- **Reported cost semantics**: Claude's `cost-state` is cumulative; the per-turn delta is the reported turn cost only if no other client wrote to the session in between. Check against a resumed session.

## 7. Regenerating the price table

`src/tracing/pricing.toml` is generated from LiteLLM's `model_prices_and_context_window.json`, the table ccusage prices from, converted per-token → per-million. Rows LiteLLM does not carry under a bare name are kept from the previous table and marked. Done on 2026-10-04 (phase 1); to refresh:

```sh
curl -sL -o /tmp/litellm.json \
  https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json
python3 - /tmp/litellm.json src/tracing/pricing.toml > /tmp/rows.toml <<'EOF2'
import json, re, sys
d = json.load(open(sys.argv[1])); old = open(sys.argv[2]).read(); M = 1e6
oldrows = {re.search(r'id = "([^"]+)"', b).group(1): dict(re.findall(r'^(\w+) = ([0-9.]+)$', b, re.M))
           for b in old.split("[[models]]")[1:]}
def find(k):
    if k in d: return d[k]
    for x in sorted(d):
        if x.startswith(k + "-20") or x.startswith(k + "@"): return d[x]
    for p in ["anthropic.", "vertex_ai/", "azure/", "gemini/", "openai/"]:
        if p + k in d: return d[p + k]
def row(id, provider, match, key=None):
    r = find(key or id); out = ["[[models]]"]
    if r is None:
        v = oldrows[id]; out.append("# kept from the previous table (not in LiteLLM)")
        f = {k: float(v[k]) for k in ["input","output","cache_read","cache_write","cache_write_1h","reasoning"] if k in v}
    else:
        g = lambda n: (r.get(n) or 0) * M
        f = {"input": g("input_cost_per_token"), "output": g("output_cost_per_token")}
        if g("cache_read_input_token_cost") > 0: f["cache_read"] = g("cache_read_input_token_cost")
        if g("cache_creation_input_token_cost") > 0: f["cache_write"] = g("cache_creation_input_token_cost")
        if g("cache_creation_input_token_cost_above_1hr") > 0: f["cache_write_1h"] = g("cache_creation_input_token_cost_above_1hr")
        if g("output_cost_per_reasoning_token") > 0 and abs(g("output_cost_per_reasoning_token") - f["output"]) > 1e-9:
            f["reasoning"] = g("output_cost_per_reasoning_token")
    out += [f'id = "{id}"', f'provider = "{provider}"', "match = [" + ", ".join(f'"{m}"' for m in match) + "]"]
    out += [f"{k} = {f[k]:g}" for k in ["input","output","cache_read","cache_write","cache_write_1h","reasoning"] if k in f]
    return "\n".join(out) + "\n"
# … one row(...) call per model, as in the current file …
EOF2
```

Then fix integers to floats (`sed -E 's/^(\w+) = ([0-9]+)$/\1 = \2.0/'`), paste the rows under the header, bump `updated_at`, and run `cargo test --lib pricing`: the overlap test refuses two rows that match one name.
