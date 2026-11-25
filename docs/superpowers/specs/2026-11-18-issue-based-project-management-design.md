# Projects: every spec-wave flow, in agent-mux — design

Status: proposal, 2026-11-18 (third draft). Phases 1, 2 and 3 are implemented, phase 4 in nine parts (§19 to §31) and the first part of phase 5 (§32), with what each left out named there; the rest of phase 5 is §33 to §36 and phase 6 is §37 and §38. §39 on are what was built after the phases.

**The requirement.** Any repository configured with spec-wave must open in
agent-mux as a **Project** and work there: every flow spec-wave has, each
with a screen of its own, with no change to the repository and with none of
spec-wave's programs running. The team keeps using the same GitHub issues,
board, labels and documents, and a person working in spec-wave's CLI or web
UI sees what agent-mux did as if their own tool had done it.

Three consequences shape the design:

1. **`.spec-wave.json` is the Project's configuration.** agent-mux reads it
   in full and never rewrites it. A repository without it is not a Project
   until the last phase (§16).
2. **Compatibility is the contract, not a mode.** The board's `Etapa`
   field, the labels, the document paths, the branch names, the hub and
   code repositories, the Portuguese section titles: all are spec-wave's,
   byte for byte (§3).
3. **agent-mux is the engine.** No `@spec-wave/cli`, no `spec-wave-agent`,
   no sandbox, no web UI or its DynamoDB tables. A repository whose Actions
   are armed still works (§6.1).
4. **Every harness call is an agent-mux session.** Each place where
   spec-wave starts a model or an agent, the dev-agent fleet included, is
   here a launch through `spawn_traced_full`: recorded in the trace store,
   listed in History, followed live, and counted in tokens and cost against
   its work item (§8, §9).

This is not the spec of 2026-11-17 (`2026-11-17-spec-wave-harness-design.md`),
which makes agent-mux the engine spec-wave's backend calls. The two do not
depend on each other.

Decisions the operator still has to make are in §17. The spec is written on
the recommended answer to each.

## 1. What the code shows today

Read on 2026-11-18 from the checked-out `main` of the spec-wave
repositories and from agent-mux `master` (c0ca979).

### 1.1 spec-wave

Read in `spec-wave-cli`: `CLAUDE.md`, `packages/spec-wave/src/config.mjs`
(the single source of the flow), `src/templates/skill/core.md`,
`src/lib/board-policy.mjs`, the step and block tables of
`src/lib/next-step.mjs`, the frozen names of
`test/artifact-identity.test.mjs`, the command list in `README.md` (37
commands), the skills `bug`, `triage`, `implement`, `fix-pr` and the heads
of `preparar-feature` and `preparar-specs`, `rfc/rfc-multi-repo-por-project.md`
§3–4, and the `.spec-wave.json` of a hub (`spec-wave-ui`) and of a code
repository (`spec-wave-cli`). In `spec-wave-ui`: the headings and two
sections of `rfc/rfc-role-based-workspaces.md`. **Not read:** the
`model-prompt*.md` files, the QA executor and its result schemas
(`protocol/qa-result.v1.json`, `qa-trail-report.v1.json`), the hub's
dispatch workflows.

- **Types.** Initiative, Epic, Feature, Story, Task, Bug, Spike, RFC: a
  label (`[FEATURE]`…), the same prefix in the title, and the board field
  `Work Item Type`. Priority is a label `P0`–`P3` and a board field.
- **Hierarchy and dependencies.** Native sub-issues plus `Parent: #N` in
  the body; `Depende de: #4, #5` in the body plus native *blocked by*.
- **The board.** One Projects v2 board per Project. `Etapa` holds twelve
  stages in one order (Backlog, Triagem, Priorizado, Spec, Plan, Ready,
  Desenvolvimento, Code Review, QA, Homologação, Deploy, Done) and only
  advances; the native `Status` (Todo, In Progress, Done) is the progress
  inside a stage and resets on each advance. Other fields: `Story Points`,
  `Area`, `Release`. `.spec-wave.json` records the board's id and every
  field and option id. A board may still hold a retired stage
  (`📋 Backlog Técnico`).
- **Tracks.** Each type walks part of the order and is born at its own
  stage: a Story and a Task in Ready, a Bug in Triagem (Ready when P0), the
  rest in Backlog. A Task goes Ready → Desenvolvimento → Done. A Bug skips
  Priorizado, Spec, Plan and Homologação. A Spike is moved only by hand.
  Initiative and Epic follow their children.
- **Documents.** `docs/features/<slug>/{spec.md, plan.md, decomposition.md,
  qa-plan.md, dependency-map.json, qa/cenario-<n>/}`,
  `docs/bugs/<slug>/bug.md`, `docs/rfcs/<slug>/decomposition.md`,
  `docs/qa/<milestone-slug>/cycle-<n>/report.{md,json}`. The slug comes
  from the issue title. Each document is published in a pull request of its
  own, from `spec-wave/<n>-<spec|plan|bug|decompose|decompose-apply|qa-plan>`,
  and **the next step reads it from the base branch**: an unmerged document
  blocks the next step (`pr-pending`).
- **Labels.** Trigger labels fire an Action (`spec-wave:spec`, `:plan`,
  `:critique`, `:ready`, `:decompose`, `:decompose-apply`, `:bug`, `:qa`).
  State labels record a gate (`:plan-approved`, `:decompose-ready`,
  `:decomposed`, `:critique-failed`, `:needs-human`, `:risk-accepted`,
  `:bug-approved`, `:qa-ready`, `:qa-approved`, `:triaged`, `:duplicate`,
  `:wont-fix`, `:regression`, `:origin:<qa|uat|support|dev|review|regression>`).
  `spec-wave:model:<alias>` overrides the model for one issue.
  `spec-wave:dev-agent` (and `-b`, `-c`… from `devAgent.queues`) is the
  daemon's queue. `repo:<slug>` names the code repositories a Story
  touches.
- **Two execution modes.** `execution.mode = "actions"` (a label fires a
  workflow that runs the CLI) or `"local"` (`spec-wave run <issue>`: a pure
  function reads labels and documents and names the step; no label is
  applied). A repository variable `SPEC_WAVE_EXECUTION` disarms the
  workflows.
- **Critique.** After a plan, a decomposition draft, a bug document and a
  QA plan a second model returns findings with `severity` `grave` or
  `menor`. A `grave` one sets `critique-failed`; from the second attempt
  `ai.escalationModel` runs; at `ai.maxCritiqueAttempts` (3) the flow sets
  `needs-human` and stops.
- **Decomposition in two steps.** A draft a person reviews (`## Story N`,
  `### Task N.M`, `**Depende de:**`, a marker comment), then an apply that
  creates the issues, already in Ready and linked.
- **Development.** `implement` builds a context for a Feature (pending
  Stories in dependency order), a Story (its Tasks) or a Task and hands it
  to `specKit.command`. Story pull requests are stacked, carry
  `Closes #<story>`, and stay drafts until the last Story. Commits carry
  the trailers `Spec-Wave-Story`, `Spec-Wave-Tasks`, `Spec-Wave-Agent`.
  `merge <feature>` merges the stack in order and moves the board to QA.
- **Bugs.** `bug.md` has six sections checked by title. Triage accepts,
  rejects or marks a duplicate. P2 and P3 need `bug-approved` first.
- **QA.** A plan per Feature (one scenario per acceptance criterion), a
  human gate (`qa-ready`), a local execution with `qa.command` that ends
  green (`qa-approved`, advance), red (one Bug per failed scenario) or
  inconclusive. `qa-lead` runs a whole milestone in cycles.
- **Multi-repo.** One hub repository holds every issue, document and
  milestone (`role` absent or `hub`, with `repositories[]`); code
  repositories (`role: "code"`, with `hub`) hold only code. A Story
  declares its repositories with `repo:<slug>`, gets one pull request per
  repository, each with `Closes <owner>/<hub>#<n>`, and GitHub does not
  close an issue across repositories from that keyword. The workspace is
  sibling folders, one per slug.
- **Who starts an agent.** Seven kinds of call, read for the 2026-11-17
  spec (its §1.1) and unchanged: T1 the daemon's Feature or Bug
  orchestration (`claude -p`, up to 8 rounds); T2 one Story through
  `specKit.command`; T3 documents with read tools (Agent SDK or an
  OpenRouter loop); T4 structured output with no tools (critique, audit);
  T5 QA through `qa.command`, and `docker run` per Feature for a trail; T6
  server-side text calls (refine, estimate, insights); T7 interactive chat,
  design sessions and Agent Studio's custom agents in the sandbox.
- **The fleet** (`spec-wave-ui/rfc/rfc-dev-agent-fleet.md`, RFC-008). The
  Development view is a kanban of the Features in Desenvolvimento with four
  columns: Backlog, WIP, Blocked, Review. A person moves a card only
  Backlog → WIP (the dispatch, choosing the executor: the fleet, or one
  developer's local agent by SCM login) and WIP → Backlog (the
  cancellation); the system moves it to Blocked or Review. A WIP card is
  waiting for a slot, queued, waiting for a named agent, running, or
  running with no signal. The fleet has a limit per product (default 3),
  Bugs go before Features, and a failure below `max_failures` (3) retries
  while the limit, a missing credential or a lost lease blocks with its
  reason. A running card opens the agent's session live, read-only. The
  dispatch state lives in spec-wave's DynamoDB; the tracker holds only the
  stage.
- **The lease** (`spec-wave-agent/src/lease.rs`). One git ref per issue
  under `refs/heads/spec-wave-agent/claims` (configurable), holding
  `{issue, owner, generation, heartbeat}`. Acquire is a non-forced push,
  renew and steal are `--force-with-lease` on the exact sha seen,
  `generation` grows on every acquire or steal, the defaults are a
  heartbeat of 120 s and a TTL of 600 s (at least four heartbeats), and a
  lost lease means killing the executor at once with no checkpoint, release
  or push. The daemon's queue is the label `spec-wave:dev-agent`; it works
  on one branch `agent/issue-<n>` and opens one pull request for the whole
  Feature, with no closing keyword.
- **Roles in the UI.** Product Manager, Tech Leader and Developer each get
  the screens of their part of the flow (backlog, prioritization, planning;
  specification, technical review, technical backlog, development, code
  review, QA, UAT; pending, in progress).
- **Language.** Documents, section titles and `Depende de:` are Brazilian
  Portuguese.

### 1.2 agent-mux

- **The factory** (`src/factory/`) takes one GitHub issue to a draft pull
  request: `work_items` with a claim per `(repo, source, source_ref)`, one
  worktree, an independent reviewer, and `factory::deliver`, the only code
  that pushes (one ref, never the default branch, never `--force`, hooks
  off). It reads GitHub only through `gh`.
- **Workflows** (`src/workflows/`) interpret a TOML document in Rust; a
  `revise` step is work, a review on another harness, and the work sent
  back with the findings.
- **The inbox**, **the Run screen**, **the Markdown viewer** and **the
  brain** exist and are reused.
- **Tracing** records Claude Code, Codex, Antigravity, OpenCode, Muse and
  Cursor sessions; Grok and Pi sessions are not recorded. A launch carries
  free-form `launches.metadata`, which is how a swarm, a flow and a factory
  item are already read back as one tree.
- Stopping a session kills its direct child only (`session.rs`), and a
  session inherits agent-mux's whole environment.
- Nothing knows a parent, a dependency, a stage, a board or a document that
  outlives one item.

## 2. The model

```
GitHub: issues · labels · sub-issues · blocked-by · milestones ·
        the Projects v2 board · pull requests · docs/ on the default branch
   │ read through gh                                   ▲ written by Rust only
   ▼                                                   │
project::snapshot ─ project::next (pure) ─ step ─ flow ─ harness
                                             │
                    project::write (labels, comments, issues, Etapa, Status)
                    project::deliver (branch + pull request)
```

1. **GitHub holds the state.** agent-mux keeps a cache and a journal;
   `project sync` rebuilds everything a screen shows.
2. **Rust decides the step.** `project::next` is a pure function of the
   snapshot, the port of spec-wave's `nextStep`.
3. **A model writes documents and code, nothing else.** Labels, comments,
   new issues, the board and every push are written by Rust from a step's
   validated result.
4. **A document reaches the default branch by pull request**, and the next
   step waits for its merge.
5. **A stage only advances**, and the gates are a person's.

## 3. The contract (`src/project/contract.rs`)

