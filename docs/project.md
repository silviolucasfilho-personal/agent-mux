# Projects

A **Project** is a repository configured with spec-wave, opened in agent-mux: its epics, features, stories, tasks and bugs, read from its GitHub issues and its Projects v2 board, each with its stage, its documents, its pull requests and its next step.

Four things are built: reading a Project, the first changes to it (create an item, move a card, repair a stage, triage a Bug, bring the board up to its pull requests), running its document steps (spec, plan, bug document, their critique and validation, and the decomposition into Stories and Tasks), and building one Story or fixing one Bug into a draft pull request. Every change is one you ask for and confirm. agent-mux never writes `.spec-wave.json` and never merges. A Feature built as a stack, QA and the dev-agent fleet are the later phases of the design: `docs/superpowers/specs/2026-11-18-issue-based-project-management-design.md`.

## 1. What you need

- A folder that has `.spec-wave.json`, or is inside one that does. A code repository (`"role": "code"`) opens the Project of its hub.
- `gh`, logged in. The board needs the `project` scope on its token: `gh auth refresh --scopes project`. Without it the Project still opens, each stage is derived from the issue's own labels, documents and pull requests, and every screen says so.

None of spec-wave's programs is used: no CLI, no Action, no daemon.

## 2. The screens

F1 → **Open project** opens the Project of the selected session's folder.

| Screen | Shows |
| --- | --- |
| Home | What waits on you (a document to review and merge, a plan to validate, a draft to apply, a bug to triage), where a step can run, and what is in development, code review and QA |
| Board | Every stage of the flow with its items. A retired column or one made by hand is listed after the flow's own, marked |
| Backlog | Initiative → Epic → Feature, for what is still before Spec |
| Specification | Features prioritized or in Spec: spec to write, spec in review, spec merged |
| Technical review | Features whose plan is being written or reviewed, whose critique failed, or that wait for validation |
| Decomposition | Approved Features and RFCs on their way to Stories and Tasks: to draft, draft in review, to audit, reviewed and ready to apply |
| Ready | The Stories that are ready, under their Feature, in execution order |
| Development | Stories and Bugs whose code is to be written: in development, ready to build, and waiting on a dependency that is not merged |
| Code review | Items with an open pull request for their code: changes requested, approved, waiting for a person, still a draft; and items the board says are in review with no pull request |
| Bugs | Bugs to triage first, then each later stage of a Bug's track |
| UAT and release | What is in acceptance (Homologação) and what waits for deploy |
| Milestones | Each open milestone, its Features and the state of each one's documents |
| Health | What `agent-mux project doctor` reports |

The left column holds the screens and, under them, the selected screen's rows. The right pane shows the selected row.

| Key | Does |
| --- | --- |
| `[` `]` | Previous and next screen |
| `Tab` | Between the list of screens and the rows |
| `↑` `↓` | Move |
| `Enter` | Open the item; on a heading, fold it |
| `←` `→`, `1`–`9` | The pane's tabs |
| `/` | Filter the rows by title or number |
| `o` | The issue in the browser |
| `Ctrl+R` | Read the Project from GitHub again |
| `Esc` | Clear the filter, then close |
| `r` | Run the selected item's next step: a document step, or the build of a Story or the fix of a Bug |
| `n` | New work item, under the selected one when it can have children |
| `m` | Move the selected item to a later stage |
| `M` | Repair its stage: any stage, earlier ones included, with a reason |
| `A` | Make the board moves its pull requests already justify |
| `f` | Fix what the review of the item's pull request asked for (the fix-pr step) |
| `P` | Merge: the pull requests of a Feature's Stories in order, or of one Story or Bug |
| `t` `R` `D` | On a Bug: accept, reject, mark a duplicate |

