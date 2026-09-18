# Loop and workflow results UX plan

Date: 2026-09-18

Status: proposed; implementation is not started. Grounded in a headless
render of the Loops (`E`) and Workflows (`W`) views against the local store
(3 workflow runs, 53 sessions, 49 loop runs), the `loop status` /
`workflow status` CLI output, and the schema and view code. No usability
sessions were observed.

## The problem in one sentence

Both views show the **ledger** of a run (every session, every counter) and
hide the **verdict** (what happened, what it found, what needs the user), so
reading a result means reconstructing it from labels, raw integers and files
on disk.

## What the user sees today

Workflows view, Progress tab, a finished `research` run on Antigravity:

```text
Workflow  research (built-in)
Harness   agy · Antigravity
Workspace /Users/sifilho/workspace/agent-mux
Status    finished
Sessions  21 · 7619k tokens · $0.00
────────────────────────────────────────
sweep                        Sweep          object · 142821 tokens
sweep[1]                     Sweep          object · 1187360 tokens
…
read[14]                     Read           object · 224320 tokens
answer                       Synthesize     text · 63953 tokens
critique                     Critique       object · 887495 tokens
r resumes this run from its journal
```

Loops view, Runs tab, `daily-triage` on findon:

```text
run                  outcome       lvl found  act  esc  tokens    cost    dur  verifier / files
> 2026-09-17T17:37:33Z escalated     L1      8    0    2    417k   $1.18   103s  — / 0 file(s)
    summary   L1 report-only: spec-wave bump (dup of PR #2238) and loop scaffolding both uncommitted >1 d
    launch    323ef87c-1e7c-4bce-9308-337338cab056
  2026-09-16T12:48:32Z escalated     L1      6    0    1    289k   $1.21   117s  — / 0 file(s)
```

## Where it breaks, with evidence

### Workflows

| Symptom | Evidence | Root cause |
| --- | --- | --- |
| No headline. The first thing shown is metadata, then a session list. | Detail header rows are `Workflow / Harness / Workspace / Status / Sessions` (`src/app/workflows_view.rs:807-836`). | The run row has no verdict field; `result` is only the output step's value. |
| The document's structure is flattened into label strings. | `read[5]`, `confirmed[3]/vote2`, `best/r1/judge2.1` are the only encoding of phase → step → item → vote → round (`src/workflows/interp.rs:52-69`). | `workflow_steps` has no parent, index, round or role column. |
| A failed session says `null` and nothing else. | Run `2e64575a`: `read[5] … null · 250092 tokens`. The reason (`the workflow-result block is not valid JSON: EOF …`) is in `result.json` notes and `journal.jsonl`, never on screen for a stored run. | Notes live in memory (`RecentWorkflowRun.notes`) and are lost on restart; the CLI never prints them. |
| Progress and Journal tabs render the same lines for a stored run. | `src/app/workflows_view.rs:838-863`. | Journal was meant for the live case. |
| Results that are not the `output` step are invisible. | Run `3afc71e9`: the `critique` step answered `confidence: high` with three `missing` items; the Result tab shows only `answer`. Vote tallies, the tournament bracket and `until` round counts are never stored. | Aggregates exist only in the interpreter's memory; the view reads `run.result`. |
| Nothing is navigable. | Steps are plain text lines; `Enter` on a stored run prints "r resumes a stored run" (`src/app/workflows.rs:2190`). Per-step `result`, `item`, `changed_files`, `launch_id`, `started_ns/ended_ns` are stored and never shown. | No step cursor in the view state. |
| Numbers are raw. | `1278027 tokens`, `7619k tokens`, and `$0.00` next to 7.6M tokens on agy and 3.5M on codex. | No humanizer; `pricing.toml` has no Antigravity or Codex prices, and the UI prints the zero as a price. |
| Duration is never shown. | `workflow_runs.started_ns/ended_ns` exist (runs took 7, 10 and 22 minutes) but no view or CLI prints them. | Not wired. |
| The end-of-run notice says nothing about the outcome. | `workflow research finished: 21 sessions, 7619k tokens` (`src/app/workflows.rs:1032`). | Same. |
| CLI `status` is the same flat table followed by the raw result dump. | `src/workflows/cli.rs:598-647`. | Same. |

### Loops