Every string that leaves the process is in one module, as in spec-wave's
`config.mjs`, and a test freezes each as a literal. Phase 1 copies the
byte-exact forms from `test/artifact-identity.test.mjs` (pull request
title and body, the dependency map's JSON, the `Depende de:` line).

| Thing | Form | Written when |
| --- | --- | --- |
| Type | label, title prefix, board field `Work Item Type` | an item is created |
| Priority, Area, Story Points, Release | label (priority) and board fields | only on the operator's word |
| Parent | native sub-issue + `Parent: #N` | create with a parent, apply |
| Dependency | `Depende de: #a, #b` + native *blocked by* | apply, `order --sync` |
| Stage | board `Etapa`, by the option ids in `.spec-wave.json` | every advance |
| Progress | board `Status`, reset to Todo on an advance | task start and done, an advance |
| Gate | the state labels of §1.1 | from a step's result |
| Repositories of a Story | `repo:<slug>` labels | apply, the item screen |
| Document | the paths of §1.1 | by a flow, in a worktree |
| Artifact branch | `spec-wave/<n>-<suffix>` | `project::deliver` |
| Story branch and pull request | spec-wave's names; `Closes #n`, or `Closes <owner>/<hub>#n` from a code repository | `project::deliver` |
| Commit trailers | `Spec-Wave-Story`, `Spec-Wave-Tasks`, `Spec-Wave-Agent: agent-mux` | the build prompt; checked after |
| Decomposition marker, dependency map | spec-wave's | the draft, the apply |
| Comments | spec-wave's headings (the 🔎 critique with its attempt and model, the stage repair note) | from a step's result |

**Reading is wider than writing.** A retired stage, a board column made by
hand, a `spec-wave:*` label agent-mux does not know: each is shown as it
is and never "fixed". An item at an unknown stage is not moved, as
`planStageMove` rules.

**agent-mux adds nothing under `spec-wave:`.** `spec-wave update` treats
an unknown `spec-wave:*` label as an orphan.

## 4. Configuration and reading

### 4.1 `.spec-wave.json`

Found by walking up from the session's folder. Read with
`project::config`, which accepts every shape the CLI's own reader does
(blocks from older versions are normalized on read). Never written: the
CLI serializes it byte-exactly for its `update` diff.

| Block | Use in agent-mux |
| --- | --- |
| `owner`, `repo` | the repository `gh` is pointed at |
| `project.id`, `project.fields` | the board and its field and option ids. A missing id (an old file) is resolved from the board by name and kept in the store, not in the file |
| `role`, `hub`, `repositories[]` | §4.4 |
| `execution.mode` | §6.1 |
| `ai.models`, `ai.model` | the default model of a step when it runs on Claude Code and its flow names none |
| `ai.escalationModel`, `ai.maxCritiqueAttempts` | the critique's second attempt and the limit before `needs-human` |
| `ai.modelAliases` | resolves a `spec-wave:model:<alias>` label on the issue (two such labels: none applies, with a warning) |
| `ai.maxTokens` | not used: a harness has its own limit. A cut document is still refused by validation |
| `specKit.command` | not used: `build` is a flow. `project doctor` says so once |
| `qa.command`, `qa.setup`, `qa.env`, `qa.defaultBugPriority`, `qa.lead` | §10 |
| `devAgent.queues` | the extra queue labels the fleet's `label` source reads (§8.4) |
| `cache.ttlSec` | the snapshot's age before a refresh |

A model id from the file is passed only to a harness that takes it (a
`claude-*` id to Claude Code); on any other harness the step's own model
stands and the Run screen says which was used.

### 4.2 GitHub access (`src/project/github.rs`)

Every call is `gh` (`gh issue …`, `gh pr …`, `gh api`, `gh api graphql`)
through the factory's `Tools`, so tests put a recording `gh` on `PATH`. No
token is read by agent-mux. The board needs the `project` scope on `gh`'s
token (`gh auth refresh --scopes project`); without it the Project opens
read-only on issues alone and every screen says why.

### 4.3 The snapshot

`project::snapshot::load` reads, in a few paginated GraphQL queries and
not one per issue: the board's items with `Etapa`, `Status` and the other
fields; each issue's labels, milestone, sub-issues, *blocked by* and
linked pull requests; the milestones. `doc_state` answers per document
`base`, `pr`, `branch`, `missing`, or `unknown` when the read failed, and
`unknown` never regenerates a document.

An issue with a type label that is not on the board is shown under "not on
the board" with the verb to add it, since spec-wave's UI would not show it
at all.

### 4.4 Hub and code repositories

- Opened in a **hub**: the Project is that repository, its code
  repositories are `repositories[]`.
- Opened in a **code repository**: agent-mux resolves `hub`, looks for its
  checkout among the sibling folders (the workspace convention), and opens
  the hub's Project with this repository selected as a filter. Without a
  local hub checkout the Project still reads (everything is on GitHub) and
  the steps that write documents ask for the hub's folder once
  (`[project.paths]`, never `.spec-wave.json`).
- Pull requests of a Story are discovered **per code repository**, by the
  hub issue named in the pull request's body, title or branch, since
  GitHub's linked-pull-request relation does not cross repositories.
- No Action is needed to move the board. agent-mux moves it itself when a
  pull request opens, is approved or merges (§7.4).

## 5. The next step (`src/project/next.rs`)

`next(item, snapshot) -> Step | Blocked(reason) | Waiting(gate) | Nothing`,
pure and total, with spec-wave's own table and block names so both tools
give the same answer for the same issue.

| Type | When | Step | Then waits for |
| --- | --- | --- | --- |
| Feature | no `spec.md` | `spec` | the document's pull request merged |
| Feature | `spec.md` on base, no `plan.md` | `plan`, then its critique | the merge, then the operator's `validate` |
| Feature | plan corrected by hand | `critique` (the file as it is) | |
| Feature, Bug | the operator approves | `validate` → `plan-approved` / `bug-approved` | |
| Feature, RFC | `plan-approved` (an RFC needs none), not `decomposed` | `decompose` (draft, critique) → `decompose-ready` | the operator's apply |
| Feature, RFC | `decompose-ready` and the operator's yes | `apply` → `decomposed`, Stories and Tasks in Ready | |
| Bug | `[BUG]`, no `bug.md` | `bug` | the merge, then triage |
| Bug | Triagem | `triage` (a person's) | |
| Story, Task, Bug | Ready, dependencies done | `build` (`fix` for a Bug) | review of the pull request |
| Feature | Ready, pending Stories | `build` of each, in order | |
| Feature | QA, no `qa-plan.md` | `qa-plan`, critique → `qa-ready` | the operator's review |
| Feature, Story, Bug | QA, plan `qa-ready` | `qa-run` | |
| any | a pull request with review comments | `fix-pr` | |

`Blocked` carries spec-wave's reasons: `unsupported-type`, `needs-human`,
`critique-failed`, `trigger-pending` (a trigger label is on the issue: a
step is pending elsewhere), `pr-pending` (the document it reads is not
merged), `inconsistent-state`, `stale-checkout`, `needs-confirmation`,
`board-unreachable`, `manual`; plus a dependency not done, a dependency
cycle and a Story with no Tasks.

`agent-mux project next <n>` prints the step and why and runs nothing.
Every screen shows this line before a run starts.

## 6. Steps

### 6.1 Who runs a step

agent-mux's own flows run every step, in both execution modes, and never
apply a trigger label.

- **`execution.mode = "local"`:** nothing else can run the step. This is
  the case the design is built for.
- **`execution.mode = "actions"`:** the workflows are armed, and a person
  who applies a trigger label still gets the Action. agent-mux does not
  collide with it: it blocks on a trigger label that is on the issue
  (`trigger-pending`), and it reads `doc_state` again immediately before
  it publishes, giving up its own document when the artifact branch
  appeared meanwhile. `project doctor` reports the mode and, as the CLI's
  doctor does, a disagreement between the file and the repository
  variable.
- **`[project] engine = "actions"`** (a setting, off by default): for a
  team that wants its Actions to do the document steps, a step is started
  by applying the trigger label, and the screen follows the workflow run
  (`gh run list`, `gh run watch`) in place of a flow. Development, review,
  merge, triage and moves are agent-mux's in either setting.

### 6.2 The flows

Built-in documents under `workflows/project-*.toml`, registered in
`workflows::library::BUILTIN` and editable in the flow builder. A Project
step runs only on a harness agent-mux traces (§9.1). A step
runs headless (`workflows::cli::headless`) in a worktree of the repository
that owns what it writes: the hub for a document, a code repository for
code.

| Step | Flow | Shape | Writes | Result |
| --- | --- | --- | --- | --- |
| `spec` | `project-spec` | `single` | `spec.md` | the path |
| `plan` | `project-plan` | `single`, then `critique` | `plan.md` | the path |
| `critique` | `project-critique` | `single`, read-only | nothing | findings `{severity: grave\|menor, where, why}` |
| `decompose` | `project-decompose` | `single`, then `critique` | `decomposition.md` | the path |
| `bug` | `project-bug` | `single`, then `critique` | `bug.md` | the path |
| `qa-plan` | `project-qa-plan` | `single`, then `critique` | `qa-plan.md` | the path |
| `build`, `fix` | `project-story`, `project-bug-fix` | `revise` | code | the factory's verdict |
| `qa-run` | `project-qa` | `fanout` over scenarios | evidence | `{scenario, verdict: pass\|fail\|blocked, reason}` |
| `fix-pr` | `project-fix-pr` | `single` | one commit per problem | `{comment, commit}` rows |

**The critique behaves as in spec-wave.** It is a second session, on
another harness when one is installed (`harness = "other"`), with a result
schema that admits only `grave` and `menor`; a result outside the schema
fails the step and is never recorded as an approval. A `grave` finding
writes the 🔎 comment (attempt, model), sets `critique-failed`, and stops
for a person. Attempt 2 runs on `ai.escalationModel`. At
`ai.maxCritiqueAttempts` the model is not called: `needs-human` is set and
the flow stops. A clean critique removes `critique-failed`.
`risk-accepted` is the Tech Leader's, from the Technical review screen.

**Validation is Rust** (`project::validate`): the required sections of
each document by title, byte for byte as spec-wave checks them; the
decomposition's grammar; the signs of a cut document.

**Apply is Rust** (`project::apply`): it parses the reviewed draft,
creates each Story and Task, adds it to the board with its type and
`Etapa` Ready, links sub-issues and *blocked by*, writes the `Depende de:`
lines, the `repo:<slug>` labels and the dependency map, and sets
`decomposed`. It runs only on the operator's yes. Each created issue is
journaled by its position in the draft, so an apply that stopped halfway
resumes without a duplicate; an issue that already has sub-issues of the
target type is skipped.

**Delivery** (`project::deliver`) is `factory::deliver` with the branch
name taken from the contract and a document's pull request opened ready
for review while code stays a draft. One ref, never the default branch,
never `--force`, hooks off.

**Context a step reads** (`args`, computed in Rust): the issue and its
comments, the parent chain, the earlier documents from the base branch,
the text of `.github/config/tech_context.yml` and of the issue's
`## Tech Override` section, the repository's brain, and for `build` the
Tasks, the Stories not to touch, a digest of recent code and any pending
dependency.

## 7. Development

### 7.1 A Story, a Task or a Bug is a factory item

`build` and `fix` create a `work_items` row (`source = 'issue'`,
`source_ref` the hub issue) with new columns `flow`, `base_ref` and
`code_repo`. The claim index keeps one item per issue. The item skips the
factory's triage stations: the plan and the Tasks are the analysis.

The run does what spec-wave's protocol asks of an agent, with Rust doing
the board: one Task at a time (`Status` In Progress, then `Etapa` Done),
never two in progress in one Story; commits with the trailers, checked
after the run; the Story's draft pull request; the Story to Code Review
with `Status` Todo. A Bug follows spec-wave's four phases (reproduce, root
cause, smallest fix, regression test), reads `bug.md` as a hypothesis, and
goes to Code Review alone, without its parent Feature.

Review, the revision cycles, `unresolved` and `rejected` are the
factory's; `factory::ci` already turns red checks into a fix item.

### 7.2 A Feature is a stack

`project::order` sorts pending Stories topologically from the dependency
map, the `Depende de:` lines and *blocked by*. A cycle is an error naming
the Stories; a pending Story with no Tasks stops the run and asks for a
decomposition. Stories already in Code Review or later are listed as "do
not touch".

One item per pending Story, each based on the branch of the Story before
it, worked one at a time. Every pull request stays a draft; when the last
Story is delivered all are marked ready together. The Feature advances to
Code Review only when every Story is there.

### 7.3 A Story across repositories

A Story with several `repo:<slug>` labels gets one worktree and one pull
request per repository, all drafts until the Feature's last Story, each
with `Closes <owner>/<hub>#<n>`. Since that keyword closes nothing across
repositories, agent-mux closes the hub issue itself when the Story reaches
Done.

### 7.4 Board moves nobody else makes

Without Actions, three moves have no author, so the snapshot refresh makes
them: a pull request opened for an item moves it to Code Review; approved
or merged moves it to QA; a Feature follows when all its Stories did.
`plan_move` only advances, so a repository whose Actions also move the
board gets the same result once.

### 7.5 Merge

`project merge <feature>` prints the plan (the queue in order, the
retargets, any draft that blocks it) and with the operator's yes does what
spec-wave's `merge` does: for each pull request in order, point its base
at the default branch, merge with a merge commit, move the board to QA,
and delete the branches only at the end. A cross-repository Story is
merged as a block. Running it again resumes. It is the one place agent-mux
merges, and only on a person's confirmation.

## 8. The fleet (`src/project/fleet.rs`)

The dev-agent fleet of RFC-008, with agent-mux as the control plane, the
executor and the live view. There is no SQS queue, no Kubernetes Job and
no DynamoDB record: the dispatch lives in agent-mux's store, the lease
lives in git, and an agent is a traced session on this machine.

### 8.1 The Development kanban

The Development screen (§12) is RFC-008's: every open Feature and Bug in
Desenvolvimento, in four columns.

| Column | Who moves a card there | When |
| --- | --- | --- |
| Backlog | the flow, or the operator (cancel) | the item reaches Desenvolvimento, or comes back from WIP |
| WIP | the operator | the dispatch: `d`, then the executor |
| Blocked | agent-mux | `max_failures` reached, no progress, a credential missing, a dependency not done, a lost lease |
| Review | agent-mux | the pull request is open |

An item with no dispatch row is in Backlog. Moving a card on the board
itself dispatches nothing, as in spec-wave.

**Dispatch.** The picker offers, for any item:

- **The fleet** (default): this machine's slots, shown as `2/3`. The
  harness and model come from the flow, or are chosen in the picker from
  the traced harnesses that are installed.
- **A named agent:** a developer's SCM login. agent-mux on that
  developer's machine, or their `spec-wave-agent`, takes it when it sees
  the request (§8.4). The card waits, and says since when.

**A WIP card** is one of: waiting for a slot · queued · waiting for
`<login>` · running (where: `login@host`, the last heartbeat, Stories done
`k/t`, the round) · running with no signal (the heartbeat is older than the
TTL, so the lease may be stolen). `Enter` on a running card opens its
session live; `x` is the cancellation (WIP → Backlog), which stops the
session, releases the lease and resets the failure count.

**A Blocked card** shows the reason and `b`, back to Backlog. **A Review
card** shows the pull request, and `Enter` opens the session that produced
it, from the trace store.

### 8.2 The dispatcher

`fleet::next_actions` is pure, as the swarm's engine is, and runs on the
TUI's tick and in `agent-mux project fleet run [--once]` for a machine
with no TUI.

- **Slots.** `[project.fleet] max_runs` per Project (default 3, RFC-008's
  D6). Bugs before Features, then by dispatch time. A named agent's work
  does not count, and that agent works one item at a time (D9).
- **One run** of a Feature is the daemon's shape, so a team sees the same
  thing whichever engine ran: the lease is acquired, one worktree on one
  branch `agent/issue-<n>`, the pending Stories in dependency order each
  as a `build` step with a checkpoint commit and its trailers, the board
  written by Rust as in §7.1, and one pull request for the whole Feature,
  with no closing keyword (the merge does not end a Feature: it still has
  QA, Homologação and Deploy ahead). A Bug is one `fix` step. The operator's
  own `project run <feature>` keeps the stack of §7.2; `[project.fleet]
  delivery = "agent" | "pr-stack"` chooses for the fleet.
- **Rounds and failures.** At most `max_rounds` (8) rounds per run; a
  round that changes nothing twice is no progress. A failed run below
  `max_failures` (3) is queued again; at the limit the card is Blocked. A
  missing or refused credential, a failed dependency and a lost lease
  block with their reason and do not count as a failure, as in RFC-008 §7.
- **Outcomes** follow RFC-008's table: success with a pull request →
  Review, slot freed; cancelled → Backlog; no signal past the TTL → a
  failure.

### 8.3 The lease

`fleet::lease` is `spec-wave-agent`'s protocol, unchanged, so the two are
safe on one repository at the same time: the same ref prefix
(`refs/heads/spec-wave-agent/claims`, or the configured one), the same
JSON, a non-forced push to acquire, `--force-with-lease` on the sha seen
to renew or to steal after the TTL, `generation` as the fencing token,
`owner` as `<scm login>@<host>`, a heartbeat every 120 s and a TTL of
600 s.

A lost lease is fencing: every process of the run is killed at once, and
nothing is checkpointed, released or pushed. This needs two changes in
`session.rs` that the 2026-11-17 spec also asks for: a session started in
a process group of its own, and a stop that kills the group.

The lease is also what the rest of a team can see: who holds an item,
since when, and whether it still beats. The Development screen reads every
claim ref on each refresh and shows a run held by another machine as
running, with its owner, even though that machine's dispatch row is not
here.

### 8.4 Where work comes from

Three sources feed one queue; each is on or off in `[project.fleet]`.

| Source | What it is | Default |
| --- | --- | --- |
| `screen` | the Development screen's dispatch and `project dispatch <n>` | on |
| `label` | spec-wave's queue label on an issue (`spec-wave:dev-agent`, and the extra tracks of `devAgent.queues`), added by a login with `triage` or above (the factory's check) | off |
| `request` | a dispatch to a named agent, written as a comment on the issue with a marker, which the named developer's agent-mux reads | on |