**The item screen** (`Enter` on an item) has five tabs: Card, Documents, Children, Pull requests and Thread. `Enter` opens what the cursor is on: a document on the default branch in the Markdown viewer (from the hub's checkout), a document still in review as its pull request in the browser, a child as its own item screen, a pull request in the browser. `Esc` goes back.

A `!` at the right of a row means the item waits on you. A `~` beside a stage means it was derived, because the issue is not on the board.

## 3. Changing a Project

Every change follows the same three steps: you pick it, agent-mux shows a box titled **This writes to GitHub** with exactly what will be written, and nothing happens until `y`. The change then runs in the background, the list's title says what is being done, and the item is read again from GitHub when it ends.

The rules are spec-wave's:

- **A stage only advances.** `m` offers only later stages. An item whose column is not a stage of the flow is not moved at all.
- **A repair is the exception.** `M` may put an item at any stage. It needs a reason, and it leaves a note with the reason and your login on the issue.
- **A Spike is moved by hand** on the board itself.
- **A new item is born at its type's stage:** a Bug in triage (in Ready when P0), a Story or a Task in Ready and only under a parent, anything else in Backlog. It is put on the board, with its type, in the same step.
- **Triage has three outcomes.** Accept adds `spec-wave:triaged` and moves the Bug to Ready; with a severity it replaces the priority label. A P2 or P3 Bug, or one with no severity, needs `spec-wave:bug-approved` first. Reject needs a reason and closes the issue as not planned. Duplicate needs the original's number, comments on both issues and closes this one. A reject and a duplicate never touch the stage.
- **The board follows the pull requests only when you say so.** With no Action running, nobody moves an item when its pull request is ready, approved or merged. `A` (and `agent-mux project advance`) lists those moves and makes them on your yes.

What agent-mux writes on an issue (the parent line, the triage and repair comments) is in spec-wave's own words, so an issue reads the same whichever tool acted on it.

Items that wait on you are also rows of the inbox (`I`), under **Projects**, for every Project read on this machine. `Enter` opens the item.

## 4. Running a step

`r` on a Feature or a Bug (or `agent-mux project run <n>`) runs its next document step; on a Story that is Ready or a Bug that was triaged it builds (see "Building a Story, fixing a Bug" below). The order is spec-wave's:

| Item | Step | What happens |
| --- | --- | --- |
| Feature with no spec | spec | A session writes `spec.md`; agent-mux publishes it |
| Feature with a merged spec | plan | A session writes `plan.md` from the merged spec and the tech context; agent-mux publishes it; a second session audits it |
| Feature with both merged | validate | Rust checks the required sections, cut documents and open `[TODO]` marks; if all pass, `spec-wave:plan-approved` is added |
| Approved Feature, or an RFC | decompose | A session drafts the Stories and Tasks; agent-mux renders them as `decomposition.md` and publishes it; a second session audits it. No issue is created |
| Feature or RFC whose draft you reviewed and merged | apply | agent-mux creates the Story and Task issues. No model takes part |
| Bug with no document | bug | A session writes `bug.md`; agent-mux publishes it; a second session audits it |
| Bug with a merged document | validate | Rust checks its six sections; if they are there, `spec-wave:bug-approved` is added |

How a step works:

- **The session is read-only.** It reads the repository and answers the document as text. It writes no file and runs no git command.
- **agent-mux publishes.** It commits the file on top of the remote's default branch in a temporary worktree, pushes it to `spec-wave/<n>-<step>`, opens a pull request and comments on the issue. Your checkout is not touched, the default branch is never pushed, and nothing is merged.
- **The next step waits for your merge.** A document in a pull request blocks the step after it (`pr-pending`) and is never generated again over your review.
- **The critique** answers findings, each `grave` or `menor`, in a 🔎 comment on the issue. A grave one adds `spec-wave:critique-failed` and stops the flow for you: correct the document, remove the label, then `agent-mux project run <n> --step critique` audits it again as it is. The third failed audit in a row adds `spec-wave:needs-human` and calls no model.
- **The comments and labels are spec-wave's**, the attempt marker included, so either tool counts the other's attempts.

### The decomposition

The draft is a file you edit, not a list of issues. `decomposition.md` holds `## Story N` sections, each with a user story, a `**Depende de:**` line and its `### Task N.M` sections. Review and correct it in its pull request:

- Position decides, not the number written: insert a Story in the middle without renumbering.
- `**Depende de:**` takes earlier Stories (`Story 1`) and existing issues (`#412`), or `—`. A Story that depends on a later one is an error, with its reason.
- `**Repos:**` names the code repositories a Story touches, by their slugs in `.spec-wave.json`.
- Anything else under a Story or a Task is its description, Markdown included.

A clean audit adds `spec-wave:decompose-ready`. After you merge the draft, the next step is the apply, and it never runs on its own:

```
agent-mux project run <n> --dry-run     the Stories and Tasks it would create
agent-mux project run <n> --apply       create them
```

The apply creates each Story as `[STORY] …` and each Task as `[TASK] …`, with the Feature's milestone, the `repo:<slug>` labels, the `Depende de: #a, #b` line and GitHub's blocked-by relation, each one a sub-issue of its parent and in Ready on the board. It then moves the Feature to Ready, adds `spec-wave:decomposed`, comments the list and the execution order, and opens one more pull request that records the issue numbers in the draft and writes `dependency-map.json`.

It cannot be applied twice: the label, the draft's own `applied=` mark and existing Story sub-issues each stop it. If it fails halfway (a network error after three issues), run it again: it keeps a journal of what it created and makes only what is missing.

```
agent-mux project run <n> --dry-run     what would run, and nothing else
agent-mux project run <n>               run it
agent-mux project run <n> --step critique|validate|spec|plan|bug
agent-mux project run <n> --harness codex --critic claude
```

A step runs on Claude Code unless `--harness` names another; only the six harnesses agent-mux traces are accepted. `--critic` puts the audit on a different harness. On Claude Code the model is the one `.spec-wave.json` asks for (`ai.models`, `ai.escalationModel` from the second audit, a `spec-wave:model:<alias>` label on the issue). From a screen, `r` starts the step as a process of its own, on Claude Code; the notice names its log file, and the run is in the runs view (`W`) like any flow's.

The flows (`project-spec`, `project-plan`, `project-bug`, `project-critique`, `project-decompose`, `project-decompose-rfc`, and the two build flows below), their step skills (`wf-project-*`; the document ones are ported from spec-wave's prompts, in Portuguese) and their agents (`spec-writer`, `tech-lead`, `critic`, and the factory's `implementer` and `reviewer`) are in the Configuration view (`C`) and can be edited like any other.

### Building a Story, fixing a Bug

`project run <n>` on a Story that is Ready, or on a Bug that was triaged, writes its code:

```
agent-mux project run <story> --dry-run     where it would build, its Tasks, the branch
agent-mux project run <story>               build it
agent-mux project run <bug>                 fix it
```

1. **The board** moves at once: the Story (and its Feature) to Desenvolvimento / In Progress, its Tasks to Desenvolvimento.
2. **One session writes the code** (flow `project-story` or `project-bug-fix`) in a worktree of the code repository, forked from the remote's default branch. Your checkout is not touched. The session reads what spec-wave's `implement` would hand its executor: the Story, its open Tasks in order, people's comments, and the Feature's merged spec and plan. A Bug's session follows spec-wave's four phases (a failing test, the root cause, the smallest fix, the regression test) and reads `bug.md` as a hypothesis to confirm.
3. **A second session reviews the committed diff**, on another harness when one is installed, against the Story's acceptance criteria (the bullets under its "Critérios de aceite" heading) and one criterion per Task. The work goes back to the first session at most twice.
4. **agent-mux delivers**, only when the review approved with every criterion passing: it pushes one branch (`story/<n>-<slug>` or `fix/<n>-<slug>`, never the default branch, never forced), opens a **draft** pull request whose description starts with `Closes <owner>/<hub>#<n>` (`Fixes` for a Bug), moves the Tasks to Done and the Story to Code Review (the Feature too, once every Story is there), and leaves a note on the issue.

Otherwise nothing is pushed and nothing is written to GitHub but the board's first move: the command says what the review found and where the work is (its worktree and branch).

What stops a build before it starts, each with its reason:

- **A dependency that is not merged** (`Depende de:` or blocked-by on an item that is open and before QA). The build forks from the default branch, which would not have that code. `--ignore-dependencies` builds anyway, and the session is told.
- **An open pull request** for the item: review it, or close it to build again.
- **The branch already on the remote** with no pull request: open the pull request from it, or delete it.
- **A Task** on its own: it is built with its Story.
- **A Feature**: its Stories are built one at a time, and the message names the next in order.

Where the code goes: the repository of the item's `repo:<slug>` label, found as spec-wave finds it (a `dirs` entry in the hub's `.spec-wave/workspace.json`, the folder beside the hub, then `.spec-wave/repos/<slug>` in the hub). An item with no label is built in the hub. The folder must be a checkout of that repository. An item with several labels is not built yet.