| Symptom | Evidence | Root cause |
| --- | --- | --- |
| Ten columns per run; the one sentence that explains the run is truncated. | Runs table header (`src/app/loops_view.rs:399-414`); the `summary` line is cut at the pane edge in the render above. | Table-first layout; detail is spliced under the row instead of a pane. |
| The outcome word says what the loop did, not what the user must do. | `escalated`, `report-only`, `no-op`, `fix-proposed`, `blocked`, `failed` (`src/loops/mod.rs:71-114`). Only the Inbox tab turns two of them into "waiting on you". | Vocabulary designed for the store, reused for the screen. |
| Why the outcome was chosen is not shown. | `derive_outcome` (`src/loops/run.rs:246-274`) may override the model's block; the verifier's bullets are discarded (`store::parse_verdict` keeps only the word); "High Priority grew" is never displayed. | Derivation inputs are not persisted as such. |
| The actual product of a report-only run, the rewritten state file, is not visible. | The user must open `STATE.md` in the workspace. | Nothing diffs or shows the state file. |
| Stored facts never rendered. | `detail.final_message` (2000 chars), `detail.exit_code`, `detail.timed_out`, per-run `readiness_score`, the launch's trace. | Written at `src/app/loops.rs:1556-1572`, read nowhere. |
| The run log line has no summary or cost. | `loop-run-log.md` entries carry counters only. | `runlog::Entry` omits them. |
| The sidebar's run history is one string. | `Recent: 09-17 17:37 escalated 417k │ 09-16 12:48 escalated 289k …`. | Compactness over readability. |

## Principles

1. **Verdict before ledger.** The first screen answers: did it work, what did it
   find, what needs me, what did it cost. The ledger is one key away.
2. **Show the structure the user wrote.** A workflow is phases and steps; a
   loop is a timeline of runs. Render those, not session labels.
3. **Every null and every override carries its reason.** A dropped item, a
   null answer, a capped level, a derived outcome: the reason sits on the
   same row.
4. **Everything on screen is navigable.** A step opens its result; a session
   opens its transcript or trace; a run opens its files.
5. **Same shape everywhere.** The view, the sidebar card, the notice and the
   CLI print the same headline, so the user learns it once.

## The shared result model

Both features compute one **headline** in Rust and hand it to every surface.

```rust
pub struct Headline {
    pub status: Status,          // ok | attention | failed | running
    pub verdict: String,         // one sentence, ≤ 80 chars
    pub counts: Vec<(String, u64)>, // "found 8", "need you 2", "answered 20/21"
    pub tokens: u64,
    pub cost: Cost,              // Priced(usd) | Unpriced(harness)
    pub duration: Duration,
    pub next_action: Option<String>, // "[a] apply · [x] reject", "read the report"
}
```

Verdict rules:

- Workflow finished: the output step's first Markdown heading or first line,
  truncated; else "finished, N/M sessions answered".
- Workflow failed / cancelled / budget: the error or "stopped at the token
  budget after N sessions; partial result kept".
- Loop: `detail.summary` when present; else the derived outcome in words.

Cost rule: `Unpriced` renders as `unpriced (agy)` in dim text, never `$0.00`.

Number rule: tokens as `143k`, `1.2M`; durations as `41s`, `3m 12s`, `19 min`;
timestamps as `Sep 17 17:37` with the RFC 3339 form on the detail line.

## Workflows view

### Tabs

`Report | Steps | Result | Document`. Journal is folded into Steps (a
session row is a journal entry). The title keeps the scroll position. The
Report tab is specified in "Workflow results: every run is a report"
below; the Summary layout here is its skeleton.

### Summary tab

```text
┌ Summary | Steps | Result | Document · research · 3afc71e9 ─────────────────────────┐
│ ✓ finished   The Skill Creator Feature in agent-mux                                 │
│   21 sessions · 20 answered · 1 dropped · 7.6M tokens · unpriced (agy) · 7 min      │
│   Sep 17 14:31 → 14:38 · Antigravity · /Users/sifilho/workspace/agent-mux           │
│ ─────────────────────────────────────────────────────────────────────────────────── │
│ Phase        Step        Sessions            Tokens   Notes                          │
│ Sweep        sweep       ████ 4/4             3.2M                                   │
│ Read         read        ██████████████░ 14/15 3.5M    9 duplicates dropped,         │
│                                                        read[14] retried once         │
│ Synthesize   answer      █ 1/1                 64k                                   │
│ Critique     critique    █ 1/1                887k    confidence high · 3 missing    │
│                                                                                      │
│ Also answered (not the output)                                                       │
│   critique   missing: Inspect tests/config_library.rs…; Examine src/app.rs (4665…)   │
│                                                                                      │
│ Notes                                                                                │
│   · read: dropped 9 duplicate item(s)                                                │
│   · read[14]: retrying after a schema mismatch                                       │
│                                                                                      │
│ [Tab] steps  [Enter] read the result  [r] resume  [s] save  [o] open run folder      │
└──────────────────────────────────────────────────────────────────────────────────── ┘
```