`label` is off by default for one reason: a `spec-wave-agent` daemon on
the same label would also start. The lease makes that safe (one wins, the
other reports `skipped`), but each loser is a wasted start. A team that
retired the daemon turns it on.

What does not carry over: spec-wave's own Development screen reads its
DynamoDB dispatch record, so it shows an item agent-mux runs as Backlog,
though its stage, lease, branch and pull request are all there. Until
spec-wave reads the claim refs, the kanban of record for agent-mux's fleet
is agent-mux's.

## 9. Every harness call, traced

### 9.1 The inventory

Each of spec-wave's seven kinds of call (§1.1) has one place here, and
each is a launch through `App::spawn_traced_full`.

| spec-wave | Here | Session |
| --- | --- | --- |
| T1 Feature or Bug orchestration by the daemon | the fleet's run (§8.2) | headless, one per Story or fix |
| T2 one Story through `specKit.command` | steps `build`, `fix` (§7) | headless; the reviewer is a second session |
| T3 documents with read tools | steps `spec`, `plan`, `decompose`, `bug`, `qa-plan` | headless, in a worktree of the hub |
| T4 structured output, no tools | `critique`, `audit --critique` | headless, read-only agent, result checked against a schema |
| T5 QA, and a trail's container per Feature | `qa-run`, `qa-trail` (§10) | headless, one per scenario |
| T6 refine, estimate, insights | flows `project-refine`, `project-estimate`, `project-insights`: one read-only step each, started from the item screen (`R` refine the description, `E` estimate into `Story Points`) and from Home | headless; the answer is shown, and written to the issue only on the operator's yes |
| T7 interactive chat and design sessions | `c` on any item: an interactive session on the chosen harness, with the item's context as its opening message and the `/project` skill installed | interactive, in the item screen's Live tab |
| T7 Agent Studio's custom agents | agent-mux's own agents (`.agent-mux/agents/*.toml`), run on an item with `A` | headless or interactive, as the agent says |

Nothing in a Project calls a model any other way. There is no SDK, no
`fetch` to a provider and no shell template: `specKit.command` and
`qa.command` are read, reported by `project doctor`, and not executed.

**Only traced harnesses run Project work:** Claude Code, Codex,
Antigravity, OpenCode, Muse and Cursor (`project::HARNESSES`, the same set
as `swarm_harnesses`). Grok and Pi are refused in the pickers with the
reason, because their sessions are not recorded and a step that leaves no
trace breaks the requirement. Cursor reports tokens only with its hooks
installed; `project doctor` says so.

### 9.2 What a launch records

`launches.metadata.project` (`project::ProjectLaunch`, with
`from_metadata`), on every launch above:

```json
{ "hub": "owner/repo", "item": 412, "type": "Story", "feature": 360,
  "milestone": "v0.6", "step": "build", "attempt": 1, "role": "work|review|critique|qa|chat",
  "dispatch": "<id>", "executor": "fleet|<login>|operator", "code_repo": "owner/api",
  "run": "<workflow run id>" }
```

With it the existing machinery answers for a Project with no new store of
its own:

- **Trace.** Turns, tool calls, tokens, cost and the files touched, per
  session, as for any session. A Project step is a workflow run, so
  `workflow_runs` and `workflow_steps` hold its tree, and its output is
  kept under `<run>/output/`.
- **One item, one trace.** `tracing::project_report` reads every launch of
  an item, and of a Feature with all its Stories, as one tree in time
  order (spec, critique, plan, …, each build and its review, QA), in the
  shape of `tracing::swarm_report`. `agent-mux trace project <n>
  [--tree|--timeline|--log|--json]` and the MCP tool
  `agent_mux_get_project_item` answer from it.
- **History.** These sessions are in the History list with a badge naming
  the item (`#412 build`), filterable by Project, and `Enter` resumes one
  as any other. A headless run is resumed interactively to ask it what it
  did.
- **Live.** A running session's terminal is the Live tab of its row (item
  screen, Development, the Run screen), through `ui::draw_session_pane`;
  a finished one is read back from its kept output and its trace.
- **Guards.** A Project launch runs with the loop guard's rules already
  used by flows: no push, no merge, the path guard for a step that writes
  one document, and the per-item budget (`[project] budget_tokens`). The
  hooks that enforce them are per launch on Claude Code and Codex, the
  plugin on OpenCode, and the installed hooks on Cursor.
- **Environment.** A fleet run does not inherit agent-mux's whole
  environment: an allowlist plus the harness's own credentials
  (`project::fleet::env`), the policy of the 2026-11-17 spec's §6.1.

### 9.3 Where it is shown

- **Cards.** Every item card shows tokens, cost, sessions and wall time,
  its own and rolled up over its children; a milestone's row sums its
  Features. The numbers come from `query` over the metadata, never from a
  comment on the issue.
- **Observability screen** (§12): per Project, the runs going, the last
  failures with their reason, cost by step kind, by harness and model, by
  milestone and by executor; critique attempts per document; time from
  Ready to Review.
- **Heimdall.** The startup dossier gains a `projects` section
  (`ProjectCard`: items moved, steps run, gates waiting, failures, cost in
  the last 24 hours), and `agent_mux_get_briefing` answers the same.
- **Langfuse.** One Langfuse session per work item (`session.id =
  <hub>#<n>`, tags `project`, `project:<step>`, `milestone:<m>`), and a
  summary trace when an item reaches Review (`map::project_summary`), as
  swarms have.
- **Web explorer.** `/api/project/` and its pages (phase 6), read-only.
- **ai-memory.** Project runs are marked suppressed in the feed, as loop
  and workflow runs are; the interactive `c` sessions are relayed as any
  session.

## 10. Bugs and QA

**Triage** has spec-wave's three outcomes and gates: `accept` (→ Ready,
`triaged`; `--severity` replaces the priority label), `reject` (`--reason`
required; closed with `wont-fix`), `duplicate` (`--of` required; a comment
on both issues). P2 and P3 need `bug-approved`; `critique-failed` and
`needs-human` refuse any severity. A reject or a duplicate never touches
the stage. A new Bug records its origin (`origin:<…>`) and its parent.

**QA of an item.** `qa-run` needs the owning Feature's plan `qa-ready`
and the item in QA; it never promotes. It executes the Story's scenarios,
or a Bug's `Teste de Regressão`, with the `qa-analyst` agent in a worktree
at the merged commit, after `qa.setup` and with `qa.env`. Green:
`qa-approved`, Story to Homologação (a Bug to Deploy), the Feature when
all its Stories passed; an open child Bug holds a Story even when green.
Red: one Bug per failed scenario, child of the Story, in Ready, with a
`bug.md` written from the scenario's result by Rust. Blocked with no
failure: nothing moves. Evidence goes to `docs/features/<slug>/qa/cenario-<n>/`.
`--only <n>` retests one scenario and comments on the existing Bug.

