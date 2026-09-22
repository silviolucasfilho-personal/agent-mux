# TDD evidence: restored sessions resume their conversation

**Source plan:** none. The journey comes from the request "Antigravity when
restoring a session is not showing the past dialogs and the history of
output… can you check why?", followed by "implement it test-first".

Branch: `fix/restore-resumes-conversation` (from `master`).

## Diagnosis

`save_active_sessions` stored each open session as its profile, directory
and skill only. On the next start, `restore_saved_sessions` relaunched the
saved profile as it was, e.g. `agy --dangerously-skip-permissions`, with no
conversation id. agy then opened a new, empty conversation. Its own log
shows the difference:

- restore-style launches: `Full redraw completed … for conversation  (epoch 0, items 1)`
- launches given an id: `… for conversation 8fcec510-… (epoch 6, items 70)`

agy replays a conversation when it is given one. Resuming from History was
never affected, because it already passed `--conversation <id>`. Claude and
Codex sessions lost their conversation on restart the same way.

## User journey

As a user who quits agent-mux with sessions open, I want each session to come
back on the next start as the conversation it held, with its dialog and
output, keeping the flags it was started with.

## Design decisions

- `SavedSession` gains an optional `conversation`: the harness's own id.
  Files written before the field load unchanged, and a session without an id
  writes no key.
- On save, the id comes from the launch's `session_key` in the trace store
  (`query::launch_conversation`, which removes the `<provider>:` prefix). If
  the store has no key yet, the save uses the id the session was resumed
  with (`Session::conversation`, set by History and by restore). Restore
  saves again right away, before the new launch is correlated, so the id
  must survive that save.
- `harness::restore_args` keeps the profile's own arguments (model, approval
  flags), removes any resume they already carried (`--continue`/`-c`,
  `--resume`/`-r`, `--conversation`, `codex resume <x>`, including the
  `--flag=value` forms), and adds a resume of the saved id. It differs from
  `resume_args`, which replaces the whole command line on purpose.
- A session with no known id (tracing off, or never correlated) restores as
  before: a fresh launch of its profile.

## Task report

| Stage | Commit | Command | Result |
|---|---|---|---|
| RED (compile-time) | `6782959` | `cargo test --no-run --no-fail-fast` | 13 errors, all intended: `harness::restore_args` (×5), `SavedSession::conversation` (×7), `query::launch_conversation` (×1) |
| GREEN (unit) | see git log | `cargo test --lib -- harness:: persistence::` | `15 passed; 0 failed` |
| GREEN (integration) | see git log | `cargo test --test persistent_sessions --test trace_store --test trace_session` | `9 + 6 + 4 passed; 0 failed` |
| Lint | see git log | `cargo clippy --all-targets`, `cargo fmt --check` | no warnings; fmt clean |

## Test specification

| # | What is guaranteed | Test | Type | Result |
|---|---|---|---|---|
| 1 | Restore keeps the profile's flags and resumes the id, per harness (`--conversation`, `--resume`, leading `resume`) | `harness::tests::restoring_keeps_the_profiles_arguments_and_resumes_the_conversation` | unit | PASS |
| 2 | A resume already in the saved arguments, in any spelling, is replaced rather than doubled | `harness::tests::restoring_replaces_whatever_resume_the_saved_arguments_carried` | unit | PASS |
| 3 | The id round-trips through the sessions file | `persistence::tests::save_and_load_roundtrip` | unit | PASS |
| 4 | A file written before the field loads, and writes no `conversation` key back | `persistence::tests::a_file_written_before_conversations_were_saved_still_loads` | unit | PASS |
| 5 | A launch's `session_key` reads back as the bare harness id; an uncorrelated or unknown launch gives none | `trace_store::a_launch_reads_back_as_its_harness_conversation_id` | integration (real store) | PASS |
| 6 | A saved agy session restores as `--dangerously-skip-permissions --conversation <id>`, keeps the id through the save that follows restore, and resumes it once, not twice, on a second restart | `persistent_sessions::restoring_a_saved_antigravity_session_resumes_its_conversation` | integration (fake `agy`) | PASS |
| 7 | A session resumed from History is saved with its id | `persistent_sessions::a_session_resumed_from_history_is_saved_with_its_conversation` | integration | PASS |
| 8 | A running traced session is saved with the conversation its launch was correlated to (the quit path) | `trace_session::a_running_traced_session_is_saved_with_its_conversation` | end-to-end (fake `claude`, real pipeline and store) | PASS |