Commits carry spec-wave's trailers (`Spec-Wave-Story: #n`, `Spec-Wave-Tasks: #a, #b`), which its `merge` reads. The session is asked for them; when no commit has them, agent-mux adds one empty commit that does.

### Fixing what a review asked for

When a person reviewed an item's pull request and left comments, `f` on the item (or the command) sends them back to a session:

```
agent-mux project run <n> --step fix-pr --dry-run     the comments it would fix
agent-mux project run <n> --step fix-pr
```

1. **agent-mux reads the review**: the threads that are not resolved, and the body of a review submitted after the branch's last commit. A thread this step already answered is left out until the reviewer writes in it again or resolves it. With nothing to fix, it stops and writes nothing.
2. **One session fixes them** (flow `project-fix-pr`) in a worktree on the pull request's own branch, one commit per comment. A comment it should not apply (wrong, out of scope, a product decision) changes no code: the session declines it with a reason.
3. **A second session reviews the new commits**, one criterion per comment. The work goes back at most twice.
4. **agent-mux delivers**, only on an approval with every criterion passing: it pushes the commits to the pull request's branch (never forced), answers each thread with the commit that fixes it, or with the reason it was declined, and leaves one summary on the pull request.

Nothing moves on the board, and no conversation is resolved for you: that is the reviewer's. The item must have exactly one open pull request, on a branch such as `story/…` or `fix/…`.