**QA of a milestone** (spec-wave's `qa-lead`): `plan` starts the missing
QA plans and stops at the human gate; `run` starts only with the whole
trail `qa-ready`, runs up to `qa.lead.maxParallel` Features at once, each
in its own worktree, and writes `docs/qa/<slug>/cycle-<n>/report.{md,json}`
and the delimited block in the milestone's description. A new cycle starts
only when a Bug closed or a block fell, at most `qa.lead.maxCycles`.
spec-wave runs each Feature in a container (`qa.lead.container.image`);
agent-mux uses worktrees, and that difference is the operator's to accept
(§17).

**Homologação and Deploy** have no automation in spec-wave either. They
are moves a person makes, from the UAT and release screen.

## 11. Every flow, and where it lives

The 37 commands of the CLI, each with its place in agent-mux. A row marked
*n/a* is the installation of spec-wave itself.

| spec-wave | agent-mux | Screen (§12) |
| --- | --- | --- |
| `info`, `doctor` | `project doctor` | Health |
| `mode` | shown; changed only with the CLI (it writes `.spec-wave.json`) | Health |
| `refresh` | not needed: ids missing from the file are resolved into the store | Health |
| `repo link\|unlink\|list` | `list` shown; link and unlink only with the CLI | Health |
| `issue`, `initiative`, `feature` | `project new` | Backlog |
| `bug` | `project new --type bug` | Bugs |
| `triage` | `project triage` | Bugs |
| `generate-spec` | step `spec` | Specification |
| `generate-plan`, `critique`, `validate` | steps `plan`, `critique`, `validate` | Technical review |
| `decompose`, `decompose --apply` | steps `decompose`, `apply` | Decomposition |
| `generate-bug` | step `bug` | Bugs |
| `generate-qa-plan`, `qa`, `qa-lead` | steps `qa-plan`, `qa-run`; `project qa-trail` | QA |
| `run` | `project run`, `project next` | every screen's `r` |
| `implement` | steps `build`, `fix` | Development |
| `order` | `project order` | Ready |
| `task start\|done`, `story review` | done by the build run; by hand with `project task`, `project story` | Development |
| `merge` | `project merge` | Code review |
| `workspace prepare\|map` | `project workspace` (clones or maps the declared repositories as siblings) | Development |
| `pr open` | `project::deliver` | Code review |
| `move` | `project move` | Board |
| `repair-stage` | `project repair-stage --yes --reason` (may move back; leaves spec-wave's note on the issue) | Board |
| `code-review`, `qa --pr-number` (the Actions' movers) | the refresh's moves (§7.4) | Board |
| `preflight`, `audit` | `project preflight`, `project audit` | Milestones |
| `dev-agent` | the fleet (§8): `project dispatch`, `project fleet run` | Development |
| `init` | `project init` (§38): the board, the labels and the file, no workflow files | |
| *(none: an issue is edited on GitHub)* | `project edit` (§39): title, priority, milestone, description | every screen (`e`) |
| `update`, `install-skill`, `uninstall` | *n/a*: spec-wave's CLI | |

Skills not in the table by name: `preparar-specs` and `preparar-feature`
are guided rounds (a milestone's specs; one Feature from spec to Ready).
Here they are the Milestones screen's "prepare" and the Feature's own row,
which already stops at each gate. `fix-pr` is the step of that name.
`rfc` is `project new --type rfc`. `workflow` is the `/project` skill.

## 12. Screens (`Mode::Project`)

F1 → "Open project" (`ActionId::Project`), on the repository of the
selected session; unavailable, with the reason, where no `.spec-wave.json`
is found. The layout is the Library's and the Swarm's: a navigation column
at `ui::SIDEBAR_WIDTH`, the selected screen's list, and the selected row's
pane with tabs. The verbs go through the action registry, and every screen
shares `/` filter, `Ctrl+R` reload, `r` run the next step (the screen
shows `project next`'s line and asks), `i` open the item, `Enter` open.
A filter bar holds milestone, repository, type and "mine".

Navigation, one entry per flow, each with a count of what waits there:

| Screen | Lists | Pane and verbs |
| --- | --- | --- |
| **Home** | what needs the operator, by gate; runs going; health warnings | `Enter` goes to the item on its own screen |
| **Board** | the twelve stages as columns, items as cards (type glyph, number, priority, `Status`) | `m` move (later stages only), `M` repair with a reason, a retired or hand-made column shown apart |
| **Backlog** | Initiative → Epic → Feature tree, items in Backlog and Priorizado | `n` new (type, title, parent, priority, area), `p` priority, `l` milestone, `m` to Priorizado |
| **Specification** | Features in Priorizado and Spec | tabs Card · Spec (Markdown viewer) · Pull request · Runs; `r` generate, `e` edit the file, `o` the pull request |
| **Technical review** | Features in Plan | tabs Plan · Findings (each `grave` or `menor`, with its attempt) · Spec · Runs; `r` plan, `c` re-critique as it is, `k` accept a risk, `y` validate |
| **Decomposition** | Features and RFCs `plan-approved`, drafts, `decompose-ready` | tabs Draft (parsed: Stories, Tasks, dependencies, repositories) · Findings · File; `r` draft or re-critique, `e` edit, `a` apply after a preview of every issue it will create |
| **Ready** | Stories and Tasks in Ready, grouped by milestone and Feature, in execution order | tabs Order (the map, cycles, out-of-order and milestone warnings) · Dependencies · Card; `s` sync the dependency map, `r` build |
| **Development** | RFC-008's kanban of the Features and Bugs in Desenvolvimento: Backlog · WIP · Blocked · Review, with the fleet's occupancy (`2/3 fleet · 1 named`) and runs held by other machines (§8) | tabs Stack (Stories, Tasks, rounds) · Live (the session's terminal) · Trace · Changes; `d` dispatch (the executor picker), `x` cancel to Backlog, `b` a Blocked card back to Backlog, `Enter` the session live, `w` prepare the workspace |
| **Code review** | items in Code Review with their pull requests per repository, checks and review state | tabs Pull request · Review comments · Merge plan; `f` fix-pr, `M` merge the Feature after the plan |
| **Bugs** | Triagem first, then by stage, with origin and severity | tabs Card · bug.md · Findings · Thread; `n` new, `r` bug.md, `y` validate, `t` accept, `R` reject, `D` duplicate |
| **QA** | Features in QA with plan state; scenarios per Story; the milestone's cycles | tabs Plan · Scenarios (verdict, evidence) · Bugs opened · Cycles; `r` plan, `y` mark reviewed, `q` run, `Q` run the trail |
| **UAT and release** | items in Homologação and Deploy, by milestone | `m` to Deploy, `m` to Done; the milestone's release block |
| **Milestones** | milestones with progress, due date and state of each Feature's documents | tabs Features · Preflight · Audit · QA trail; `P` preflight, `A` audit (`--critique` asks first), `g` generate the missing specs |
| **Observability** | the Project's sessions: going now, failed, by step, harness, model, milestone and executor (§9.3) | tabs Runs · Cost · Failures · Critiques; `Enter` the Run screen or the session's trace, `h` the same filter in History |
| **Health** | `project doctor`: `gh` scopes, the file against the board, armed Actions, labels, board hygiene, skills, each code repository's checkout | read-only; each line names its fix |

**The item screen** (`i` on any row, `Mode::ProjectItem`) is the same for
every type: Card (type, stage and track, priority, parent chain,
dependencies with their state, repositories, tokens and cost, the next
step and why), Documents, Children, Pull requests, Runs, Live, Trace (the
item's one tree, §9.2), Thread. `c` opens a chat on the item, `A` runs an
agent on it, `R` and `E` refine and estimate (§9.1). It follows the session
run screen's conventions.

**Roles.** spec-wave's three roles are presets of the navigation, not
permissions: Product Manager (Home, Backlog, Board, Milestones, UAT and
release), Tech Leader (all), Developer (Home, Ready, Development, Code
review, Bugs). `[project] role` chooses; the default shows all.

**The inbox** gets a row for every gate (`InboxItem::Project`): a
document's pull request to review, a plan to validate, a decomposition to
apply, a QA plan to review, a Bug to triage, a `needs-human`, a merge
plan. `App.project_needs` counts them for the Agents sidebar and the
status bar.

The Project screens start nothing in-process: like the Factory screen
they spawn `agent-mux project …` (`intake::spawn_cli`), and a run is
followed on the existing Run screen (`run_screen::Origin::Project`).
The fleet's dispatcher is the exception: like the swarm's engine it runs
on the TUI's tick, and in `project fleet run` where there is no TUI.

## 13. CLI, skill and settings

```
agent-mux project doctor | sync | ls | show <n> | next <n>
agent-mux project run <n> [--step <name>] [--apply] [--yes]
agent-mux project new --type T --title "…" [--parent N] [--priority P] [--area A] [--body …]
agent-mux project move <n> <stage> [--status S]
agent-mux project repair-stage <n,…> <stage> --yes --reason "…"
agent-mux project task start|done <n>      agent-mux project story review <n>
agent-mux project triage accept|reject|duplicate <n> [--severity P] [--reason …] [--of N]
agent-mux project accept-risk <n> --finding "…"
agent-mux project order [<feature>] [--milestone M] [--sync] [--json]
agent-mux project merge <feature> [--yes] [--keep-branches]
agent-mux project qa <n> [--only a,b] [--severity P] [--dry-run]
agent-mux project qa-trail plan|run|report <milestone> [--only …] [--max-cycles N]
agent-mux project preflight|audit --milestone M [--critique] [--json]
agent-mux project workspace prepare|map --issue <n>
agent-mux project dispatch <n> [--to fleet|<login>] [--harness H]   agent-mux project cancel <n>
agent-mux project fleet status | run [--once]
agent-mux project refine|estimate <n> [--yes]
agent-mux trace project <n> [--tree|--timeline|--log|--json]
```

`project new` gives each type its birth stage (a Bug in Triagem, or Ready
when P0; a Story or Task in Ready and only with `--parent`), adds the item
to the board and sets its fields.

**Skills.** The model prompts become step skills declaring the six traced
harnesses: `wf-project-spec`, `-plan`, `-critique`, `-decompose`, `-bug`,
`-qa-plan`, `-qa-run`, `-fix-pr`; `build` reuses `wf-factory-implement`
and `wf-factory-review` with the Story's context. One visible skill,
`skills/project` (`/project`), replaces spec-wave's agent-facing skills in
an interactive session: it drives `agent-mux project …`, shows
`project next` before any run, and keeps spec-wave's rules (never write
`spec.md`, `plan.md` or `bug.md` by hand; never create a Story or Task
outside a decomposition; never `gh issue create`). Agents:
`spec-writer`, `tech-lead`, `qa-analyst`; the critic is `skeptic`; build
and review are the factory's.

A repository that has spec-wave's own skill installed keeps it. The two do
not conflict: that skill calls `npx @spec-wave/cli`, and both write the
same contract.

**MCP** (read-only): `agent_mux_project_list`, `agent_mux_project_item`,
`agent_mux_project_next`, `agent_mux_get_project_item` (the item's trace
tree).

```toml
[project]
engine = "own"          # or "actions" (§6.1)
role = "all"            # or "pm", "tl", "dev"
poll_interval_s = 300
budget_tokens = 0        # per item; 0 is none

[project.fleet]
max_runs = 3            # slots of this machine
max_rounds = 8
max_failures = 3
heartbeat_s = 120
lease_ttl_s = 600       # at least four heartbeats
lease_ref_prefix = "refs/heads/spec-wave-agent/claims"
delivery = "agent"      # or "pr-stack"
sources = ["screen", "request"]   # add "label" to read spec-wave:dev-agent

[project.paths]         # machine paths, never in .spec-wave.json
"astratech-net-br/spec-wave-ui" = "~/workspace/spec-wave/spec-wave-ui"
```

## 14. Store (schema v30)

One migration. The 2026-11-17 spec also names v30 for `exec_runs`;
whichever lands second takes v31.

```sql
CREATE TABLE project_items (          -- a cache of GitHub, rebuilt by sync
  hub TEXT NOT NULL, number INTEGER NOT NULL,
  type TEXT NOT NULL, title TEXT NOT NULL, state TEXT NOT NULL,
  labels TEXT NOT NULL, parent INTEGER, milestone TEXT, priority TEXT,
  depends_on TEXT NOT NULL, repos TEXT NOT NULL,       -- JSON arrays
  stage TEXT, status TEXT, board_item TEXT,            -- board_item: the Projects v2 item id
  fields TEXT NOT NULL, docs TEXT NOT NULL, pulls TEXT NOT NULL,   -- JSON
  github_updated TEXT, synced_ns INTEGER NOT NULL,
  PRIMARY KEY (hub, number)
);
CREATE TABLE project_boards (         -- ids the file lacks, resolved from the board
  hub TEXT PRIMARY KEY, project_id TEXT NOT NULL, fields TEXT NOT NULL, synced_ns INTEGER NOT NULL
);
CREATE TABLE project_steps (          -- the journal: what agent-mux ran
  rid INTEGER PRIMARY KEY AUTOINCREMENT,
  hub TEXT NOT NULL, number INTEGER NOT NULL, step TEXT NOT NULL,
  run_id TEXT, item TEXT, attempt INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL,               -- running|clean|grave|failed|cancelled
  findings TEXT, branch TEXT, pr_number INTEGER, model TEXT,
  started_ns INTEGER NOT NULL, ended_ns INTEGER
);
CREATE TABLE project_dispatches (     -- RFC-008's dispatch record, local to this machine
  id TEXT PRIMARY KEY, hub TEXT NOT NULL, number INTEGER NOT NULL,
  col TEXT NOT NULL,                  -- backlog|wip|blocked|review
  sub TEXT,                           -- waiting|queued|starting|running|no-signal
  executor TEXT NOT NULL,             -- fleet | <scm login>
  source TEXT NOT NULL,               -- screen|label|request
  dispatched_by TEXT, harness TEXT, model TEXT,
  lease_generation INTEGER, heartbeat_ns INTEGER,
  round INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0,
  reason TEXT, branch TEXT, pr_url TEXT,
  created_ns INTEGER NOT NULL, updated_ns INTEGER NOT NULL
);
CREATE UNIQUE INDEX project_dispatches_open ON project_dispatches (hub, number)
  WHERE col IN ('wip','blocked','review');
ALTER TABLE work_items ADD COLUMN flow TEXT;
ALTER TABLE work_items ADD COLUMN base_ref TEXT;
ALTER TABLE work_items ADD COLUMN code_repo TEXT;
```

Sessions need no new table: a Project launch is a `launches` row with
`metadata.project` (§9.2).

The critique's attempt count is the one fact that must survive a deleted
store, so it is read back from the 🔎 comments' headers, where spec-wave
also writes it.

## 15. Living beside spec-wave's own tools

| The repository also has | What happens |
| --- | --- |
| Actions armed | §6.1: agent-mux never applies a trigger label, blocks while one is on the issue, and re-reads the artifact branch before publishing |
| A person using `spec-wave run` locally | the same protection. spec-wave's lock is a file in its own `.git`, which agent-mux cannot share; the artifact branch is the common signal |
| `spec-wave-agent` on a developer's machine | both speak the same lease, so one item has one owner; a run the daemon holds is shown on the Development screen from its claim ref. The `label` source is off by default so the two do not race for every start (§8.4) |
| spec-wave's own Development screen | it reads its DynamoDB dispatch record, so it does not show agent-mux's dispatch; stage, lease, branch and pull request are visible to it as always |
| `spec-wave update` | it rewrites `.spec-wave.json`, workflows and labels. agent-mux wrote none of them, so nothing is lost |
| The web UI | it reads the same board and issues. An item agent-mux created is on the board with its `Etapa`, which is what the UI lists by |
| A newer CLI that adds a stage, a label or a field | shown as it is (§3); `project doctor` names what agent-mux does not know |

## 16. Phases

1. **Open any spec-wave repository, read-only.** `project::{config,
   contract, github, snapshot, stage, next, order}`, the store,
   `project doctor|sync|ls|show|next|order`, hub and code repositories;
   the screens Home, Board, Backlog, Ready, Milestones (Features tab),
   Health and the item screen, with no verb that writes.
2. **Board and items.** `project new|move|repair-stage|task|story|triage`,
   the refresh's moves, the Bugs and UAT screens, the inbox rows.
3. **Documents.** The flows and step skills for `spec`, `plan`,
   `critique`, `decompose`, `bug`; `project::{validate, apply, deliver}`;
   `project run|accept-risk`; the Specification, Technical review and
   Decomposition screens; `engine = "actions"`.
4. **Development, the fleet and review.** `build`, `fix`, the stack,
   cross-repository Stories, `project workspace|merge`, `fix-pr`; the
   lease, the dispatcher, the three sources and process-group fencing; the
   Development kanban and the Code review screen; chat, agents, refine and
   estimate on an item; the `/project` skill and the MCP tools.
5. **QA and milestones.** `qa-plan`, `qa-run`, `qa-trail`, `preflight`,
   `audit`; the QA screen and the rest of Milestones; Heimdall's
   `projects` section and the Langfuse sessions.
6. **Without spec-wave.** `project init` for a repository that never had
   the CLI: the board, the labels and a `.spec-wave.json` in the CLI's own
   shape, so the repository is a spec-wave repository from then on. A
   `/api/project/` area in the web explorer.

Tracing is not a phase. From the first step that starts a harness (phase
3), every launch carries `metadata.project`, and the item's Trace tab,
`trace project`, the History badge and the Observability screen ship with
it.

Phase 1 alone is useful: every spec-wave repository gets its board, its
order map and its "what is next and why" in the terminal.

## 17. Decisions for the operator

1. **Who runs a step where Actions are armed.** Recommended: agent-mux's
   own flows always, with `engine = "actions"` as a setting (§6.1). The
   alternative is to follow `execution.mode` and drive Actions by label by
   default, which keeps spec-wave's CLI in the loop.
2. **`.spec-wave.json` is never written.** Recommended. It costs two
   things: `mode` and `repo link` stay CLI commands until phase 6, and
   machine paths live in agent-mux's own settings.
3. **The critique stops for a person on the first `grave` finding**, as
   in spec-wave. The alternative, an automatic revision before the label,
   would save rounds but make the same issue behave differently in the two
   tools.
4. **QA of a milestone in worktrees, not containers.** Recommended for
   phase 5; a team whose QA needs an isolated environment keeps `qa-lead`
   from the CLI until a container runner exists here.
5. **Porting the prompts.** The `model-prompt*.md` files carry the quality
   of the documents and were not read for this spec. Recommended: port
   them word for word, in Portuguese, changing only how the result is
   returned.
6. **The fleet's shape.** Recommended: one branch and one pull request
   per Feature, as the daemon does (`delivery = "agent"`), so a team sees
   one behaviour whichever engine ran. `pr-stack` gives smaller reviews.
7. **The fleet is this machine.** RFC-008's fleet is Jobs in the
   customer's cluster. Here a slot is a session on the machine that runs
   agent-mux, and several machines share work through the lease, each
   with its own slots. A machine with no TUI runs `project fleet run`.
8. **Grok and Pi are refused for Project work** until they are traced.
9. **Roles are views, not permissions.** GitHub's own permissions decide
   what a person may write.
10. **Name.** `project` (`src/project/`, `agent-mux project`,
   `Mode::Project`). Free in the code today.

## 18. Tests

- **Compatibility fixtures.** Snapshots recorded from two real
  repositories, a hub and a code repository (`spec-wave-ui`,
  `spec-wave-cli`), replayed through a recording `gh`. The test asserts
  that every item gets a stage and a next step, and that reading writes
  nothing.
- **Parity with the CLI.** A table of issues (labels + documents) with the
  answer of spec-wave's `nextStep`, generated once from the CLI and
  frozen; `project::next` must give the same step or the same block name
  for each.
- `contract`: every string of §3 as a literal; the label set equal to the
  CLI's `ALL_LABELS` on the day of the port.
- `config`: every `.spec-wave.json` shape found in the spec-wave
  repositories, including one from before `project.fields` existed, and a
  test that the file's bytes are unchanged after every command.
- `stage`: `plan_move` over every pair; an unknown current stage is never
  moved; `repair-stage` refuses without `--yes` and `--reason`.
- `validate`, `apply`: the decomposition grammar; an apply interrupted
  after each issue, resumed without a duplicate.
- `deliver`, `merge`: against a bare origin in a temporary folder; the
  default branch is never pushed; a draft blocks the merge plan.
- Integration (`tests/it/project/`): a Feature from `spec` to a merged
  stack with fake harnesses, in a hub with one code repository, asserting
  the labels, the board writes and the branches. No test reaches GitHub.
- `fleet::lease`: against a bare origin: acquire, lose the race, renew,
  steal after the TTL, release, and a renew after a steal that must
  report lost. One case reads and writes a claim produced by
  `spec-wave-agent`'s own serializer, frozen as a fixture.
- `fleet::next_actions`: a table test over RFC-008 §7's outcomes, the
  slot limit, Bugs before Features, and the three sources.
- Fencing: a fake harness that starts a grandchild; after a lost lease
  no process of the group is alive and nothing was pushed.
- Tracing: every row of §9.1 started with a fake harness leaves a
  `launches` row with `metadata.project`, and `project_report` returns a
  Feature with its Stories as one tree. A test fails when a Project code
  path starts a process that is not a traced launch.
- Screens: one test per screen of §12 (rows from a fixture snapshot, each
  verb's availability through the action registry).

## 19. Phase 1 as built

Built on 2026-11-18: `src/project/`, `src/app/project.rs`,
`src/ui/project.rs`, `agent-mux project doctor|sync|ls|show|next|order`,
schema v30, `docs/project.md`. Where the code differs from the sections
above:

- **The store** (§14) has two tables, not five: `project_items` (the plain
  columns for queries and a `data` column with the whole item) and
  `project_boards` (the rest of a snapshot, by hub). `project_steps`,
  `project_dispatches` and the `work_items` columns come with the phases
  that write them.
- **The item screen** (§12) is part of `Mode::Project`
  (`ProjectView.item`), not a mode of its own. Its tabs are Card,
  Documents, Children, Pull requests and Thread; Runs, Live and Trace come
  with the phases that start sessions.
- **The pane** of a list row has fewer tabs than §12's table: Card
  everywhere, Order on Ready, Features and Order on Milestones.
- **The stage** (§4.2): there is no `[project] stage` setting. The board
  is read when gh's token may read it; an issue that is not on the board,
  or every issue when the board is not read, shows a derived stage marked
  `~`. No `stage_max` is kept, since nothing is moved yet.
- **`project::next`** blocks a step whose document is `unknown`
  (`unknown-document`), where spec-wave only warns. It has no
  `stale-checkout`: agent-mux reads the default branch on GitHub, not a
  clone. `needs-confirmation` and `board-unreachable` belong to the phase
  that runs steps.
- **The hub of a code repository** (§4.4) is found only beside it, by the
  hub's repository name. `[project.paths]` is not read yet.
- **Not built:** the mouse on these screens, the filter bar's milestone,
  repository and "mine" filters (`/` filters by title and number), roles
  as navigation presets, the inbox rows, the parity table generated from
  spec-wave's CLI and the recorded fixtures of two real repositories
  (§18). The port was checked by hand against a real hub: its 215 issues,
  its board, its retired column and its documents in review.
- **A read** of that hub (145 open issues) takes about 20 seconds, which
  is why the screens open on the cache.

## 20. Phase 2 as built

Built on 2026-11-18: `src/project/{write,ops}.rs`, the verbs in
`agent-mux project`, the Bugs and UAT screens, the dialogs, the inbox
rows. Where the code differs from the sections above:

- **The board's catch-up moves** (§7.4) are not made on a refresh. They
  are computed from the snapshot (`ops::auto_moves`) and made when the
  operator says so: `A` on a screen, after a confirmation that lists
  them, or `agent-mux project advance --yes`. A read never writes.
- **Every change from a screen is confirmed** in a box that says what
  will be written. The CLI verbs write at once, except `advance`.
- **`task` and `story`** are CLI verbs only; on a screen the same moves
  are `m`.
- **The new-item form** has type, title, parent and priority. The area
  and the body are CLI flags.
- **The repair note** is spec-wave's text with `agent-mux project
  repair-stage` and agent-mux's version in place of spec-wave's.
- **A new Story or Task** is born in Ready (§13), where spec-wave's own
  `issue` command puts every type in Backlog and tells the agent to move
  it after.
- **Screens run their changes in-process**, on a thread, and not by
  spawning the CLI as the Factory screen does: the report comes back as
  a notice and the touched issues are merged into the snapshot at once.
- **Not built:** the Specification-and-later screens' verbs, editing an
  item's fields (milestone, priority, area) from a screen, role presets,
  the mouse. Homologação and Deploy moves are `m` on the UAT screen.

## 21. Phase 3, first part, as built

Built on 2026-11-18: the document steps for a Feature's spec and plan and
for a Bug's document, their critique and validation, and the
Specification and Technical review screens (`src/project/{docs,critique,
deliver,run}.rs`, `workflows/project-*.toml`, `workflows/skills/wf-project-*`,
`agent-mux project run`). The prompts were read and ported word for word,
in Portuguese (decision 5 of §17). Where the code differs from §6:

- **A step's session writes no file.** It is one read-only `single` step
  whose result is the document as text; Rust commits it in a temporary
  detached worktree, pushes it and opens the pull request. There is no
  `revise` step and no worktree for the session. This is stricter than
  §6.2: a model cannot leave a file anywhere.
- **The critique is its own flow run** (`project-critique`), after the
  publication, not a second step of the writing flow, so it can also run
  alone on a document corrected by hand (`--step critique`).
- **One session audits through three lenses.** spec-wave runs three
  verifiers, one per lens. Its downgrade of a grave finding whose quote
  is not in the documents is not ported.
- **A spec is not audited when it is generated**, as in spec-wave. The
  `spec` kind of the critique skill exists and is not wired to a step.
- **The critic's harness** is the writer's unless `--critic` names
  another: `harness = "other"` exists only for a `revise` step's review.
- **The model** from `.spec-wave.json` is applied only on Claude Code and
  only for a `claude-*` id.
- **A run is not tagged with its work item** in `launches.metadata`
  (§9.2). It is an ordinary workflow run, traced and listed in the runs
  view, and the item's 🔎 and 📋 comments are what link them today.
- **From a screen, `r` runs on Claude Code**; another harness is a CLI
  flag.
- **Comments** are spec-wave's texts, shortened where they told the
  reader to apply a trigger label. A missing section is reported without
  spec-wave's hint of the heading it was probably renamed to.
- **Not built, the rest of phase 3:** the decomposition (draft, critique,
  apply) and its screen, a Tech Leader's decisions on findings
  (`risk-accepted`), and `engine = "actions"`.

## 22. Phase 3, second part, as built

Built on 2026-11-18: the decomposition (`src/project/{decomposition,apply}.rs`,
the flows `project-decompose` and `project-decompose-rfc`, the skill
`wf-project-decompose`, the `stories` kind of the critique, the
Decomposition screen, `project run --apply`). Where the code differs from
§6:

- **The draft's session answers data, not a file.** It returns Stories and
  Tasks as JSON, as spec-wave's model does, and Rust renders
  `decomposition.md`. A repository the draft names that
  `.spec-wave.json` does not list refuses the publication.
- **The journal is a file**, not a table: `apply-<hub>-<n>.json` under the
  scratch folder, one entry per created issue by its position in the
  draft, removed when the apply is whole.
- **Board failures do not fail the apply.** An issue that could not be
  placed is listed in the comment and in the report, as in spec-wave, and
  `project move` places it.
- **Open `[TODO]` marks** in the spec or the plan refuse the draft with
  an error naming them, where spec-wave also comments on the issue.
- **The apply's pull request** carries the annotated draft and the
  dependency map in one commit.
- **Not built:** a Tech Leader's decisions on findings (`risk-accepted`),
  `engine = "actions"`, and `order --sync`.

## 23. Phase 4, first part, as built

Built on 2026-11-22: the `build` step of one Story and the `fix` step of
one Bug (`src/project/build.rs`, `Ops::build_board`, the flows
`project-story` and `project-bug-fix`, the skills `wf-project-build` and
`wf-project-review`, `project run` on a Story or a triaged Bug), and the
Development and Code review screens. Where the code differs from §7 and
§12:

- **No `work_items` row.** A build is planned, run and delivered by
  `project run` in one process, as the document steps are; it reuses the
  factory's `revise` shape, its verdict rule (`factory::settle`: an
  approval with a failing or missing criterion delivers nothing) and its
  push and pull request code, not its queue. The new columns of §7.1 are
  not added.
- **The board is written twice, not per Task.** One session builds every
  Task of the Story, so Rust cannot see a Task start: the Tasks go to
  Desenvolvimento when the run starts and to Done when it is delivered,
  as spec-wave's `planBoardMoves` does for its own executor.
- **The trailers are guaranteed by Rust.** The session is asked for
  them; a branch without them gets one empty commit that carries them.
- **The branch** is `story/<n>-<slug>` or `fix/<n>-<slug>`. spec-wave
  names none for an operator's build (its executor chooses).
- **A dependency is met when it is merged** (closed, or in QA or later),
  not when it is in review: without the stack of §7.2 a build forks from
  the default branch. `--ignore-dependencies` is the operator's override.
- **An item with no `repo:` label is built in the hub**, as in spec-wave
  (found against a real hub that holds code and lists other
  repositories).
- **The context is spec-wave's, shortened.** The board instructions a
  session cannot follow (it never touches GitHub) are left out, and the
  digest of recent code is not built: the session has the repository.
- **The reviewer's harness** is the flow's `harness = "other"`; `--critic`
  does not reach it.
- **The Development screen is three groups**, not the fleet's kanban of
  §8.1: in development, ready to build, waiting on a dependency. Code
  review groups items by what their pull request waits on.
- **Not built, the rest of phase 4:** see §24, which built the merge,
  `fix-pr` and `metadata.project` and lists what is still left.

## 24. Phase 4, second part, as built

Built on 2026-11-22: `project merge` (`src/project/merge.rs`,
`Ops::merged_board`), the `fix-pr` step (`src/project/fix_pr.rs`, the
flow `project-fix-pr`), and `launches.metadata.project`
(`project::ProjectLaunch`, `App.project_launch`) with
`tracing::project_report` and `agent-mux trace project`. On the screens,
`f` and `P`. Where the code differs from §6, §7.5 and §9.2:

- **A merge takes a Feature, a Story or a Bug.** spec-wave's takes a
  Feature only. Until the stack of §7.2 is built every Story's pull
  request is based on the default branch, so one Story is a useful
  queue of its own.
- **The board after a merge** is §7.4's rule, not spec-wave's `qa`
  command: the Story or Bug goes to QA / Todo, and the Feature follows
  when every one of its Stories is in QA or later. spec-wave moves the
  Feature at the first merged Story.
- **The plan is made from the snapshot.** A Story's pull requests are the
  ones GitHub links to its issue and, in a code repository, the ones
  that name it. spec-wave also reads the bodies of the hub's open pull
  requests for a stacked one GitHub does not link; that read comes with
  the stack.
- **No lock.** spec-wave's `merge` takes a local lock file; here each
  pull request is read again right before it is merged, and one merged
  meanwhile only moves the board.
- **The trailer check is a line in the report**, written after the
  fresh read and before the merge, for a Story only.
- **A draft is never marked ready by agent-mux**, in the plan or on the
  screens: a single Story's draft is marked ready by a person on GitHub.
- **`fix-pr` is a `revise` step, not a `single`** (§6's table): the fixes
  are reviewed by a second session against one criterion per comment
  before anything is pushed, as a build is. It reuses the skills
  `wf-project-build` (kind `fix-pr`) and `wf-project-review`.
- **The session answers no comment.** spec-wave's skill pushes and
  replies itself; here the session reports `{comment, commit, note}`
  rows and `declined` rows, and Rust pushes, replies under each thread
  and leaves the summary. A reported commit that is not on the branch
  is never named to a person.
- **What counts as open:** a review thread that is not resolved and that
  holds no reply with the marker `<!-- agent-mux:fix-pr -->`, and the
  body of a `CHANGES_REQUESTED` or `COMMENTED` review submitted after
  the branch's last commit. At most 30 go to one session.
- **`metadata.project` has no `dispatch`** (there is no fleet yet), and
  `executor` is always `operator`. It is set for the whole process
  (`App.project_launch`), which runs one step, and `role` is read from
  the step's label.
- **`trace project` has `--tree`, `--log` and `--json`**; there is no
  timeline view. The items under a Feature come from the Project's
  cache in the same store; with no cache the item is read alone.
- **Not built, the rest of phase 4:** a Feature as a stack and marking
  its pull requests ready together (§7.2), a Story across repositories
  (§7.3), a Task on its own, `project workspace`, the fleet (§8),
  chat, refine and estimate on an item, the `/project` skill and the
  other MCP tools. (The trace surfaces of §9.2 and §9.3 are §25.)

## 25. Phase 4, third part, as built

Built on 2026-11-23: where an item's trace is shown (§9.2, §9.3), all
over `tracing::project_report`, which gained `runs` (the sessions of a
report grouped by workflow run, `StepRun`), `run_line`, `session_steps`
(the step of every recorded session, by the harness's session id) and
`badge`.

- **The item's Trace tab** (`ITEM_TABS[5]`, `app::project::ItemTrace`,
  `ui::project::trace_tab`): the runs as a list, the selected run's
  sessions under it. `App::project_load_trace` reads the store on the
  spot when the tab opens, on `Ctrl+R` and on the way back from a run;
  it is a local read, so it has no thread of its own. `Enter` opens the
  run on the Run screen (`run_screen::Origin::Project`,
  `App.project_return`, `return_to_project`), as the Factory screen does.
- **History** (`App.history_steps`, `HistoryState.steps`): the badge
  `#<n> <step>` in the sidebar and on the History screen, where `p`
  keeps only the sessions of a step (`HistoryState.project_only`). The
  spec says "filterable by Project"; the filter is every Project's
  steps, not one Project's, since the History list is already scoped to
  a folder.
- **`agent_mux_get_project_item`** (`mcp::server::project_item_result`,
  25 tools now): `item`, optional `hub` and `workspace`; the report plus
  `runs`. An item with no session is an empty trace, not an error.
- **Langfuse** (`map::MapCtx::projects`, learned by `note_launch` and
  seeded for a replay by `seed_swarms`): `session.id = <hub>#<n>`, tags
  `project`, `project:<step>`, `milestone:<m>`, and the launch's fields
  as `project_*` metadata. A swarm's launch keeps the swarm's session.
  `map::project_summary` is the summary trace (a root span and one
  `project-run` span per run; deterministic ids, lower-cased hub).
- **When the summary is sent.** §9.3 says "when an item reaches Review".
  It is sent by `project::cli` after a `build`, a `fix` or a `fix-pr`
  finished (`langfuse::send_project_summary`), delivered or not, since
  the ids are deterministic and a later send updates it; and by
  `agent-mux langfuse export-project <n>` (`langfuse::export_project`),
  which also replays the item's sessions. A document step sends none.
- **Not built from §9.3:** tokens and cost on the cards, the
  Observability screen, Heimdall's `projects` section, the web pages
  and the timeline view of `trace project`.

## 26. Phase 4, fourth part, as built

Built on 2026-11-23: a Feature as a stack (§7.2), a Story across
repositories (§7.3) and a Task on its own (§7.1). `src/project/stack.rs`
is the port of spec-wave's `planFeatureImplementation`,
`featureCanAdvance` and `lib/pr-ready.mjs`; the rest is in
`project::build` and `project::cli`.

- **One run per Story, not one for the Feature.** spec-wave hands the
  whole Feature to one session, which opens the pull requests itself.
  Here `project run <feature>` (`Planned::Stack`, `stack::plan`) loops
  over the pending Stories and each is the build of §23: work, an
  independent review, a delivery by Rust. A stack that stops resumes at
  the Story it stopped on, because the plan is made again from GitHub.
- **The base of a build** (`stack::base_for`): the branch of the nearest
  Story before it, in the Feature's order, with an open pull request in
  the same repository; else the default branch. It is applied to a Story
  run on its own too, so "build Story by Story" and "build the Feature"
  give the same stack.
- **A dependency with an open pull request counts as met**
  (`build::dependency_met`); §23 asked for it to be merged. After the
  base is fetched, `build::check_stack` verifies with `git merge-base
  --is-ancestor` that each such dependency's branch, in this repository,
  is under the base, and refuses the build when one is not.
- **A pending Story with no Task stops the stack** (§7.2); a Story run on
  its own is still built from its description, as in §23.
- **Marking ready** (`stack::mark_ready`, called by `Ctx::build_finish`):
  when every open Story of the Feature is in code review or further on,
  the open pull requests that name the Feature or one of its Stories
  (`stack::collect`, read then from the hub and every code repository by
  their closing lines) and are drafts are marked ready with `gh pr
  ready`. So a Feature with one Story has its pull request marked ready
  at once, as in spec-wave; a Story with no Feature, a Bug and a Task
  stay drafts. `--keep-drafts` skips it. Best effort: a draft that could
  not be marked is a line.
- **The snapshot links the hub's stacked pull requests** by their text
  (`snapshot::build` over the hub's open pull requests), since GitHub
  records its own link only for a pull request based on the default
  branch; `snapshot::merge` keeps such a link across the re-read of one
  issue. This is the read §24 left for the stack, and `project merge`
  now finds the upper pull requests of a stack.
- **A Story across repositories** (`build::targets`, `build::Part`,
  `part_note`): one run per repository, in the order of the labels, the
  next being the first with no pull request of the Story. The board
  moves and the drafts are marked only after the last (`Part::last`).
  The Story's context gains a section naming the session's repository,
  and both step skills say that a Task or criterion wholly of another
  repository passes there. The spec's "one worktree per repository" is
  kept; "worked by one run" is not: no session sees two repositories.
  A Story is `pr-pending` only when each of its repositories has a pull
  request (`decision`, counted by repository).
- **Not built from §7.3:** closing the hub issue when a cross-repository
  Story reaches Done. A Bug or a Task with several labels is refused.
- **A Task on its own** (`BuildKind::Task`, `task_context`): branch
  `task/<n>-<slug>`, a draft pull request closing the Task, the Task to
  Development / In Progress and then Done / Done as spec-wave's
  `planBoardMoves` does for a single Task; its Story and Feature are not
  moved. It runs the flow `project-story` with the skills' `story` kind.
  §7.1 makes it a factory item; it is a Project build like the others.
  A Task whose Story is in code review is refused (`with-story`).
- **Still not built in phase 4:** `project workspace`, the fleet (§8),
  chat, refine and estimate on an item, the `/project` skill and the
  other MCP tools; from §9.3, the cards' tokens and cost, the
  Observability screen and Heimdall's `projects` section.

## 27. Phase 4, fifth part, as built

Built on 2026-11-23: refine and estimate (§9.1 T6), the chat on an item
(T7), the `/project` skill and the read-only MCP tools (§13).

- **A proposal is a file, and the yes is a second command**
  (`src/project/assist.rs`). §9.1 says "the answer is shown, and written
  to the issue only on the operator's yes". `project estimate|refine <n>`
  runs the session and saves a `Proposal` under the scratch folder
  (`<scratch>/proposals/<hub>-<n>-<kind>.json`); `--yes` writes the one
  that waits without running a session again, so what was read is what
  is written; `--yes` with none runs and writes at once; `--discard`
  drops it. The screens show a waiting proposal on the item's card
  (`ProjectView.proposals`, re-read every two seconds while an item is
  open) and `E` / `W` ask for one or ask to write it.
- **`W`, not `R`, refines.** §12 names `R` and `E`; `R` rejects a Bug on
  the same screens since phase 2.
- **Estimate** (flow `project-estimate`, skill `wf-project-estimate`,
  agent `tech-lead`): the value goes to the board's single-select `Story
  Points` field that spec-wave's CLI creates (`Ops::set_story_points`),
  fitted to its numeric options (`assist::scale`, `fit`; a tie goes up).
  spec-wave's server writes a numeric `Estimate` field at once and
  re-estimates when the spec changes; neither is done here. Any item is
  estimated, not only a Feature.
- **Refine** (flow `project-refine`, skill `wf-project-refine`, agent
  `spec-writer`) rewrites the issue's description; spec-wave's refine
  works on `spec.md` and `plan.md`, which here are changed by running
  their step again. `assist::check_refined` refuses an answer that
  changes the dependencies, drops an HTML comment or unchecks a box, and
  `Ctx::apply_proposal` refuses to write over a description that changed
  since (`base_hash`), reading the issue from GitHub at that moment.
- **`project-insights` is not built.** No flow, no Home entry.
- **The chat opens in the Agents list**, not in a Live tab of the item
  screen (`App::project_chat`, `Outcome::Chat`): the Project screen has
  no session pane. It runs on Claude Code when a profile does, else the
  first traced harness with one; there is no harness picker. Its launch
  carries `metadata.project` with `step = "chat"` and `role = "chat"`
  (`App.launch_project`, taken by the spawn as `launch_agent` is), so it
  is in `trace project` and has the History badge.
- **`skills/project`** is a visible built-in skill (`/project`), installed
  for the chat's harness before it starts. It keeps the rules of §13.
- **MCP:** `agent_mux_project_list`, `agent_mux_project_item` and
  `agent_mux_project_next` (`mcp::server`, 28 tools) answer from the
  cache in the trace store and never call GitHub; a Project that was
  never read says to run `project sync`. The hub is matched without
  regard to case.
- **Not built from §9.1:** T7's design sessions and Agent Studio's
  custom agents on an item (`A`).
- **Still not built in phase 4:** `project workspace`, the fleet (§8),
  and from §9.3 the cards' tokens and cost, the Observability screen and
  Heimdall's `projects` section.

## 28. Phase 4, sixth part, as built

Built on 2026-11-23: `project workspace` (§13) and the fleet (§8), on
schema v31 (`project_dispatches`).

- **`project workspace map|prepare --issue <n>`** (`src/project/
  workspace.rs`) is the port of spec-wave's command over
  `build::repo_dir`: the slugs of the item and of everything under it,
  the layout, a clone of what is missing under `.spec-wave/repos` only,
  and the manifest `.spec-wave/workspace-<n>.json` with spec-wave's
  keys. `map` is spec-wave's `--dry-run`. A clone uses the operator's
  git credentials, never a token in the URL, and is never shallow.
- **The lease** (`src/project/lease.rs`) is `spec-wave-agent/src/lease.rs`
  ported rule by rule: a non-forced push to acquire, `--force-with-lease`
  on the sha seen to renew and to steal after 600 s, `generation`, the
  same `lease.json`, the same default prefix. It lives in a small
  repository of its own under the runtime folder, so the operator's
  checkout is not touched. `owner` is `<gh login>@<hostname>`.
- **The rows and the dispatcher** (`src/project/fleet.rs`):
  `project_dispatches` holds one row per dispatched item (`queued`,
  `running`, `blocked`, `review`, `cancelled`); `next_actions` is pure
  (Bugs first, then the oldest dispatch; a row whose process is gone is a
  lost run; an item with an open pull request in code review goes to
  Review whoever built it; a closed item is forgotten) and `settle`
  records how a run ended.
- **The runner is a process, not the TUI's tick.** §8.2 runs the
  dispatcher on the tick and in `project fleet run`; only the command is
  built (`cli::fleet_run`), one per hub and machine (a pid file). The
  screen's `d` starts `project fleet run --once --dispatch <n>`, which
  leaves at once when a runner is already going. A TUI that closes does
  not stop the fleet.
- **A run is `project run <n>`**, a process per item, so the delivery is
  the operator's own: `pr-stack` for a Feature. The daemon's shape of
  §8.2 (`delivery = "agent"`: one branch `agent/issue-<n>`, checkpoint
  commits, one pull request with no closing keyword) is not built, and
  neither are rounds within a run (`max_rounds`): a build's own
  `max_cycles` is the only loop inside one.
- **Outcomes** differ from RFC-008's table in one row: a run that exits
  cleanly with no pull request is Blocked with its last line, not a
  success. A lost lease and a lease that cannot be renewed past its TTL
  block without counting as a failure; a lease another owner holds
  leaves the card queued with `held by <owner>`.
- **Fencing stops the run's process only.** §8.3 asks for a process
  group; `session.rs` was not changed, so a harness started by the
  stopped run may outlive it briefly.
- **The kanban** (`app::project::kanban_rows`): three headings over the
  Development screen's own groups, which stay as the Backlog, instead of
  four columns side by side; `d`, `x` and `b`. A run held by another
  machine is shown by `fleet status` (it lists every claim on the
  remote), not on the screen. `Enter` on a card opens the item, not its
  live session; the card names the run's output file.
- **Sources** (§8.4): only `screen`. The queue label and the request to
  a named agent are not built, so nothing is dispatched without the
  operator.
- **Settings.** `[project.fleet]` is not read: `--max-runs` and
  `--max-failures` on `fleet run` (3 and 3).
- **Left of phase 4:** from §9.3 the cards' tokens and cost, the
  Observability screen and Heimdall's `projects` section; from §9.1
  `project-insights` and agents on an item.

## 29. Phase 4, seventh part, as built

Built on 2026-11-23: what §28 left of the fleet.

- **`[project.fleet]`** (`config::FleetConfig`, `resolve_fleet`):
  `max_runs`, `max_failures`, `delivery`, `label`, `queue_label`,
  `request`. §8.4 turns each source on or off; `screen` has no switch,
  since it is the operator's own key. A flag of `fleet run` wins.
- **The `request` source** (`fleet::request_comment`, `parse_request`,
  `cli::take_requests`): `project dispatch <n> --to <login>` writes a
  comment with the marker `<!-- agent-mux:dispatch to=<login>
  [harness=<h>] -->` and keeps a local row whose `target` is the login
  (shown as "waiting for <login>", never started here). The named
  developer's runner reads every issue comment of the hub since its last
  look in one call (`github::comments_since`; the first look reads one
  day back), takes a request addressed to its `gh` login when the
  comment's author may triage the hub (the factory's check), and keeps
  the ids it took so a comment is taken once. `cancel` writes the marker
  with `cancel`. `spec-wave-agent` does not read the marker: a named
  agent has to run agent-mux.
- **The `label` source** (`fleet::labelled`): open items of a type the
  fleet runs that carry `queue_label` and have no row, each dispatched
  only when the login that added the label may triage the hub
  (`factory::intake::labeler`, `permission`). One label, not the tracks
  of `devAgent.queues`. The label is read from the cached snapshot, so a
  new label is seen when the cache is next refreshed.
- **`delivery = "agent"`** (`build::AgentDelivery`, `as_checkpoint`,
  `Ctx::checkpoint_finish`): `project run <feature> --delivery agent`
  walks the pending Stories as the stack does, and each approved Story
  is pushed to `agent/issue-<n>` as a checkpoint; the Feature's one pull
  request is opened after the last, as a draft, with a body that names
  the Stories and no closing keyword (`agent_pr_body`). Each Story is
  still reviewed by a second session before its checkpoint, which the
  daemon does not do. The branch is the same name the daemon uses, so a
  run that the daemon started is continued, and the lease keeps the two
  from working it at once. Because the pull request closes nothing,
  GitHub links it to no issue: the runner reads its URL from the run's
  output (`cli::opened_pr`) to move the card to Review, and `project
  merge` does not find it. A cross-repository Story is refused.
- **Rounds** (`max_rounds`, "no progress") are not built: a run is one
  pass over the Feature's pending Stories, and a failed run is retried
  from where it stopped.
- **The process group.** The runner starts each run in a group of its
  own and stops the group (`cli::stop_run`: `TERM`, then `KILL`).
  `session.rs` is unchanged: a harness is a child of the run's process
  and ends with its terminal.
- **Not built:** dispatch to a named developer from the Development
  screen, and a run held by another machine shown there.

## 30. Phase 4, eighth part, as built

Built on 2026-11-23: insights (§9.1 T6) and an agent on an item (T7).

- **`project insights [<n>] [--scope …]`** (`src/project/insights.rs`,
  flow `project-insights`, skill `wf-project-insights`, no persona): the
  four scopes of spec-wave's `insightsService.ts` under shorter names
  (`progress`, `tech`, `daily`, `brainstorm`; its own names are taken
  too). `insights::digest` is pure over the cached snapshot: open items
  per stage, the milestones, and one line per open item (at most 120)
  that also carries the item's next step or block, which spec-wave's
  context does not. With an issue number the digest is that item and
  what is under it. The session may read the repository. The answer is
  printed and kept per hub and scope under the scratch folder; `I` on
  any Project screen asks for one or opens the last in the Markdown
  viewer. Its launch is tagged `step = "insights:<scope>"`, with `item =
  0` for a reading of the whole Project, so it is not in any item's
  trace. Nothing is written to GitHub and no issue comes from a
  brainstorm's idea; spec-wave's selectable ideas panel is not built.
- **An agent on an item** (`a`, not §12's `A`, which is the board's
  catch-up since phase 2): `ProjectView.agents` lists the personas of
  the library, the hub's `.agent-mux/agents/` and the built-in ones (an
  agent with a `[task]` is left out: it runs its own task).
  `App::project_agent` starts an interactive session as the one picked,
  applying its launch plan as the New session dialog does
  (`App::session_agent`), with `app::project::agent_opening` as the
  first message and no `/project` skill. §9.1 says "headless or
  interactive, as the agent says"; only interactive is built. The launch
  carries `metadata.agent` and `metadata.project` with `role = "agent"`
  and `step = "agent:<name>"`.
- **Left of phase 4**, all from §9.3: tokens and cost on the cards, the
  Observability screen, Heimdall's `projects` section, and the timeline
  view of `trace project`.

## 31. Phase 4, ninth part, as built

Built on 2026-11-23: the rest of §9.3, all read-only and all over
`tracing::project_report`, which gained `hub_launches`, `hubs_since`,
`Totals`, `item_totals`, `card_line`, `observability_lines` and
`timeline_lines`.

- **Cards.** An item's card ends with a `Sessions:` line: its own
  sessions, tokens, cost and running time, and for an item with children
  the total with what is under it (`item_totals` rolls a session up
  every ancestor the snapshot knows). On the screens it is read on the
  tick, at most every ten seconds (`ProjectView.totals`); `project show`
  reads it once. A milestone's row does not sum its Features yet.
- **The Observability screen** (`Screen::Observability`, before Health):
  a screen with no rows whose pane is `observability_lines`, in two
  tabs, the last 30 days and ever: the totals, the sessions going now,
  the cost by step (an `agent:<name>` or `insights:<scope>` step counts
  under its kind), by harness, by milestone, by executor and by item,
  and the sessions that had tool errors. Not built from §9.3's list:
  cost by model (the report adds up launches, and a model is a turn's),
  critique attempts per document, the last failures with their reason
  (the fleet's Blocked cards carry theirs on the Development screen),
  and the time from Ready to Review.
- **`trace project <n> --timeline`**: one row a session on one time axis
  60 columns wide. It is text, not the Swarm screen's zoomable lanes.
- **Heimdall** (`dossier::ProjectDossier`, `ProjectCard`,
  `project_card`): the `projects` section holds the card of the
  workspace's own Project when the folder is a spec-wave repository;
  §9.3's "items moved" and "gates waiting" are not in it, since they are
  facts of GitHub and the dossier reads none. It carries the fleet's
  counts and the reasons of its Blocked cards instead.
  `agent_mux_get_briefing` answers the same dossier.
- **Phase 4 is built** as §19 to §31 describe, each naming what it left
  out. Phase 5 (QA, milestones' audit and trail) and phase 6 (`project
  init`, the web pages) are not started.

## 32. Phase 5, first part, as built

Built on 2026-11-23: QA of an item (§10), the port of spec-wave's
`commands/qa-run.mjs`, `lib/qa-exec.mjs`, `lib/qa-report.mjs` and the
parser of `lib/qa-plan-doc.mjs`.

- **`src/project/qa_plan.rs`** reads `qa-plan.md` (`parse`, `targets`):
  the marker, the scenarios by position, the Story in each title, the
  four fields, a heading inside a code fence left as body. It does not
  render or validate a plan: writing the plan is the next part.
- **`src/project/qa.rs`** holds the rules as pure functions (`gate`,
  `green_target`, `open_bugs`, `feature_pending`, `regression_section`,
  `validate_results`, `aggregate`, `combine`, `parse_last_report`,
  `render_report`, `render_bug_doc`, `context`) and the two steps on
  `Ctx`: `qa_plan` (reads everything, writes nothing) and `qa_finish`.
  The report's marker, table and `spec-wave:qa-results` block, the
  origin marker of a Bug and the six sections of its `bug.md` are
  spec-wave's, so either tool reads the other's.
- **The verdict travels in the answer.** spec-wave's executor writes a
  result file; here the session ends with a `workflow-result` block
  (flow `project-qa`, skill `wf-project-qa`, agent `qa-analyst`), checked
  by the same rules.
- **Where it runs.** `project::cli::cmd_qa` makes a detached worktree of
  the item's code repository at `origin/<default>` under the scratch
  folder, runs the session there and removes it afterwards, whatever
  happened. spec-wave runs in the operator's checkout. `qa.command` is
  not executed; `qa.setup` and `qa.env` are given to the session, which
  runs the setup itself, and `qa.env` is not put in its environment.
- **A red scenario's `bug.md` goes up by pull request**
  (`deliver::publish`, on `spec-wave/<bug>-bug`), after the Bug is
  created, since the branch carries its number. spec-wave commits the
  file to the default branch before creating the issue. So a Bug opened
  by QA has `spec-wave:bug-approved` while its document still waits for
  a merge, and its fix is planned from the report until then. When the
  Bug cannot be created, its scenario counts as blocked and the run is
  inconclusive, as spec-wave does when its push is refused.
- **The Bug** is created with `write::create_issue` (labels `[BUG]`, the
  severity, `spec-wave:origin:qa`), linked under the Story, placed in
  Ready / Todo (`Ops::place`) and labelled `bug-approved`. It does not
  inherit the Story's milestone, and its `Priority` field on the board
  is not set.
- **`planSha`** is seven hex digits of a SHA-256 of the plan (spec-wave:
  SHA-1). Neither tool compares it across reports; a plan that changed
  between the read and the finish is caught by comparing the two reads.
- **Not built:** visual evidence (`evidenceFiles`, `qa.evidence`), a
  Story tested across several repositories (the first is used, and the
  command says so), and `qa` on the screens beyond the `Q` key.
- **Left of phase 5 (at the time):** the `qa-plan` step (write, publish and audit the
  plan, which is what grants `qa-ready`), the QA of a milestone
  (`qa-trail plan|run|report`), and `preflight` / `audit --milestone`.

## 33. Phase 5, second part, as built

The `qa-plan` step: the port of spec-wave's `generate-qa-plan`
(`commands/generate-qa-plan.mjs`, `lib/qa-plan-doc.mjs`, the prompts of
`plugin/skills/qa/`). It is what grants `spec-wave:qa-ready`, so with it
`project qa` no longer depends on a plan spec-wave's own tools wrote.

- **The step** is `project run <feature> --step qa-plan`
  (`run::Ctx::qa_plan_step`). It is never the automatic next step of an
  item: `next::next_step` is unchanged, and the operator asks for it (the
  command, or `Q` on a Feature without `qa-ready`). It refuses a Story
  (naming its Feature), a Bug and any other type, an issue with
  `needs-human` or `critique-failed`, a Feature whose `spec.md` or
  `plan.md` is not on the default branch, and one with no Story. A
  refusal is an error line and writes nothing.
- **Generation** is the flow `project-qa-plan` (skill
  `wf-project-qa-plan`, spec-wave's `model-prompt.md` in Portuguese;
  agent `qa-analyst`, read-only), given spec-wave's payload: the
  Feature, its Stories with their bodies read from GitHub, the spec and
  the plan. The answer is `{document}`; `qa_plan::parse_answer` reads it
  without asking for the marker, `qa_plan::render` writes the file
  (spec-wave's `renderQaPlanDoc`: title, marker, positional numbers) and
  `qa_plan::validate` (spec-wave's `validateQaPlan`, its messages) checks
  it **before** the publication; spec-wave publishes first and validates
  after. `deliver::publish` then opens the pull request on
  `spec-wave/<n>-qa-plan`.
- **A plan that is there** is validated and audited as it is, never
  generated again: from the default branch, else from the artifact
  branch on the remote (`deliver::read_branch_file`). A validation
  failure stops the step before any session.
- **A dropped Story** is one closed with GitHub's reason `NOT_PLANNED`,
  read at the step (`github::close_reason`; the snapshot does not keep
  the reason). It needs no scenario and is left out of the payload.
- **The audit** is `critique::Kind::Qa` through the same flow
  `project-critique` (`kind = "qa"`, spec-wave's
  `model-prompt.critique.md`), with the same attempt counter, 🔎 comment
  and `needs-human` ceiling as the other documents
  (`<!-- spec-wave:critique kind=qa … -->`, so either tool counts the
  other's attempts). Grave: `critique-failed`, and `qa-ready` is removed
  when present. Clean: `critique-failed` is lifted, `qa-ready` added and
  the 🧪 comment posted with the scenarios per Story.
- **Different from spec-wave:** the trigger label `spec-wave:qa` is
  neither needed nor removed; a refusal or a failed validation leaves no
  comment on the issue; the audit is not given the decomposition or the
  tech context; and a Tech Leader's decisions on findings
  (`risk-accepted`) are not applied, as for every other document.
- **Left of phase 5 (at the time):** the QA of a milestone
  (`qa-trail plan|run|report`), and `preflight` / `audit --milestone`.

## 34. Phase 5, third part, as built

The QA of a milestone: `agent-mux project qa-trail plan|run|report
<milestone>`, the port of spec-wave's `qa-lead` (`commands/qa-lead.mjs`,
`lib/qa-lead.mjs`).

- **The rules** are `project::trail`, pure and ported one by one:
  `resolve_milestone`, `classify_for_plan`, `classify_scope`, `run_gate`,
  `feature_order` (over `order::order_nodes`), `feature_verdict`,
  `scenario_counts`, `aborted_entry`, `build_report`,
  `changed_since_previous`, `termination_of`, `next_cycle_gate`,
  `retest_plan`, `milestone_block`, `upsert_block`, `render_md`. The
  report is `trail::Report`, serialized with spec-wave's keys
  (`qa-trail-report.v1`), frozen by a test.
- **`plan`** does not apply the `spec-wave:qa` trigger: for each Feature
  classified `Generate` it runs `agent-mux project run <n> --step
  qa-plan` as a child process, one at a time, then reads the Project
  again and reports each Feature's state. There is no `--watch`: the
  step is synchronous. A Feature with `spec-wave:qa` on it is reported
  as being written elsewhere and left alone.
- **`run`** is one cycle (`cli::cmd_qa_trail`). Each target is
  `agent-mux project qa <issue> [--only …]` as a process in a group of
  its own (`AGENT_MUX_PROJECT_EXE` names a stand-in in tests), up to
  `qa.lead.maxParallel` at once on threads, stopped as a group after
  `qa.lead.featureTimeoutMin`. What a target did is read as spec-wave
  reads it: the issue's last 🧪 report, counted only when its `run`
  number rose (`qa::LastReport` now also gives the report's verdict and
  the Bugs of its "Bugs abertos" line).
- **No container.** spec-wave's backend (`docker` or `sandbox`,
  `qa.lead.container`) is not ported: a target runs in the temporary
  worktree `project qa` makes. The preflight checks what can abort every
  target alike here: the harness on `PATH` and a gigabyte of free disk.
- **The report is published by pull request**, not pushed to the default
  branch as spec-wave does: `deliver::publish` on
  `spec-wave/<milestone>-qa-trail-c<n>` with `report.md` and
  `report.json` under `docs/qa/<slug>/cycle-<n>/`. Since the next cycle
  must find the last one before that merge, a copy is kept under the
  scratch folder (`trail::keep`, `kept`), and `trail::cycles` reads both
  it and the default branch (`merged`), the kept one winning. The
  pull request's title says "issue <n>" for the milestone's number,
  which `deliver::pr_title` writes for every artifact.
- **The milestone's description** gets spec-wave's delimited block
  (`github::milestone_description`, `write::set_milestone_description`:
  `gh api repos/<hub>/milestones/<n>`, `-X PATCH -f description=…`),
  best effort.
- **One rule is not spec-wave's:** `next_cycle_gate` lets a cycle start
  when the last one had an aborted execution, even with no scenario
  counted as blocked. spec-wave counts the planned scenarios from the
  plan in the checkout; when that count is zero its gate would refuse
  every later cycle.
- **Not built:** the trail on the Milestones screen, `naoExecutado`
  (always 0, as in spec-wave's command), and evidence files.
- **Left of phase 5:** `preflight` and `audit --milestone`.

## 35. Phase 5, fourth part, as built

A milestone before its documents: `agent-mux project preflight
--milestone <M>` and `agent-mux project audit --milestone <M>
[--critique]`, the ports of spec-wave's `preflight` and `audit`
(`commands/preflight.mjs`, `commands/audit.mjs`, `lib/spec-audit.mjs`).
Both read only.

- **The rules** are `project::audit`, pure and ported one by one:
  `inspect_milestone`, `preflight_verdict`; `dependency_section`,
  `dependency_section_is_empty`, `issue_refs`, `mentioned_issues`,
  `untraceable_bullets`, `milestone_after`, `slug_keywords`,
  `code_overlap`, `audit_milestone`, `audit_verdict`. The results are
  serialized with spec-wave's keys (`gerar`, `aguardandoMerge`,
  `grafo`, `inversoes`, …), frozen by tests; the lines a person reads are
  in English, like the rest of the command line. The port was run beside
  spec-wave's own functions on a real hub's specs and gave the same
  bullets and edges.
- **The inventory** is taken from the snapshot, not from a checkout: a
  spec's state is the snapshot's `DocState`, named as spec-wave names it
  (`remote`, `pending-pr`, `branch-only`, `missing`, `unknown`; never
  `local`). A closed Feature is in the snapshot only for
  `github::CLOSED_DAYS` days, so the list of closed ones is short.
- **The preflight's environment** is not spec-wave's. Its two inspectors
  judge GitHub Actions (`describeModeState`, `inspectPrPublishing`:
  secrets, workflow permissions, branch protection); here a document is
  generated and published from this machine. The checks are the
  doctor's `gh` and `execution mode` lines, the harness on `PATH`, the
  hub's checkout, and `github::can_push` (`gh api repos/<o>/<r>`,
  `permissions.push`, probed on 2026-11-23). They are the `ambiente`
  list of the JSON, in place of spec-wave's `modo` and `publicacao`. A
  check that could not be made blocks nothing.
- **The audit's catalog** is the snapshot's Features of every milestone.
  A `#N` it does not know is read from GitHub (`ops::read_items`, one
  query): found, it joins the catalog with its milestone; not found, it
  is an issue that does not exist. A read that fails stops the command,
  where spec-wave reads a failure as "does not exist".
- **A spec is read where it is** (`cli::read_spec`): the default branch,
  or its artifact branch when it waits for its merge
  (`deliver::read_branch_file`). A read that fails is `specIndisponivel`,
  never `semSpec`.
- **The code heuristic** reads the hub's files and those of each code
  repository checked out beside it (`audit::list_repo_files`, bounded as
  spec-wave's; a hit names its repository).
- **`--critique`** is the `project-critique` flow with `kind =
  "conjunto"` (a new section of `wf-project-critique`, ported from
  `plugin/skills/audit/model-prompt.critique.md`), one session over
  `audit::set_document`. The Features a finding crosses travel in its
  `anchor` as `#N` references instead of spec-wave's `features` array,
  so the flow's schema did not change; `audit::set_findings` refuses a
  finding that names none or one outside the set. The comment is
  spec-wave's "Crítica avulsa" format, with no marker, so it counts no
  attempt; it is printed and never posted. The session is tagged
  `metadata.project` with `step = "audit"`, the milestone and item 0.
- **Not ported:** `unownedDecisions` (`semDono`: the
  `decisoes_de_modelagem` section of `tech_context.yml` needs a YAML
  parser agent-mux does not carry), and the downgrade of a grave finding
  whose quote is not in the documents (as in §20).
- **Not built:** the Preflight and Audit tabs of the Milestones screen
  and their keys (`P`, `A`, `g`): §36 built the tabs.
- **Phase 5 is built** as §32 to §35 describe, less what each names.
  Phase 6: §37 built the web pages and §38 `project init`.

## 36. Phase 5, the Milestones screen, as built

The Milestones screen's row of §9.2: a milestone's QA trail, preflight
and audit as tabs, after Features and Order.

- **Tabs:** Features · Order · QA trail · Preflight · Audit
  (`Screen::tabs`, `TAB_TRAIL`, `TAB_PREFLIGHT`, `TAB_AUDIT`). The first
  two are drawn from the snapshot. The other three are lines read on a
  thread of their own when the tab opens (`ProjectView::milestone_due`,
  `App::project_load_milestone`, taken by `project_tick` into
  `ProjectView.milestone_reads`), once per milestone and tab; `r` drops
  the open one so it is read again, and a sync drops them all. None is
  read on the UI thread, and none writes.
- **One text for the command and the screen.** `project preflight` and
  `project audit` were split into a read and its lines
  (`cli::preflight_read`, `preflight_lines`; `cli::audit_read`,
  `audit_head`, `audit_lines`, `audit_tail`), and the tabs show those
  lines. The tabs read over the snapshot on the screen, where the
  commands read GitHub first.
- **The QA trail tab** (`app::project::trail_lines`, pure): each
  Feature's scope (`trail::classify_scope`) and plan
  (`trail::classify_for_plan`), the gate (`trail::run_gate`), the cycles
  known (`trail::cycles`: kept here and merged; only the kept ones with
  no checkout of the hub) and the last one's report (`trail::render_md`).
- **Keys differ from §9.2's row.** `P` and `A` are the merge and the
  board catch-up on every screen, so the preflight and the audit have no
  key: they are read when their tab opens. `Q` on a milestone starts
  `project qa-trail plan <number>` while the gate holds a Feature and
  `project qa-trail run <number>` once it holds none; `C` starts
  `project audit --milestone <number> --critique`. Both are a
  confirmation and then a process of its own (`Op::Process`), named by
  the milestone's number, never its title.
- **Not built:** `g` (generate the missing specs of a milestone: `r` on
  each Feature does it), and the set critique's findings in the Audit
  tab: they are in the process's output file.

## 37. Phase 6, first part, as built

The web pages: `/api/project/` in the web explorer and its pages (§10).

- **Everything is answered from the cache.** `web::project` reads the
  snapshot `project::store` keeps, the fleet's dispatches and the
  sessions of the trace store through a read-only connection. No route
  calls `gh`: a page shows the last read and says when it was. A hub
  that was never read is a 404 that names `agent-mux project sync`.
- **Routes:** `GET /api/project/list` (the cached hubs),
  `/api/project/board?hub=` (items, stages, milestones with each
  Feature's documents, dispatches, `project_report::item_totals`),
  `/api/project/item?hub=&n=` (the item as `cli::item_json`, the card's
  lines, ancestors, children, `order::feature_order`, its dispatch and
  its trace: `project_report::report`, `runs`, `tree_lines`) and
  `/api/project/activity?hub=` (`observability_lines`). The hub is a
  query key, since it has a slash. POST aliases serve the SPA's client.
- **Pages** (`web/project.js`, group Projects, `g p`): Projects, Project
  (Board · Needs you · Milestones · Fleet · Activity, the view in the
  query string) and Item (Card · Children · Order · Documents · Pull
  requests · Trace). A session of the trace opens the session viewer
  and a run opens its workflow run.
- **Read-only.** No verb, no step, no merge, no dispatch and no sync
  from the web: the explorer starts no process and writes nothing.
- **Not built:** a milestone's QA trail, preflight and audit (they read
  a checkout or `gh`), an issue's thread (a `gh` read), a document's
  text, the Kanban columns of the Development screen as a page of their
  own, and a refresh when a sync lands (the pages follow the trace
  store's change feed, which a sync does not move: reload the page).
- **Left of phase 6:** `project init`, which §38 built.

## 38. Phase 6, second part, as built

`agent-mux project init`: a repository that never had spec-wave's CLI
becomes a spec-wave repository (§16, phase 6). The port of spec-wave's
`init` (CLI 1.7.6) without its third phase.

- **Decision 2 of §17, revised.** `.spec-wave.json` was never written.
  The operator chose (2026-11-25) that `init` writes it once: a new file,
  only in a folder that has none, only on `--yes`, in the CLI's exact
  shape. A file that exists is still never touched, by `init` or by
  anything else (`init::write_new` creates and never replaces).
- **What it makes** (`project::init`): the board (`createProjectV2`),
  `Etapa` with the twelve stages and their colours, `Work Item Type`,
  `Priority`, `Story Points`, `Area` (one at a time, as Projects v2
  requires), the text field `Release`, the link to the repository (a
  refusal is a line), and the forty labels of spec-wave's `ALL_LABELS`
  with their colours and descriptions (`gh label create`, 120 ms apart).
  The names, colours and descriptions are frozen by tests.
- **No workflow file.** agent-mux runs the steps, so spec-wave's files
  phase (the Actions, the issue templates, `tech_context.yml`) is left
  out: the result is what `spec-wave init --skip-files` makes. With no
  `execution` key the file says `actions` and the repository has no flow
  workflow, which the doctor reads as "no Action runs a step".
- **The plan only reads** (`init::plan`, all that runs without `--yes`):
  the owner and the repository in one query, the owner's boards with
  the title, the repository's labels. It refuses a folder with the file
  (before any read), a login that may not write, and an owner that
  already has an open board with the title.
- **Nothing that exists is changed.** A label that is there is not
  created again and never updated (no `--force`). `--board N` adopts a
  board: only the fields it lacks by name are added, and an `Etapa`
  that lacks a stage is a warning, never an edit. A failure after the
  board exists names `--board N`, which finishes the run with no second
  board. The file is written last.
- **The file** (`init::render_config`): spec-wave's keys in its order
  (`version`, `role`, `owner`, `repo`, `tracker`, `scm`, `project`, `ai`,
  `initializedAt`; a code repository: `version`, `role`, `owner`, `repo`,
  `hub`, `initializedAt`), two spaces and one newline at the end, written
  by an ordered writer since `serde_json` sorts keys. `project.fields`
  holds every single-select field as the board was read back, `Status`
  included, in the board's order: the shape the CLI's `refresh` writes,
  so a move sets `Status` from the file. Checked against a real hub on
  2026-11-25: the block read from its board equals the one in its file,
  order included. `version` is the CLI version ported (`1.7.6`).
- **A personal account is a warning**, where spec-wave refuses: the
  refusal is about its GitHub App, which agent-mux's steps do not use.
- **A code repository** (`--role code --hub o/r`): one read (both
  repositories exist) and the file. The entry for the hub's
  `repositories` is printed, since the hub's file is not changed.
- **Probed** with gh 2.98.0 against a real hub, reads only: the three
  queries, and that `gh api graphql -f` sends an `Int!` variable as a
  string (the board's number is written into the query). The mutations
  are spec-wave's own and run only against stand-ins in the tests
  (`project::init`'s tests, `tests/it/project_init.rs`).
- **Not built:** the workflow files, issue templates and
  `tech_context.yml`; writing a code repository into its hub's file
  (`repo link`); `update`, `refresh`, `mode`, `uninstall`; a wizard; a
  key on the screens (a folder with no file has no Project screen to
  press it on).

## 39. After the phases: editing an item, as built

`agent-mux project edit <n>` and `e` on the screens: the operator changes
an issue's title, priority, milestone or description without leaving
agent-mux. spec-wave's CLI has no such command (an issue is edited on
GitHub's site), so the rules are agent-mux's own, chosen so that what
spec-wave reads stays true.

- **Two steps** (`project::edit`): `Ops::edit_plan` reads the issue now
  (and the milestones or the description only when the edit names them)
  and resolves an `EditPlan`; `Ops::edit_write` writes it. A field that
  already has the value asked for is not in the plan, and an empty plan
  writes nothing. The command prints the plan and stops without `--yes`.
- **The title keeps its `[TYPE]` tag** (`edit::new_title`): the tag is
  the item's type for both tools. A tag typed in the new title is
  dropped, with a warning when it differs from the issue's.
- **The priority** is the label and the board's `Priority` field, as
  `new_item` writes them. The new label is added before the old ones are
  removed; `none` removes them and empties the field
  (`write::clear_field`, the mutation `clearProjectV2ItemFieldValue`). An
  issue with no board item changes only its label, and a board with no
  such field or option is a line, never an error.
- **The milestone** is named by title (any case) or number
  (`edit::resolve_milestone`; a title wins over a number) and must
  exist; `none` takes the issue out. A closed one is a warning. The title
  and the milestone go in one REST call (`write::patch_issue`:
  `gh api -X PATCH repos/<o>/<r>/issues/<n> -f title=… -F milestone=<n>|null`).
- **The description** is replaced whole (`write::set_body`). An empty
  one is refused. `edit::body_warnings` warns when the `Depende de:`
  lines change or an HTML comment is dropped; unlike `refine`, where a
  model wrote the text, the operator may change them. With `body_base`
  (the hash of the text the new one was written from) a description that
  changed on GitHub since is never replaced.
- **On the screens** `e` opens `Modal::Edit` (`EditForm`: title,
  priority, milestone; the snapshot's open milestones and the item's
  own), which ends in the same confirmation as every verb and runs as
  `Op::Edit`. `Ctrl+O` on the form leaves it for the description:
  `App::project_edit_body` reads it on a thread, the tick writes it under
  the scratch folder and asks for the operator's editor
  (`EditorRequest`, `project::BODY_EDITOR`), and
  `App::project_body_edited` asks for the yes when the text changed.
- **A fault this found:** `r`, `x`, `d` and `R` on the Project screens
  never reached them through the app's key path. The shared action
  registry (`App::command_availability`) matched them as Run, Stop,
  Delete and Restore, found them unavailable on this screen and
  swallowed the key; the screens' tests called `handle_project_key`
  directly, so none saw it. The registry now leaves the local verbs to
  the Project screens, and `tests/it/project_ui.rs` presses its keys
  through `App::handle_key`.
- **Probed** on 2026-11-25, reads only: GitHub's schema for
  `ClearProjectV2ItemFieldValueInput` (`projectId`, `itemId`, `fieldId`),
  `gh api --help` for `-F` (an integer and `null` are sent as JSON), and
  the plan against a real hub without `--yes`. The writes ran only
  against stand-ins (`project::edit`'s tests, `tests/it/project_edit.rs`,
  `tests/it/project_ui.rs`).
- **Not built:** an item's type, assignees, area and other labels;
  creating a milestone; the `Prioridade` line of a description's
  `## Metadados`, which is left as written when the priority changes (as
  `triage accept --severity` leaves it); the description in the form
  itself.
