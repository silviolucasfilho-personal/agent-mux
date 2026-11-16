# Software factory: one work item, one line of stations — design

Status: phases 1 and 2 implemented, 2026-11-16; phase 3 proposed.
Sections 14 and 15 list where the code differs from this text.

Modelled on the software factory template of eve (`vercel/eve`,
`apps/templates/eve-software-factory-template`, read 2026-11-15). eve hosts
its stations as model calls in cloud sandboxes; agent-mux runs each station
as a headless session of an installed harness in a git worktree. Nothing
from eve is vendored or executed.

Operator decisions:
- **Engine (2026-11-16):** the line is a workflow. A version with every
  station on the swarm was drafted and compared (§11); the swarm is kept
  for a later step, the build of a large item.
- **Memory (2026-11-15):** the factory does not use ai-memory. Its memory
  is one Markdown file per repository that agent-mux reads and writes (§7).
- **Where the brain lives (2026-11-15):** the library folder, not the
  repository.

## 1. What the code shows today

Read from the docs and the files named; the interpreter (`interp.rs`) was
not read line by line, so §4.2 lists what to confirm there first.

| # | Finding | Where |
| --- | --- | --- |
| 1 | A flow runs steps of six kinds, each on its own harness, agent and model, with typed results (`string`, `integer`, `number`, `boolean`, `array`, `object`) | `workflows::document`, `PRIMITIVE_TYPES` |
| 2 | `until` repeats a step until rounds bring nothing new (`dedupe_by`); no kind sends work back to an earlier step on a verdict | `docs/workflows.md` §2 |
| 3 | A run ends `finished`, `failed`, `cancelled` or `budget-exhausted`; it cannot wait for a person | `workflow_runs.status` |
| 4 | A writing step runs in a worktree on `wf/<run>-<step>`; a worktree with changes is kept, and agent-mux never pushes or merges it | `docs/workflows.md` §4 |
| 5 | The guard blocks `git push`, `git merge`, `git rebase` and `gh pr merge` in a guarded session | `guard::is_human_gate_command` |
| 6 | Only Claude Code, Codex and Muse have a per-launch path guard for workflow sessions | `docs/workflows.md` §3 table |
| 7 | Loops triage issues and propose a change (`issue-triage`, `continuous-pr`, `ci-sweeper`), each as a separate scheduled run with its own state file | `loops/registry.toml` |
| 8 | Swarm reviews already pick a harness other than the worker's | `bus::swarm::review::harness_for` |
| 9 | ai-memory is wired to four harnesses at most (not Grok, Pi, Muse) | `memory::HARNESSES` |
| 10 | Nothing links an issue to the runs that worked on it, or to a branch and a pull request | `schema.rs` (v27) |

The parts of a factory exist. What is missing is the work item: one row
that enters, moves through a fixed line, and ends as a draft pull request.

## 2. The model

A **work item** is a request against one repository: a GitHub issue, or
text the operator typed. GitHub stays the system of record for issues and
pull requests; agent-mux keeps only the tracking row. The **line** is the
built-in flow `factory`. A **station** is one step of it, run by one agent
on one harness.

```
intake ─▶ classify ─┬─ clarify ──▶ needs-answer (questions to the requester)
                    ├─ question ─▶ answer ──────▶ answered
                    └─ build ───▶ research? ─▶ analyze ─▶ implement ⇄ review ─▶ deliver
                                                          (≤ 2 revisions)      (draft PR)
```

Rules the code enforces, not a prompt:

1. Every item runs every station of its branch, in order.
2. A station receives a self-contained context: the item, the earlier
   stations' typed results, the brain. Never another session's transcript.
3. The implementer never judges its work. The reviewer runs as its own
   session, by default on another harness, and reads the committed diff.
4. Stations cannot push, merge or open a pull request. agent-mux pushes one
   branch and opens one draft pull request. Marking it ready and merging are
   the operator's, outside agent-mux.