### Merging

`project merge` is the only place agent-mux merges, and only on your word:

```
agent-mux project merge <feature>              the plan: the order, the retargets, what blocks it
agent-mux project merge <feature> --yes        merge in that order
agent-mux project merge <story or bug> --yes   one item's pull request
```

- **The order** is the Stories' dependencies (`Depende de:`, blocked-by). A cycle has no order and stops the plan.
- **A draft blocks the whole plan.** Marking a pull request ready is your review of it, so agent-mux never does it for you. Two open pull requests for one item in the same repository block it too.
- **Each pull request** is read again, pointed at the default branch when it was based on another Story's branch, and merged with a merge commit (never a squash, which would break the pull requests stacked on it). Its Story or Bug then moves to QA, and the Feature follows once every Story is there.
- **A merge GitHub refuses** (checks, a conflict) stops the queue there. What merged stays merged; run the command again and it resumes, since a merged pull request leaves the plan.
- **The branches** are deleted at the end, when nothing open is based on them. `--keep-branches` keeps them.

`P` on a screen shows the same plan from the cache and asks before it starts the command.

### The sessions of an item

Every session a step starts records the work item it runs for on its launch row (`launches.metadata.project`: hub, item, type, Feature, step, attempt, role, run). One command reads them back as one trace:

```
agent-mux trace project <n>            a tree: each step's run, and its sessions with turns, tools, tokens and cost
agent-mux trace project <n> --log      one line per session, oldest first
agent-mux trace project <n> --json
```

A Feature is read with its Stories and their Tasks. The hub is the one of the folder's `.spec-wave.json`; `--hub owner/repo` names another. Steps run before this version carry no tag and are not listed.

## 5. The next step

Every card ends with the item's next step and why. It is the answer spec-wave's own `run` gives for the same issue: the code is a port of its `nextStep`.

```
#491 [FEATURE] Menu e Tela RFC
  next: `docs/features/menu-e-tela-rfc/spec.md` was generated and is not on the base branch yet.
  blocked (pr-pending): It is in pull request #578, not merged yet.
  then: Review and merge pull request #578.
```