The bar is sessions answered over sessions started, per step. "Also
answered" lists every object-valued step that is not `output`, with its
top-level fields on one line, so a critique or a classifier's label is never
lost.

### Steps tab

A foldable tree with a cursor; the lower half of the pane is the detail of
the selected row.

```text
│ ▾ Read · pipeline over areas · 15 items · 14 ok · 1 null · 3.5M · 4m 10s           │
│     ✓ read      TUI — src/app.rs, src/ui.rs…            399k   1m 02s               │
│     ✓ read[1]   Sessions & providers — src/session.rs…  640k   1m 40s               │
│   › ✗ read[5]   Loop Engineering — src/loops/…          250k   58s   null           │
│     ✓ read[6]   Trace store & analysis…                 1.3M   3m 05s               │
│ ▸ Synthesize · single · 1 ok · 64k · 48s                                            │
│ ▾ Critique · single · 1 ok · 887k · 1m 51s                                          │
│ ─────────────────────────────────────────────────────────────────────────────────── │
│ read[5] · null · retried once                                                       │
│ reason   the workflow-result block is not valid JSON: EOF while parsing a value     │
│ item     "Loop Engineering — src/loops/, src/app/loops.rs, …"                      │
│ launch   b1c2… · [t] trace  [Enter] transcript  [y] copy result                     │
```

Per kind:

- **fanout / pipeline**: item rows under the step; `kept`/`dropped (verify)`
  /`dropped (keep)`/`dropped (take)` as a status word.
- **verify**: vote rows under the item: `vote1 refuted · "the branch is
  unreachable"`, and the tally on the item row: `kept · refuted 1/3`.
- **tournament**: `gen1..n` rows, then a bracket block: `round 1: gen1 › gen2
  (judge1.1: "a cites the spec") · gen3 › gen4`, `round 2: gen1 › gen3`,
  `winner gen1`.
- **until**: one row per round: `r1 · 5 new · 5 total`, `r2 · 3 new · 8
  total`, `r3 · 0 new (1 quiet)`.
- **route**: `label "bug" → branch triage-bug`; skipped branches dim.

Keys: `↑↓` move, `←→` fold/unfold, `Enter` opens the session transcript
(`sessions/<n>.last.md`) in the pager overlay, `t` opens the trace browser at
the launch, `y` copies the row's result JSON, `o` opens the run folder in
`$EDITOR`.

### Result tab

Unchanged mechanics (wrap, scroll, position), plus: a one-line header with
the output step and its size, Markdown headings rendered bold, fenced blocks
dimmed, and `Enter` opening the result in the pager overlay or `$EDITOR`.

### Live run

Same Summary layout; the headline reads `▶ running · 12/21 sessions · 3.1M ·
4 min`, each step bar fills as sessions settle, and in-flight sessions show
`running on agy · Enter attaches`.

### Sidebar and notice

Sidebar row: `✓ research · 7 min · 20/21 · 7.6M`. Preview card: the headline
block (three lines) and the phase bars. Notice on finish: `research finished
in 7 min · 20/21 answered · 7.6M tokens · W to read`.

## Workflow results: every run is a report

The same rule as for loops. A workflow run already produces a report; the
view shows the ledger and one raw Markdown file.

### What a run leaves behind today

Every built-in workflow ends in a synthesize step, so `run.result` is
prose. The evidence that prose was written from is structured, validated
against the document's schemas, stored per session in `workflow_steps` and
`journal.jsonl`, and never shown:

| Workflow | Output | Structured evidence in earlier steps |
| --- | --- | --- |
| `research` | `answer` (text) | 4 sweeps → leads; 15 readings `{path, facts[], open_questions[]}`; `critique {confidence, missing[]}` after the answer |
| `understand` | `map` (text) | `areas[]`; 8 `area_notes {area, purpose, entry_points[], depends_on[], notes}` |
| `review-changes` | `report` (text) | 4 dimensions → findings `{file, line, title, why, severity}`; 3 votes `{refuted, reason}` per finding; refuted ones dropped |
| `audit-until-dry` | `confirmed` (array) | `until` rounds of findings; votes per finding |
| `migrate` | `verified` (array) | sites `{path, summary}`; changes `{path, changed, summary}` in worktrees with `changed_files`; one verifier vote each |
| `triage-route` | `wrap` (text) | `label {label, reason}`; the branch that ran |
| `judge-panel` | `synthesis` (text) | 4 attempts; judge answers `{winner, reason}`; the winner |