5. Issue and comment text is data. It never grants a capability.

## 3. Work items

### 3.1 Store (schema v28)

```sql
CREATE TABLE work_items (
  id          TEXT PRIMARY KEY,          -- wi_<ulid>
  workspace   TEXT NOT NULL,
  repo        TEXT NOT NULL,             -- owner/name, or '' without a remote
  source      TEXT NOT NULL CHECK (source IN ('issue','manual','ci')),
  source_ref  TEXT NOT NULL,             -- issue number, '' for manual, PR number for ci
  title       TEXT NOT NULL,
  body        TEXT NOT NULL,
  requester   TEXT,                      -- GitHub login, or NULL (the operator)
  attended    INTEGER NOT NULL,          -- 1: started by the operator here
  state       TEXT NOT NULL,
  kind        TEXT, priority TEXT, complexity TEXT,   -- the classifier's
  run_id      TEXT,                      -- the latest workflow run
  cycles      INTEGER NOT NULL DEFAULT 0,
  branch      TEXT, pr_number INTEGER, pr_url TEXT,
  error       TEXT,
  created_ns  INTEGER NOT NULL, updated_ns INTEGER NOT NULL
);
CREATE UNIQUE INDEX work_items_claim ON work_items (repo, source, source_ref)
  WHERE source <> 'manual';
CREATE TABLE work_item_events (
  item TEXT NOT NULL, ts_ns INTEGER NOT NULL,
  kind TEXT NOT NULL,                    -- state.<s>, run.started, station.<id>, comment.posted, answer, brain.note
  detail TEXT NOT NULL DEFAULT 'null'
);
```

The unique index is the intake claim: an issue becomes an item at most
once, whatever the poll sees later. A retry reuses the row with a new run.

### 3.2 States