| Blocked by | Means |
| --- | --- |
| `pr-pending` | A document was generated and is not merged. It is never generated again over a review |
| `critique-failed` | The critique found a grave contradiction; correct the document and remove the label |
| `needs-human` | The critique failed repeatedly; a person reviews and removes the label |
| `trigger-pending` | A trigger label is on the issue: an Action is running, or failed and left it |
| `inconsistent-state` | A label claims a document that is not there (the issue's title changed, so its folder did) |
| `unknown-document` | A read of the repository failed. agent-mux does not guess that a document is missing |
| `manual` | The step is a person's: triage, or starting the implementation |
| `dependency-pending` | A Story depends on one still open |

Past Ready the line comes from the board: how many children are done, which pull request is in review, whether the QA plan is reviewed.

## 6. Commands

```
agent-mux project doctor [--json]
agent-mux project sync
agent-mux project ls [--type T] [--stage S] [--milestone M] [--all] [--json]
agent-mux project show <n> [--json]
agent-mux project next <n> [--json]
agent-mux project order [<feature>] [--milestone M] [--json]

agent-mux project new --type T --title "…" [--parent N] [--priority P] [--area A] [--body "…"]
agent-mux project move <n> <stage> [--status S]
agent-mux project repair-stage <n[,m…]> <stage> --yes --reason "…" [--status S]
agent-mux project task start|done <n>
agent-mux project story review <n>
agent-mux project triage accept <n> [--severity P]
agent-mux project triage reject <n> --reason "…"
agent-mux project triage duplicate <n> --of <original>
agent-mux project advance [--yes]
agent-mux project merge <n> [--yes] [--keep-branches] [--dry-run]
agent-mux project run <n> [--step …] [--harness H] [--critic H] [--dry-run] [--apply] [--ignore-dependencies]
```

The first block reads. The second writes to GitHub at once, with no confirmation: typing the command is the confirmation. `advance` and `merge` alone print their plan and need `--yes`. `task start` refuses while another Task of the same Story is in progress.

Every command takes `--dir DIR`, `--refresh` and `--cached`. A stage is typed as on the board or shorter: `--stage "code review"`, `--stage ready`.

`doctor` exits 1 when a line fails. Its marks: `✓` works, `!` works with something to know, `✗` must be fixed, `○` could not be checked. It reports, among others, issues that are not on the board, columns outside the flow, labels this version does not know, whether spec-wave's workflows are armed, and a disagreement between `execution.mode` in the file and the repository variable `SPEC_WAVE_EXECUTION`.

## 7. The cache

A read of GitHub takes a few seconds per hundred issues, so it is kept in the trace store (`project_items`, `project_boards`, schema v30) for `cache.ttlSec` of `.spec-wave.json`, 600 seconds when unset. The screens open on the cache at once and read again in the background when it is old. `--refresh` and `Ctrl+R` read now; `--cached` never reads.

The cache holds every open issue and the ones closed in the last 30 days. It is only a cache: delete the rows and `agent-mux project sync` brings everything back.

## 8. What is read, and how

Everything goes through `gh`; agent-mux reads no token. A verb reads the issue it acts on at that moment, never from the cache.

| Read | With |
| --- | --- |
| Issues: labels, milestone, parent, sub-issues, blocked-by, linked pull requests, and the board's `Etapa`, `Status` and other fields | one GraphQL query per 50 issues |
| Which documents exist on the default branch | one GraphQL query over `docs/features`, `docs/bugs`, `docs/rfcs` |
| Documents still in review | the open pull requests and the branches under `spec-wave/` |
| Pull requests of each code repository | `gh pr list`, linked by the hub issue each names (`Closes owner/hub#12`); GitHub's own link does not cross repositories |
| Milestones | the REST list |

A read that fails never becomes a fact. A document whose listing failed is `unknown`, not `missing`, and the snapshot carries a warning that Home and Health show.

## 9. Not built yet

- A Feature built as a stack of Stories with its pull requests marked ready together, and a Story across several repositories.
- QA and the dev-agent fleet (dispatch, slots, the lease).
- An item's Trace tab, the History badge and the Observability screen: the sessions of an item are read with `agent-mux trace project` only.
- Recording a Tech Leader's decision on a finding (`risk-accepted`).
- Driving the repository's own Actions by trigger label instead of agent-mux's flows.
- Editing an item's title, body, milestone or priority from agent-mux.
- The mouse on the Project screens.
- A repository with no `.spec-wave.json`.