The real `research` run on agy answered "there is no standalone skill
creator" and its critique rated that answer `confidence: high` with three
things it should have checked. The view shows the answer and hides the
critique. A `review-changes` run shows a report but not the seven findings
that three refuters rejected, nor why.

### The Report tab

The Summary tab described above becomes **Report** and is built from the
document's schemas, not from the workflow's name. Rules, applied to every
step's result and every schema field:

| Shape in the schema or result | Rendered as |
| --- | --- |
| The `output` step is text | Its Markdown outline (headings) as a navigable table of contents with the first paragraph; `Enter` jumps to that heading in the Result tab |
| The `output` step is an array of objects | A table, one row per item, columns from the schema fields |
| A field named `file` or `path`, with an optional `line` | A `file:line` column; `o` opens it in `$EDITOR` at that line, `y` copies it |
| An enum field (`severity`, `confidence`, `label`) | A coloured badge; rows grouped by it, highest first |
| A `verify` step with a boolean field (`refuted`) | A tally column `refuted 0/3` on the kept rows, and a collapsed **Refuted (n)** section with each refuter's `reason` |
| A `changed` boolean with a worktree | A **Changes** section: path, summary, branch, `git diff --stat`, verifier verdict, `[a] apply · [x] reject` as in the loop inbox |
| A `route` step | `label bug — <reason>` and the branch that ran; skipped branches dim |
| A `tournament` step | The winner, then the bracket with each judge's reason |
| An `until` step | `4 rounds · 12 found · 2 quiet` |
| Any object step that is not the output (`critique`, `areas`) | An **Also answered** block with its fields |
| String arrays (`facts`, `missing`, `open_questions`, `entry_points`) | Bulleted, collapsed past five |

A document may pin the layout with an optional hint, otherwise the defaults
above apply:

```toml
[workflow]
report = { table = "confirmed", evidence = ["read", "critique"] }
```

### `review-changes` report