| State | Meaning | Leaves by |
| --- | --- | --- |
| `queued` | accepted, no run yet | the dispatcher starts a run |
| `running` | a run is on a station (the station is the run's current step) | the run ends |
| `needs-answer` | the classifier asked questions | `answer` (a new run with the answer appended), `cancel` |
| `answered` | a question item got its answer | terminal |
| `unresolved` | review still failing after the last revision | `retry`, `cancel` |
| `rejected` | the reviewer rejected the approach | `retry`, `cancel` |
| `delivered` | the draft pull request is open | terminal for agent-mux |
| `failed` | a station or the delivery failed | `retry`, `cancel` |
| `cancelled` | stopped by the operator | terminal |

A clarification does not park a run. The run finishes and the item waits;
the answer starts a new run from the classifier with the answer in the
item's text. This keeps `workflow_runs.status` and the journal as they are.

## 4. The line

### 4.1 The document (`workflows/factory.toml`)

```toml
[workflow]
name = "factory"
description = "Take one work item through classify, research, analyze, implement and review; the result is a reviewed branch."
output = "build"
default_isolation = "worktree"

[args.item]
description = "The work item: title, body and source."
required = true

[[steps]]
id = "classify"
kind = "route"
agent = "classifier"
skill = "wf-factory-classify"
isolation = "none"
result = "classification"
branches = { build = ["research", "analyze", "build"], question = ["answer"], clarify = ["clarify"] }

[[steps]]
id = "research"                     # no question, no session
kind = "pipeline"
over = "classify.research_questions[*]"
agent = "researcher"
skill = "wf-factory-research"
isolation = "none"
result = "research"

[[steps]]
id = "analyze"
agent = "analyst"
skill = "wf-factory-analyze"
isolation = "none"
input = "research"
result = "analysis"

[[steps]]
id = "build"
kind = "revise"
agent = "implementer"
skill = "wf-factory-implement"
input = "analyze"
result = "implementation"
review = { agent = "reviewer", skill = "wf-factory-review", harness = "other", result = "review", pass = "verdict == approve", stop = "verdict == reject" }
max_cycles = 2
```

`answer` is a read-only `single` step; `clarify` is a `single` step with
`run = "none"` that only carries the classifier's questions to the result.

### 4.2 Additions to the interpreter

| Addition | Behaviour |
| --- | --- |
| `kind = "revise"` | Runs the step's session in a worktree, then the `review` session in the same worktree, read-only. `pass` true: done. `stop` true: done, outcome `rejected`. Otherwise the work session runs again with its previous result and the review's result in `inputs`, up to `max_cycles` revisions; then outcome `unresolved`. The result is `{ work, review, cycles, outcome }`. Each session is journaled with a key that includes the cycle, so a resume replays finished cycles |
| `harness = "other"` in a `review` table | A harness the operator has a profile for, other than the work session's, chosen as `review::harness_for` does (agy last). With one harness only, the same one, and the run notes it |
| `run = "none"` on a `single` step | No session; the step's result is its `input`. Used for `clarify` |
| Before a review | Rust checks the worktree: no uncommitted change and at least one commit past the base. Otherwise the work session counts as failed and is retried once, as a malformed answer is |

To confirm in `interp.rs` before building: that a `route` result may carry
fields beyond `label`; that a `pipeline` over an empty array completes with
`[]`; that two sessions of one step can share a worktree.

`revise` is a general shape, offered in the flow builder as "work, then
review, send back".

### 4.3 Stations

Agents in `agents/builtin/` (new: `classifier`, `researcher`, `analyst`,
`implementer`; `reviewer` exists) and step skills in `workflows/skills/`.
All are registered in `agents::BUILTIN`, `skill::builtin_packages`,
`workflows::library::BUILTIN` and `Catalog::load`, so each is editable.

| Station | Tools | Result (required fields) |
| --- | --- | --- |
| classifier | none | `label` (`build`/`question`/`clarify`), `kind` (bug, feature, refactor, question, chore, security), `priority`, `complexity` (trivial, small, medium, large), `affected_area`, `summary`, `questions[]`, `research_questions[]` |
| researcher | web | `summary`, `findings[]` (`claim`, `sources[]` with `url` and `title`, `confidence`, `notes`), `gaps[]`, `artifact` |
| analyst | read, shell | `problem_statement`, `approach`, `plan[]`, `affected_surface[]`, `risks[]`, `acceptance_criteria[]` (at least one), `test_strategy`, `assumptions[]`, `open_questions[]`, `artifact` |
| implementer | read, edit, shell | `base`, `change_summary[]` (`path`, `change`), `verification[]` (`command`, `result`), `deviations[]`, `known_limitations[]`, `brain_note` |
| reviewer | read, shell | `verdict` (approve, request_changes, reject), `criteria_results[]` (`criterion` verbatim, `pass`, `evidence`), `blocking_findings[]`, `suggestions[]`, `summary`, `brain_note` |

The reviewer's context holds the item, the analysis and the implementer's
report, and its skill tells it to read `git diff <base>...HEAD` and to
re-run the cheapest verification command. Its order is correctness,
acceptance criteria, safety, scope, verification, quality; a blocking
finding must trace to one of the first four.

### 4.4 Artifacts

A station may write one longer document to `<run>/artifacts/<id>.md`
(`$AGENT_MUX_WORKFLOW_ARTIFACTS`) and answer its id in `artifact`. Ids match
`^[a-z0-9-]+$`; a file is never overwritten; 256 KiB at most. Later stations
get the path in their context, not the text.

## 5. Delivery (`src/factory/deliver.rs`)

Runs in Rust when an item's run finishes with `build.outcome = approved`.

1. Refuse if the worktree is dirty, the branch has no commit past the base,
   or the remote's default branch cannot be read.
2. Name the branch `<prefix><kind>-<slug>` (`factory/bug-dedupe-reset-emails`),
   adding `-2`, `-3` when the name is taken on the remote.
3. `git push origin <worktree HEAD>:refs/heads/<branch>`: one ref, no
   `--force`, no tags, hooks disabled (`-c core.hooksPath=/dev/null`),
   refused when the destination is the default branch or does not start
   with the prefix.
4. `gh pr create --draft` against the default branch. The body is rendered
   from the results by a template (`prompts.toml` `[factory] pr_body`): the
   problem statement, the approach, the acceptance criteria as a checklist
   with the reviewer's result and evidence, the verification commands and
   their results, deviations, `Closes #N` for an issue item.
5. Record `branch`, `pr_number`, `pr_url`; state `delivered`.

A failure at any step leaves the worktree and sets `failed` with the
reason; `retry` on a `failed` delivery repeats only the delivery. No verb
merges or marks a pull request ready.

## 6. Intake

| Way in | Attended | How |
| --- | --- | --- |
| Operator, by text | yes | `n` on the Factory screen, `agent-mux factory run "<text>"` |
| Operator, by issue | yes | `agent-mux factory run --issue N`, `n` → issue number |
| Label on an issue | no | the poll (below) |
| Red checks on a factory pull request | no | phase 3, §10 |

**The poll.** With `[factory] intake = "label"`, a scheduled Rust job (no
session) lists open issues with the label through `gh`. For each one with
no item it reads who added the label (`gh api …/issues/N/events`) and that
login's permission (`…/collaborators/<login>/permission`). Triage or above:
insert the item. Otherwise ignore it and record nothing. A failed `gh` call
inserts nothing.

**Comments.** agent-mux, not a station, comments on the issue: one line
when a station completes, the classifier's questions on `needs-answer`,
the pull request link on `delivered`, the unresolved findings on
`unresolved`. Text comes from templates in `prompts.toml`.

**Unattended items** differ in two ways: their writing stations run only
on a harness with a path guard (Claude Code, Codex, Muse; finding 6), and
a `needs-answer` item is answered by `agent-mux factory answer`, or by an
authorized comment the poll reads in a later phase.

**Dispatch.** One run per item, at most `[factory] max_concurrent` items
running (default 1), within `[workflows] max_concurrent`. Each item has a
token budget (`[factory] budget_tokens`), passed to the run.

## 7. The brain (`src/factory/brain.rs`)

- **File:** `<library>/factory/<repo key>/brain.md`. The key is the
  normalized `origin` URL (`github.com/owner/name`) hashed, or the hash of
  the workspace path without a remote. A `repo` file beside it holds the
  readable name.
- **Read:** by agent-mux at run start, placed whole in every station's
  context as `brain`. No station opens the file, so it works on all seven
  harnesses, and a change during a run does not reach it.
- **Write:** only agent-mux. The implementer and the reviewer may answer a
  `brain_note`: one durable fact about the repository (a build quirk, a
  check that is not obvious, a convention a station missed). It becomes an
  inbox item (`InboxItem::BrainNote`); `Enter` appends it under today's
  date, `e` edits it first, `d` discards it. Nothing is appended without
  the operator, for attended and unattended items alike.
- **Bound:** `[factory] brain_max_chars = 40000`. An append that would
  pass it is refused with a message to edit the file down.
- **Edit:** `e` on the brain row of the Factory screen opens it in the
  Markdown editor; `agent-mux factory brain show|edit|add "<note>"|path`.
- **Not in:** ai-memory, the repository, or a worktree. Factory sessions
  are workflow sessions, already marked suppressed for the ai-memory feed.

## 8. The Factory screen (`Mode::Factory`)

F1 → "Open factory", `ActionId::Factory`; state and keys in
`src/app/factory.rs`, drawing in `src/ui/factory.rs`. The Library screen's
layout and conventions.

```
┌ factory · api ──────────────┐┌ #214 Reset email arrives twice · review 2/2 ─────────┐
│ ▸ running                    ││  1 Item   2 Line   3 Thread   4 Result               │
│ > #214 reset email   review  ││                                                      │
│ ▸ needs you                  ││  ✓ classify   bug · high · small        codex   8k   │
│   #209 export CSV    ?       ││  – research   no question                            │
│   #201 flaky login   ✗ 2/2   ││  ✓ analyze    5 criteria                claude  41k  │
│ ▸ delivered                  ││  ◷ build      revision 2 of 2                        │
│   #198 typo in docs  PR #221 ││     ✓ implement                         claude  96k  │
│ ▸ brain              3.1k    ││     ▶ review                            codex   12k  │
└──────────────────────────────┘└──────────────────────────────────────────────────────┘
```

- **Tree:** items under `running`, `needs you` (`needs-answer`,
  `unresolved`, `rejected`, `failed`), `queued`, `delivered`, then the
  brain row. Closed items older than 14 days are hidden.
- **Tabs:** Item (source, requester, classification, branch, pull
  request), Line (the stations, from `workflows::run_view`), Thread
  (`work_item_events`), Result (the review per criterion, or the questions).
- **Keys**, through the action registry: `n` new, `r` retry, `a` answer,
  `x` stop, `Enter` opens the item's run on the Run screen, `e` on the
  brain row, `/` filter, `Ctrl+R` reload. From the Run screen, Live shows a
  station's output as today.
- **Needs you:** `needs you` items count in the status bar and appear in
  the inbox with brain notes.

## 9. CLI and settings

```
agent-mux factory run "<text>" | --issue N   [--workspace DIR] [--budget N]
agent-mux factory ls [--state S] | show <item> | retry <item> | cancel <item>
agent-mux factory answer <item> "<text>"
agent-mux factory poll [--dry-run]
agent-mux factory brain show|edit|add "<note>"|path
```

```toml
[factory]
intake = "off"            # off | label
label = "factory"
branch_prefix = "factory/"
max_cycles = 2
max_concurrent = 1
budget_tokens = 0         # per item; 0 = none
poll_interval_s = 300
brain_max_chars = 40000
setup_command = ""        # run in the worktree before a station that has shell
```

## 10. Phases

| Phase | Contents |
| --- | --- |
| 1 | `revise`, `harness = "other"`, `run = "none"`, artifacts; the five stations and `factory.toml`; `work_items` (v28); delivery; `factory run`/`ls`/`show`/`retry`/`cancel`/`answer`; the brain file read into contexts and the `brain` CLI |
| 2 | The Factory screen; brain notes in the inbox; the label poll with the permission check; issue comments |
| 3 | Red checks on a `factory/` pull request become a `ci` item whose line starts at the analyst and pushes to the same branch; `agent-mux eval` (its own spec) with these cases at least: stations run in order, a thin request ends `needs-answer`, an instruction inside an issue body is not followed, no station pushes, an unresolved review opens no pull request |

Later, not designed here: the swarm for the build of a `large` item (§11);
answers read from issue comments.

## 11. The swarm version, and why not

A version with one swarm per item was drafted on 2026-11-15: stations as
swarm tasks in the item's own worktree, the coordinator planning the build
tasks, each reviewed and merged into the item's branch, a final acceptance
task.

| | Workflow (this spec) | Swarm |
| --- | --- | --- |
| Orchestrator | the interpreter, a fixed order | a model, checked by `plan::check` |
| Sessions | headless | live terminals |
| Parallel build | no | yes |
| Typed results, resume | present | to add; recovery retries once |
| Harnesses | seven | five (no Grok, no Pi) |
| Unattended | its normal mode | needs the plan gate off, and reviews and merges that act without the operator |
| New code | three interpreter additions | typed task results, automatic revision, automatic merge, station checks in `plan::check`, a push block |

The workflow was chosen because a factory is mostly unattended, most items
are small, and the swarm version changes how reviews and merges behave in
an engine other swarms rely on.

What the swarm does better is the build of a large item in parallel, and
work the operator wants to watch and steer. The later step is to let the
`build` station hand the analyst's plan to a swarm in the item's worktree
and wait for its branch; it needs the item worktree and the merge without
the operator from that draft, and nothing else.

## 12. Not taken from eve

Hosted channels (Slack, Teams, Linear, webhooks), the durable workflow
runtime, cloud sandboxes and the credential-transfer sandbox, per-user
preferences, and a model as the orchestrator. The line here is the
workflow interpreter, so "never skip a station" is code.

## 13. Tests

- `revise`: approve at once; one revision; `max_cycles` reached; `reject`;
  resume mid-cycle replays finished cycles.
- Delivery against a local bare remote: refuses the default branch, a
  foreign prefix and a dirty tree; never forces; names a taken branch `-2`.
- The claim: the same issue polled twice makes one item.
- The poll with a stubbed `gh`: a label from a login without triage makes
  no item; a `gh` error makes no item.
- The brain: the key for SSH and HTTPS forms of one remote is the same;
  the bound refuses; no station context is built without it.
- No test runs against the user's home, store or a real repository.

## 14. Phase 1 as built

Where the code differs from the sections above.

| # | Section | As built | Why |
| --- | --- | --- | --- |
| 1 | §4.2 | No `run = "none"`. `clarify` is a `pipeline` with no actor over `classify.questions[*]`, which starts no session | The interpreter already settles an actor-less pipeline |
| 2 | §4.2 | Sessions of a step did not share a worktree: each got its own. A `revise` step now has one per step (`LiveWorkflowRun.step_worktrees`, `<run>/worktrees.json`), kept until the run ends | The third point "to confirm" was not as assumed |
| 3 | §4.2 | A `route` result may carry other fields, and a `pipeline` over an empty array completes with `[]`: both confirmed in `interp.rs` | — |
| 4 | §4.2 | The work session's `inputs` are `{ input, cycle, max_cycles, previous, review }` on every cycle; the reviewer's `{ input, cycle, work }` | One contract for the skill |
| 5 | §4.3 | The classifier has the `read` tool | A workflow session must read its context file |
| 6 | §4.3 | The step procedures are step skills (`wf-factory-*`), as the section says; the result schemas have no `minItems`, so "at least one criterion" is the analyst's instruction and `settle`'s check on the review |
| 7 | §4.4 | The artifacts folder and `$AGENT_MUX_WORKFLOW_ARTIFACTS` exist for every workflow run; the id, no-overwrite and size rules are instructions, not enforced | No tool sits between a session and the folder |
| 8 | §3.1 | Ids are `wi_<10 hex>`; `work_items` has a `worktree` column | — |
| 9 | §3.2, §6 | `retry` is a fresh run, not a resume: a journal replays an `unresolved` review and a null answer as they were. After a failed delivery it only delivers again (`error` starts with `delivery:`) | — |
| 10 | §5 | The work forks from `origin/<default>` after a fetch (`WorkflowRunRequest.worktree_base`), so the pull request holds only the item's commits whatever the workspace has checked out |
| 11 | §5 | The pull request body is rendered in Rust (`deliver::pr_body`), not from a `prompts.toml` template | Lists per criterion and per command do not fit the placeholder templates |
| 12 | §5 | Before delivering, `settle` refuses an `approve` with a failing criterion or none judged (`unresolved`) | An approval is not taken at its word |
| 13 | §2 rule 4 | The push block is the guard's `no_push_verdict` on sessions of a `revise` step (`launches.metadata.workflow_no_push`); it acts only where agent-mux's hooks run for the session | Workflow sessions had no push rule; only loops did |
| 14 | §7 | A station's `brain_note` is printed as a suggestion and recorded as a `brain.note` event; nothing is appended. The inbox item is phase 2 |
| 15 | §9 | `[factory]` has `branch_prefix`, `budget_tokens`, `brain_max_chars`, `setup_command`. `max_cycles` is the flow's own; the intake keys come with phase 2. `setup_command` is handed to the implementer (`args.setup`), which runs it | — |
| 16 | §6 | `factory run` works one item in its own process; there is no dispatcher or queue yet. A running item is stopped with Ctrl-C; `--force` on `retry` and `cancel` takes over an item whose process is gone |
| 17 | §13 | Covered: `revise` in the interpreter (seven tests), the line end to end with fake harnesses and a bare remote (`tests/it/factory_line.rs`), delivery, the claim, the brain, `settle`. Not covered: `factory run` as a process (it needs real harnesses), and the poll (phase 2) |

## 15. Phase 2 as built

Built 2026-11-16 on branch `feat/factory-phase-2`: the Factory screen, the
label poll, issue comments and brain notes in the inbox. Where the code
differs from §6 to §9.

| # | Section | As built | Why |
| --- | --- | --- | --- |
| 1 | §6 | The poll is `agent-mux factory poll`, a process: it inserts the items and then works the queued unattended ones one after another. The running TUI starts it every `poll_interval_s` for its launch directory (`App::factory_tick`); without the TUI it runs from cron or launchd | A run is headless in its own process (§14, 16); the TUI stays a reader |
| 2 | §6 | There is no `max_concurrent`: one item at a time per process, and one poll per repository (`intake::hold` on `poll-<key>`) | A dispatcher with parallel items is not built |
| 3 | §6 | The role is `role_name` of `…/collaborators/<login>/permission` (`permission` when absent); a label no `labeled` event explains is refused | `permission` alone folds triage into read |
| 4 | §6 | Comments: one progress comment per run, edited in place as stations finish, and one new comment for the outcome. Texts are rendered in Rust (`factory::comment`), not `prompts.toml` templates | A comment per station is a notification per station; lists do not fit the placeholder templates (§14, 11) |
| 5 | §6 | An unattended item whose run harness is not Claude Code, Codex or Muse is refused before it starts | The check is on the run's harness; a `--step` override of the build step is not inspected |
| 6 | §7 | A suggested note is a `brain.note` event; the inbox lists those with no `brain.note.added` or `brain.note.discarded` event of the same text for the item. `e` adds the note and opens the brain in the Markdown editor, rather than editing before adding | No schema change; one editor for the brain |
| 7 | §8 | The screen starts nothing in-process: `n`, `a`, `r` spawn `agent-mux factory …` (`intake::spawn_cli`), with output in `<runtime>/factory/<name>.log`. Each working process records its pid (`<runtime>/factory/<item>.pid`); `x` and `cancel` stop it, and a `running` item without a live process reads `lost` | One code path for the shell and the screen |
| 8 | §8 | The Line tab is drawn from the run's journal (one row per finished session), not from `workflows::run_view`; `Enter` opens the full Run screen, and back returns to the Factory screen (`Origin::Factory`) | The run lives in another process, so there is no live tree to read here |
| 9 | §8 | Groups also include `closed` (cancelled). `o` opens the pull request. The local verbs are the screen's own keys; the registry knows the screen for New, Edit, Run, Stop and Reload | — |
| 10 | §8 | Items that need the operator are inbox rows (`InboxItem::Work`), dismissed per state, beside brain notes (`InboxItem::BrainNote`); both count in the status bar through `App.factory_needs`, re-read every 5 s | The status bar is drawn every frame |
| 11 | §9 | `[factory]` gains `intake`, `label`, `poll_interval_s`, `comments`. `max_concurrent` and `max_cycles` are not keys | Rows 2 and §14, 15 |
| 12 | §13 | Covered: the poll and the progress comment against a stubbed `gh` (`tests/it/factory_intake.rs`), the screen, the inbox rows and the poll tick (`tests/it/factory_ui.rs`). Run for real: `factory poll --dry-run` against a labeled issue. Not run for real: an unattended item end to end, and the comments | — |