```text
┌ Report | Steps | Result | Document · review-changes · 9c1d2e0f ────────────────────────┐
│ ✓ finished   6 findings kept · 7 refuted · 2 high · 1.9M tokens · $3.10 · 6 min         │
│ Sep 18 10:02 → 10:08 · Claude Code · agent-mux · scope main..HEAD                       │
│ ─────────────────────────────────────────────────────────────────────────────────────── │
│ Findings (6)                                                    votes    dimension       │
│ › HIGH    src/app/loops.rs:1572  final_message cut at 2000 bytes 0/3      correctness    │
│           A run whose narrative exceeds 2000 chars loses its loop-result block…          │
│   HIGH    src/loops/runlog.rs:91  same run_id replaces silently 1/3      correctness    │
│   MEDIUM  src/workflows/interp.rs:1345  vote aggregate never stored 0/3   tests          │
│   MEDIUM  …                                                                              │
│   LOW     …                                                                              │
│                                                                                          │
│ Refuted (7)  ▸ 3 unanimous · 4 by two votes                                              │
│                                                                                          │
│ Report  ▸ 4 headings · Enter reads                                                        │
│                                                                                          │
│ [Enter] why  [o] open file  [y] copy  [→] refuted  [t] trace of this finding             │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

`Enter` on a finding shows its `why`, the three refuters' reasons, which
dimension found it, and the session that found it (`t` opens its trace).
Unfolding **Refuted** shows what the workflow considered and rejected, which
is how the user learns to trust or tune the refuter.

### `research` report

```text
│ ✓ finished   No standalone skill creator; scaffolding lives in the Configuration Library │
│ 21 sessions · 20 answered · 1 dropped · 7.6M tokens · unpriced (agy) · 7 min             │
│ ─────────────────────────────────────────────────────────────────────────────────────── │
│ Answer  (16 KB)                                                                           │
│ › The Answer (Conclusion)                                                                 │
│   Key Architecture and Lifecycle                                                          │
│   Evidence per File                                                                       │
│   Open Questions                                                                          │
│                                                                                           │
│ Critique   confidence HIGH · 3 missing                                                    │
│   · Inspect tests/config_library.rs to document test coverage for Catalog::new_item       │
│   · Examine src/app.rs (4665–4669) to explain just-in-time skill installation             │
│   · Compare against skills/workflow-author to rule out confusion with workflow authoring  │
│                                                                                           │
│ Read (14 of 15 leads · 9 duplicates dropped · 1 null)                                     │
│   src/assets.rs                    6 facts · 0 open questions                             │
│   src/app.rs                       4 facts · 1 open question                              │
│   docs/superpowers/specs/2026-09-15-skills-view-design.md   1 fact (irrelevant)          │
│   …                                                                                       │
│                                                                                           │
│ [Enter] read from this heading  [→] unfold  [o] open file  [t] trace                      │
```

The critique's "missing" list is the most actionable thing the run
produced; it sits directly under the answer with `r` offering to resume
with those three leads added.

### `migrate` report

```text
│ ✓ finished   9 sites · 8 changed · 7 verified · 1 refuted · 1 unchanged                  │
│ ─────────────────────────────────────────────────────────────────────────────────────── │
│ Changes (8)                                          verify    branch                     │
│ › src/tracing/store/query.rs   3 call sites → new_api  ✓        wf/9c1d-transformed[0]    │
│   src/loops/cli.rs             1 call site             ✓        wf/9c1d-transformed[1]    │
│   src/app/loops.rs             2 call sites            ✗ refuted: argument order changed  │
│   …                                                                                       │
│ Unchanged (1)  src/mcp/server.rs — no call sites after all                               │
│                                                                                           │
│ [Enter] diff --stat  [a] mark applied  [x] reject and remove worktree  [o] open file      │
```

Editing workflows end in worktrees the user has to merge; the report is the
inbox for them, with the same `applied` / `rejected` decisions as loop runs.

### Steps tab

Unchanged from the Steps tree above: it is the ledger, one key away.

### Sidebar and notice

Sidebar row: `✓ review-changes · 6 findings (2 high) · 6 min · $3.10`.
Notice: `review-changes finished · 6 findings, 2 high · 7 refuted · $3.10 ·
W to read`. A failed or budget-exhausted run says what was kept: `stopped
at the budget after 12 sessions · 3 findings so far`.

### What Rust has to add

| Change | Why |
| --- | --- |
| `workflows::report::build(doc, run, steps) -> Report { headline, outline, tables, evidence, dropped, changes }` | One builder for the view, the sidebar, the notice and the CLI |
| Schema-shape detection over `[schemas.*]` (path fields, enums, boolean votes, `changed`) and the optional `[workflow] report` hint | Layout from the document, not from the workflow name |
| A Markdown outline parser (headings, first paragraph, byte offsets) | The table of contents and `Enter` jumping into the Result tab |
| Vote tallies and drop reasons persisted (already in Data changes) | The Refuted section and the `0/3` column for stored runs |
| Decisions on workflow worktrees: `workflow_steps.decision` (`applied` / `rejected`) and the same removal rules as loop runs | The Changes section as an inbox |
| `$EDITOR +line file` through the existing editor round trip | `o` on any path |

Neither the documents nor the step skills change; the hint is optional.

## Loops view

### Runs tab

Cards in a timeline, newest first; the selected card's detail opens in the
right half of the pane.

```text
│ Sep 17 17:37   NEEDS YOU   L1   8 found · 2 for you · 417k · $1.18 · 1m 43s        │
│   spec-wave bump (dup of PR #2238) and loop scaffolding both uncommitted >1 day     │
│ Sep 16 12:48   needs you   L1   6 found · 1 for you · 289k · $1.21 · 1m 57s        │
│   …                                                                                 │
│ Sep 16 17:33   reported    L2   1 found · 106k · $0.31 · 31s                        │
│ Sep 16 17:18   quiet       L2   fingerprint unchanged · 98k · 20s                   │
│ Sep 16 09:41   skipped     L2   tokens today at the cap (2.2M of 2.0M)              │
```

Display words for the stored outcomes (the store keeps its values):

| Stored | Shown | Colour |
| --- | --- | --- |
| `escalated` | needs you | yellow |
| `fix-proposed` | fix ready | magenta |
| `report-only` | reported | white |
| `no-op` | quiet | dim |
| `blocked` | skipped · reason | dim yellow |
| `failed` | failed · reason | red |

### Run detail

```text
│ Sep 17 17:37 · daily-triage · needs you · L1 (configured L1)                        │
│ Why       the loop-result block said escalated · 2 High Priority items, was 1       │
│ Verifier  not required at L1                                                        │
│ Changed   STATE.md (+9 −3)   [d] diff                                               │
│ Touched   0 files                                                                   │
│ Last message                                                                        │
│   Two items need a decision: the spec-wave bump duplicates PR #2238 and …           │
│ Run        launch 323ef87c · exit 0 · 1m 43s · readiness 100                        │
│ [t] trace  [Enter] transcript  [d] state diff  [o] open state file                  │
```

"Why" is built from `derive_outcome`'s inputs: the block's outcome, whether
it was overridden and by which rule, the verifier verdict and its bullets,
High Priority growth, gate violations, level capping. Persisting these is
the one loop schema addition (below).

`d` shows the state file diff of that run (`git diff` of the state file
between the run's commit range, or the worktree's diff for L2+). For an
inbox run the detail adds `branch`, `worktree`, `git diff --stat` and the
`[a] apply · [x] reject` line, which is today's Inbox tab content; the Inbox
tab stays as the cross-loop queue.

### Sidebar card

```text
│ Status     ‖ paused · next Sep 18 17:37 · every 1d · L1 report-only                 │
│ Last run   needs you · 8 found · 2 for you · 417k · $1.18 · 1m 43s                   │
│            spec-wave bump (dup of PR #2238) and loop scaffolding uncommitted >1 day  │
│ Week       ▮▮▯▮▮▮▯  5 reported · 2 needs you · 0 failed · 1.6M · $4.30              │
│ Inbox      2 waiting → [E]                                                           │
```

The week strip replaces the `Recent` string: one glyph per run, coloured by
outcome, newest on the right.

## Loop results: every run is a report

This is the part that matters most for `pr-babysitter` and the other
scheduled loops. A loop run already produces a real report; agent-mux just
never shows it.

### What a run leaves behind today

Run `2026-09-17T17:36:53Z` of `pr-babysitter` on findon left three things:

- The `loop-result` block: `8 found · 0 actions · 2 escalations` and one
  sentence of summary. This is all the Runs table shows.
- The state file, rewritten in the shape every loop skill is told to keep
  (`loops/templates/STATE.md`, the `## State file` section of each
  `loops/skills/*/SKILL.md`): `## High Priority`, `## Watch List`,
  `## Recent Noise`, and per item `- [ ] #2238 <title> — <status>` followed
  by `Loop action:` and `Human decision:` lines. For findon that is two
  pull requests needing a decision, six clean ones waiting for a merge, and
  three notes about drafts, auth and a flaky listing.
- `detail.final_message`: a narrative of what the run saw and why it did
  nothing, up to 2000 characters. Stored, read by nothing.

Neither the state file nor the final message reaches the view, the sidebar,
the CLI or the notice. The run log line carries counters only. The state
file is overwritten every run, so the previous run's report is gone.

### The Report tab

`E` opens the Loops view on a new first tab, **Report**, showing the
selected run's report parsed from its state-file snapshot, its final message
and its `loop-result` block. The Runs timeline is the second tab.

```text
┌ Report | Runs | Inbox (2) | Readiness | Budget | Files · pr-babysitter @ findon ──────┐
│ Sep 17 17:36   NEEDS YOU   8 PRs · 2 for you · 6 watching · 417k · $1.18 · 1m 43s      │
│ Since last run: #1919 grew to 12 commits · nothing else moved                           │
│ ────────────────────────────────────────────────────────────────────────────────────── │
│ Needs you (2)                                                                           │
│ › #2238  chore(spec-wave): atualiza arquivos do repo para v0.34.2                       │
│          conflicts · touches .github/workflows/** (denylist) · since Sep 13              │
│          Decide   rebase spec-wave/update-v0.34.2 on develop, or regenerate and close   │
│          Loop did reported only; conflicts and workflow files are human gates           │
│   #1919  chore: back-merge main para develop                                            │
│          blocked: required `verify` never reported · 12 commits, +0/−0 · since Sep 16    │
│          Decide   merge with ruleset bypass, or close if the back-merge is unwanted     │
│                                                                                         │
│ Watching (6)                                                                            │
│   #2237  bump vitest 4.1.10 → 4.1.11        CLEAN · verify green · no review · 7 d idle  │
│   #2182  docs(spec-wave): registro … 2146   CLEAN · verify green · awaiting merge        │
│   …four more, all CLEAN and awaiting a merge; they match auto_merge_allowlist but the   │
│   loop never merges                                                                     │
│                                                                                         │
│ Ignored (3)  ▸ no drafts · gh account cannot see the repo · first listing UNKNOWN        │
│                                                                                         │
│ What the run said  ▸ 4 paragraphs                                                       │
│                                                                                         │
│ [Enter] expand  [o] open PR in browser  [y] copy  [d] diff since last run  [t] trace    │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

Rules:

- **Needs you** is the `## High Priority` section, one card per item: the
  id and title on the first line, the status fragment after the ` — ` on the
  second, then `Decide` (the `Human decision:` line) and `Loop did` (the
  `Loop action:` line). "Since" is the first run that listed the id.
- **Watching** is `## Watch List`, one line per item, collapsed to a count
  past six with the note line kept.
- **Ignored** is `## Recent Noise`, collapsed by default.
- **Since last run** is a diff of item ids and status fragments against
  the previous run's snapshot: `new`, `gone`, `moved High → Watch`,
  `CLEAN → CONFLICTING`. For a 15-minute loop this line is the whole
  point of looking.
- **What the run said** is `detail.final_message` with the `loop-result`
  fence stripped, collapsed to its first paragraph.
- A run with no `loop-result` block (the 17:18 run has null counters) shows
  `counts unknown · the run ended without a loop-result block` instead of
  dashes.
- An L2+ fix run adds a **Change** block above Needs you: the branch, the
  files, `git diff --stat`, the verifier verdict and its bullets, and the
  `[a] apply · [x] reject` line. The Inbox tab keeps the cross-loop queue.
- `o` on an item opens it with `gh pr view --web` / `gh issue view --web`
  when the id is `#n` and `gh` is installed; otherwise it copies the id.

The item grammar is the contract already written in the skills:
`- [ ] <id> — <one line>` then indented `Loop action:` and `Human decision:`.
Items that do not parse render as their raw line, so a skill that drifts
from the shape degrades to plain text instead of disappearing.

### Runs tab for a fifteen-minute loop

`pr-babysitter` may run 288 times a day and most runs are quiet. The
timeline folds consecutive quiet runs so the runs that changed something
stay visible:

```text
│ Sep 17 17:36   NEEDS YOU   8 PRs · 2 for you · 417k · $1.18 · 1m 43s                  │
│                #1919 grew to 12 commits                                                  │
│ Sep 17 14:06 … 17:21   quiet ×13   fingerprint unchanged · 1.1M · $2.60   ▸             │
│ Sep 17 13:51   reported    8 PRs · 2 for you · 289k · $0.91 · 1m 57s                  │
│                #2238 CLEAN → CONFLICTING                                               │
│ Sep 17 09:38 … 13:36   quiet ×16   ▸                                                    │
│ Sep 16 17:33   reported    1 PR · #22 CLEAN → CONFLICTING · 106k · $0.25              │
│ Sep 16 09:41   skipped     tokens today at the cap (2.2M of 2.0M)                     │
```

The second line of a non-quiet card is the "since last run" diff, so the
history reads as a changelog of the queue. `→` unfolds a quiet group.

### Sidebar card and notice

```text
│ Last run   Sep 17 17:36 · needs you · 8 PRs · 2 for you · $1.18                      │
│ Needs you  #2238 conflicts, rebase or regenerate · #1919 blocked, merge with bypass  │
│ Week       ▮▮▮▯▯▯▯▯▮▮▯▯▯  2 needs you · 3 reported · 27 quiet · 1 skipped · $9.40     │
```

Notice when a scheduled run ends with a change: `pr-babysitter findon:
#2238 CLEAN → CONFLICTING · 2 need you · E to read`. Quiet runs post no
notice; the sidebar glyph ticks instead.

### Per-item history

`Enter` on an item in the Report tab opens its timeline across runs:

```text
│ #2238 chore(spec-wave): atualiza arquivos do repo para v0.34.2                          │
│ Sep 13 19:20  first seen · Watch · CLEAN, verify green                                   │
│ Sep 17 13:51  moved to Needs you · CLEAN → CONFLICTING after push                        │
│ Sep 17 17:36  unchanged · still waiting on: rebase on develop or regenerate and close    │
```

This is computed from the per-run snapshots; it is what makes "how long has
this been stuck" answerable without reading five state files.

### What Rust has to add

| Change | Why |
| --- | --- |
| `loops::state::parse(text) -> StateReport { last_run, sections: [Section { name, items: [Item { id, headline, status, loop_action, human_decision, raw }] }], run_log, fingerprint }` | One parser for the contract every skill already follows; used by the view, the CLI and the sidebar |
| Snapshot the state file after every completed run to `<runtime>/loops/<loop_id>/<run_id>.state.md` and store its path in `detail.state_snapshot`; keep 30 days like the run log | History, the "since last run" diff, per-item timelines |
| `detail.delta` `{ new: [id], gone: [id], moved: [(id, from, to)], changed: [(id, before, after)] }` computed at run end from the previous snapshot | The changelog line on every card and in the notice, without re-parsing on render |
| `detail.quiet: bool` when the fingerprint is unchanged (the skill's early exit) | Folding quiet runs |
| The `final_message` is kept whole (today it is cut at 2000 characters) or the cut is moved to the render | The narrative is the report |
| `runlog::Entry` gains `summary`, `cost_usd`, `needs_human: [id]` | The workspace file carries what the sidebar shows |

None of this changes what the skills write. The only ask of a skill is to
keep the shape it is already told to keep.

## Command line

Print the same headline and tree; keep `--json` complete.

```text
$ agent-mux workflow status 3afc71e9
✓ research 3afc71e9 · finished in 7 min · 21 sessions · 20 answered · 7.6M tokens · unpriced (agy)
  The Skill Creator Feature in agent-mux

  Sweep       sweep      4/4      3.2M
  Read        read      14/15     3.5M   9 duplicates dropped · read[14] retried
  Synthesize  answer     1/1       64k
  Critique    critique   1/1      887k   confidence high · 3 missing

  notes: read: dropped 9 duplicate item(s); read[14]: retrying after a schema mismatch
  result: agent-mux workflow status 3afc71e9 --result   (16 KB)
  steps:  agent-mux workflow status 3afc71e9 --steps
```

- `status` prints the report (headline, tables, evidence); `status --result`
  prints only the result text (for piping to a pager or file);
  `status --table` prints the findings or changes table as TSV.
- `status --steps` prints the session tree with reasons; `status --step
  read[5]` prints one session's item, reason and result.
- `runs` gains verdict, duration and humanized tokens per line.
- `loop status` keeps its card; `loop report <id> [--run RUN]` prints the
  parsed report (Needs you, Watching, since last run); `loop runs <id>`
  prints the folded timeline; `loop show <run_id>` prints the run detail;
  `loop inbox` shows the "Why" line; `loop item <id> '#2238'` prints one
  item's history.
- `loop run --now` ends with the run detail block.

## Data changes

| Change | Why | Where |
| --- | --- | --- |
| `workflow_runs.notes TEXT` (JSON array) | Notes are the only narration and are lost on restart | schema V14, `write_workflow_run_row` |
| `workflow_steps.reason TEXT`, `workflow_steps.attempts INTEGER`, `workflow_steps.status TEXT` (`ok/null/dropped-verify/dropped-keep/dropped-take/skipped`) | A null or a drop must carry its reason on the row; the retry currently overwrites the row silently | schema V14, `write_workflow_step_row`, `interp` settle |
| `workflow_steps.role`, `parent`, `index`, `round` | Tree rendering without parsing labels | derived from `SessionKey` at write time |
| Verify tallies, tournament bracket, until rounds as `workflow_steps.result` on synthetic rows (`confirmed[3]/tally`, `best/bracket`, `audit/rounds`) or a `workflow_step_aggregates` table | Aggregates exist only in memory | interp `settle_item`, tournament round, until round |
| Prices for Codex and Antigravity models in `pricing.toml`, and `Cost::Unpriced` when absent | `$0.00` beside millions of tokens reads as a fact | `pricing.rs`, headline |
| `loop_runs.detail.derivation` `{block_outcome, override_rule, high_priority_before/after, verifier_bullets, state_diff_stat}` | The "Why" line | `derive_outcome` returns its inputs; `src/app/loops.rs` stores them |
| `runlog::Entry` gains `summary` and `cost_usd` | The workspace file should carry the sentence and the price | `run::runlog_entry` |

All additive; existing rows render with "reason unknown (recorded before
v…)" where a field is missing.

## Delivery in four slices

1. **Read what is already stored** (no schema change). Headline struct;
   humanized numbers, durations and `unpriced`; Summary tab from
   `workflow_steps` grouped by phase; null reasons and notes read from
   `journal.jsonl` and `result.json`; loop run cards with display words;
   `final_message` and `exit_code` in the loop detail; the Report tab parsing
   the workspace's current state file (no history yet); the workflow Report
   tab from `workflow_steps` results and the journal (outline, tables,
   evidence, Also answered; no Refuted section yet); the Journal tab
   folded into Steps; notice and CLI headline. This slice alone fixes most of
   the table above.
2. **Navigation.** Step cursor and fold state in `WorkflowsViewState`; detail
   pane; `Enter` transcript, `t` trace, `y` copy, `o` folder; loop `d` state
   diff and `Enter` transcript.
3. **Persist the reasons.** Schema V14, aggregates, derivation, run-log
   fields, pricing entries; state-file snapshots, `detail.delta` and
   `detail.quiet`, which turn the loop Report tab into history, the folded
   timeline and per-item timelines; vote tallies and drop reasons, which
   add the Refuted section and worktree decisions to the workflow Report. Views switch from files to rows; live and stored
   runs render identically.
4. **CLI parity and MCP.** `--result`, `--steps`, `--step`, `loop runs`,
   `loop show`; `agent_mux_get_workflow_run` returns the headline and tree
   so Heimdall can brief on runs in the same words.

## Out of scope

A web or desktop viewer; changing the outcome values stored in `loop_runs`;
changing the `workflow-result` and `loop-result` contracts the skills emit.
