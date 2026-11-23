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
| Observability | What the Project's steps cost and what is running: totals, sessions going now, and the cost by step, harness, milestone, executor and item, for the last 30 days and ever |
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
| `d` `x` | On Development: dispatch the item to the fleet; take a dispatched item back to Backlog (`b` does the same on a Blocked card) |
| `Q` | Run QA on an item that is in QA (a Story, a Feature's pending Stories, or a Bug's regression test) |
| `E` | Estimate the item in story points; with a proposal waiting, write it |
| `W` | Propose a clearer description of the item; with a proposal waiting, write it |
| `c` | Talk about the item: an interactive session with the item as its opening message |
| `a` | Run one of your agents on the item: pick it, and an interactive session starts as that agent |
| `I` | Insights: ask for a reading of the Project, or open the last one |

**The item screen** (`Enter` on an item) has six tabs: Card, Documents, Children, Pull requests, Thread and Trace. `Enter` opens what the cursor is on: a document on the default branch in the Markdown viewer (from the hub's checkout), a document still in review as its pull request in the browser, a child as its own item screen, a pull request in the browser, a run of a step on the Run screen. `Esc` goes back.

The **Trace** tab lists the runs of the item's steps, newest last, each with its sessions, tokens, cost and time; a Feature's tab includes the runs of its Stories and their Tasks. Under the list are the sessions of the selected run (work, review or critique, with the harness, turns and tools). `Enter` opens the run on the Run screen, and `Esc` there returns to the tab. It is read from the trace store each time the tab opens and on `Ctrl+R`; nothing is asked of GitHub.

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

- **A dependency with no code yet** (`Depende de:` or blocked-by on an item that is open, before QA and has no open pull request). `--ignore-dependencies` builds anyway, and the session is told. A dependency with an open pull request does not stop it: the build is stacked on it (below).
- **An open pull request** for the item: review it, or close it to build again.
- **The branch already on the remote** with no pull request: open the pull request from it, or delete it.
- **A Task whose Story is in code review**: the Story's pull request carries it.
- **A Feature whose stack cannot be planned**: its pending Stories depend on each other in a cycle, or one has no Task.

Where the code goes: the repository of the item's `repo:<slug>` label, found as spec-wave finds it (a `dirs` entry in the hub's `.spec-wave/workspace.json`, the folder beside the hub, then `.spec-wave/repos/<slug>` in the hub). An item with no label is built in the hub. The folder must be a checkout of that repository. A Story with several labels is built one repository at a time (below); a Bug or a Task with several is not built yet.

Commits carry spec-wave's trailers (`Spec-Wave-Story: #n`, `Spec-Wave-Tasks: #a, #b`), which its `merge` reads. The session is asked for them; when no commit has them, agent-mux adds one empty commit that does.

### A Feature as a stack

`project run <feature>` on a Feature whose document flow is over builds its pending Stories one at a time, in the order of their dependencies:

```
agent-mux project run <feature> --dry-run     the Stories it would build, in order, and the ones it leaves alone
agent-mux project run <feature>               build them
agent-mux project run <feature> --keep-drafts leave every pull request a draft at the end
```

- **Each Story is its own build** (the two sessions and the delivery above). Its worktree forks from the branch of the Story before it, in the Feature's order, that has an open pull request in the same repository, and its pull request is based on that branch; with none, the default branch. A stack is per repository.
- **Stories in code review or further on are not touched.** A pending Story with no Task, or a cycle among the pending ones, stops the plan before anything runs.
- **Every pull request stays a draft** until the last Story is delivered. Then agent-mux marks the Feature's draft pull requests ready for review together (`gh pr ready`) and the Feature moves to Code Review. This is the one place agent-mux takes a pull request out of draft.
- **A Story that is not approved stops the stack.** What was delivered stays; running the Feature again resumes at the Story it stopped on.

Running one Story on its own follows the same rules: it is stacked on the Story before it when that one has an open pull request, and when it is the last of its Feature it marks the Feature's drafts ready. Before the session starts agent-mux checks that every dependency with an open pull request in the repository is under the branch the build forks from, and says which is not.

`agent-mux project merge <feature>` merges a stack in order: it points each pull request at the default branch before merging it and deletes the branches at the end.

### A Story across repositories

A Story with several `repo:<slug>` labels is built one repository at a time, in the order of the labels: one run (two sessions), one branch and one draft pull request in each, all with `Closes <owner>/<hub>#<n>`. Each session is told which repository is its own, which are the others and which were delivered already, and that a Task or criterion that belongs wholly to another repository does not apply to it. The board moves to Code Review, and a Feature's drafts are marked ready, only after the last repository. A run that stops resumes at the first repository with no pull request.

GitHub closes no issue from another repository's pull request, so the hub issue of such a Story stays open after the merge; agent-mux does not close it yet.

### A Task on its own

`project run <task>` builds one Task alone: its own branch (`task/<n>-<slug>`), its own draft pull request with `Closes <owner>/<hub>#<n>`, and the Task at Done when it is delivered. Its commits carry the Story's trailer and its own. Running the Story builds all its Tasks in one pull request instead; once the Story is in code review its Tasks are not built on their own.

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

### The QA plan of a Feature

```
agent-mux project run <feature> --step qa-plan --dry-run   what it would do
agent-mux project run <feature> --step qa-plan             write the plan, or audit the one that is there
```

The plan (`qa-plan.md`, beside the Feature's spec) holds the scenarios `project qa` executes, each owned by one Story. The step needs the Feature's `spec.md` and `plan.md` on the default branch and at least one Story, and stops while `spec-wave:needs-human` or `spec-wave:critique-failed` is on the issue. It is a Feature's step: on a Story it names the Feature, and a Bug has no plan.

1. **A read-only session writes the scenarios** (flow `project-qa-plan`, agent `qa-analyst`) from the spec's acceptance criteria, the plan and the Feature's real Stories. It writes no file.
2. **agent-mux renders and checks the file**: the marker and the scenario numbers are its own, every scenario names a Story of the Feature and has a `**Critério:**` and an `**Esperado:**`, and every Story has at least one scenario (one closed as not planned needs none). A plan that fails is not published.
3. **The plan is published** on `spec-wave/<n>-qa-plan` with a pull request, and the issue gets a comment. Your checkout is not touched.
4. **A second session audits it** against the spec and the plan (flow `project-critique`, kind `qa`): a criterion with no scenario, a scenario that would pass without the implementation, an expected result nobody can observe. The 🔎 comment is spec-wave's.
5. **Clean:** `spec-wave:qa-ready` is added and a comment says how many scenarios each Story has. **Grave findings:** `spec-wave:critique-failed`, `qa-ready` is taken off if it was there, and no QA runs until the plan is corrected. After the third failed audit the step stops with `spec-wave:needs-human`.

A plan that already exists, merged or still on its pull request's branch, is never written again: the step validates and audits it as it is, so what you edited stays. Run the step again after editing; delete the file to have another written. `qa-ready` is the gate a person holds: a green QA run moves the board by itself, so read the scenarios before the pull request is merged. `project qa` reads the plan from the default branch only.

`Q` on a Feature whose plan is not released starts this step after a confirmation; once `qa-ready` is there, `Q` runs the QA.

### QA of an item

```
agent-mux project qa <n> --dry-run     the scenarios it would run, and where
agent-mux project qa <story>           the Story's scenarios
agent-mux project qa <feature>         the scenarios of every Story not approved yet
agent-mux project qa <bug>             the Bug's regression test
agent-mux project qa <story> --only 2  one scenario again, after a fix
```

QA runs only on an item that is **in QA** (the merge moves it there; `qa` never promotes) and, for a Story or a Feature, only when the Feature has `spec-wave:qa-ready`. The scenarios come from the Feature's `qa-plan.md` on the default branch: each `## Cenário N — Story #<n>` belongs to one Story, and its number is its position in the file. A Bug's one scenario is the "Teste de Regressão" section of its `bug.md`.

1. **A session executes the scenarios** (flow `project-qa`, agent `qa-analyst`) in a temporary worktree of the code repository at the default branch's tip, after `qa.setup` of the Project's file when it has one. It answers `pass`, `fail` or `blocked` for each, with the raw evidence. It fixes nothing: what it changes goes away with the worktree.
2. **agent-mux checks the answer**: one verdict per scenario asked, a `fail` with evidence, a `blocked` with one of spec-wave's reasons. An answer outside these rules, or a plan that changed on the default branch while the session ran, writes nothing at all.
3. **The 🧪 report** is posted on the issue, in spec-wave's format, with the block the next run reads. A run with `--only` keeps the other scenarios' verdicts from the last report.

Then the verdict decides:

- **Green** (every scenario of the scope passed): `spec-wave:qa-approved`, and a Story moves to Homologação (a Bug to Deploy). The Feature follows when every one of its Stories passed. A Story with an open Bug under it does not advance, even green.
- **Red**: one Bug per failed scenario, created under the Story, in Ready, with the severity of `qa.defaultBugPriority` (P2; `--severity` changes it) and `spec-wave:bug-approved`. Its `bug.md` is written by agent-mux from the scenario and the evidence, with no model, and published on a pull request. The item stays in QA. A scenario that fails again comments on the Bug it already has. A Bug's own red regression test opens no new Bug.
- **Blocked and nothing failed**: only the report. A broken environment is not a defect, so nothing moves and no Bug is created.

Not done: screenshots and video as evidence, and a Story across several repositories is tested in the first one.

### The fleet

Instead of running one item yourself and waiting, you can dispatch several and let agent-mux run them a few at a time.

```
agent-mux project dispatch <n> [--harness H]   the item's card goes to WIP
agent-mux project fleet run                    run what is dispatched, 3 at a time, until stopped
agent-mux project fleet run --once             the same, ending when nothing is left to run
agent-mux project fleet status                 the slots, every card, and who holds which item
agent-mux project cancel <n>                   back to Backlog; a run in progress is stopped
```

- **A run is `agent-mux project run <n>`** as a process of its own: a Story's build, a Feature's stack, a Task or a Bug's fix, each with its review and its delivery. Bugs go first, then the oldest dispatch. Its output is in a file the card names.
- **The lease.** Before a run starts, agent-mux takes the item's lease: a ref on the hub's remote (`refs/heads/spec-wave-agent/claims/<n>`) whose commit says who holds it (`<your login>@<host>`) and when it last beat. It is `spec-wave-agent`'s own protocol, so that daemon and another machine's agent-mux see it and stay off the item, and this machine stays off theirs. The runner writes a heartbeat every two minutes; a lease with none for ten minutes may be taken by someone else. If this machine's lease is taken, the run is stopped at once and the card is Blocked.
- **How a run ends.** A pull request open: the card goes to Review. A failure: it is queued again, and Blocked after three, with what the run said last. A run that ends cleanly with no pull request (a dependency not ready, nothing to build) is Blocked at once, since running it again would end the same way.
- **The Development screen** is the kanban: `fleet · WIP`, `fleet · Blocked` and `fleet · Review` over the rest of the screen, which is the Backlog. `d` dispatches the selected item and starts the runner on this machine if none is going; `x` takes a card back. A Blocked card is dispatched again with `d`, from zero failures.

One runner works a Project on a machine at a time: a second `fleet run` only queues what it was given and leaves. Moving a card on GitHub's board dispatches nothing.

**Where work comes from.** Besides your own dispatch, the runner can take work from two places, checked at most once a minute:

- **A request addressed to you** (on by default). `agent-mux project dispatch <n> --to <login>` hands an item to another developer: it writes a comment on the issue with a marker, and the card on your side reads "waiting for `<login>`". Their `fleet run` reads the hub's new comments, and takes a request addressed to their login when its author may triage the repository. `project cancel <n>` takes the request back with a second comment.
- **spec-wave's queue label** (off by default). With `label = true`, an open Feature, Story, Task or Bug that carries `spec-wave:dev-agent` and has no card is dispatched, if the login that added the label may triage the repository. It is off by default because a `spec-wave-agent` daemon on the same label would start too; the lease lets only one of them run the item, but the other's start is wasted.

**One branch for a Feature.** `delivery = "agent"` (or `--delivery agent` on `fleet run` and on `project run <feature>`) delivers a Feature the way spec-wave's daemon does: every Story is a checkpoint on one branch, `agent/issue-<n>`, each still reviewed before it is pushed, and the Feature gets one pull request after its last Story. That pull request names the Stories and closes nothing, since a merged Feature still has QA ahead. A Story that changes several repositories cannot be delivered this way.

**Settings**, in agent-mux's `profiles.toml`; a flag of `fleet run` wins over them:

```toml
[project.fleet]
max_runs = 3              # runs at once
max_failures = 3          # failed runs before a card is Blocked
delivery = "pr-stack"     # or "agent": one branch and one pull request per Feature
label = false             # take issues that carry queue_label
queue_label = "spec-wave:dev-agent"
request = true            # take dispatches addressed to your login
```

Stopping a run (a cancel, a lost lease) ends its whole process group, so the sessions it started end with it.

### The workspace

```
agent-mux project workspace map --issue <n>       where each code repository of the item is, or would be
agent-mux project workspace prepare --issue <n>   clone the ones missing, and write the manifest
```

The repositories are those of the item's `repo:<slug>` labels and of everything under it (a Feature gathers its Stories'). Each is looked for as a build looks for it: the override in `.spec-wave/workspace.json`, the folder beside the hub, then `.spec-wave/repos/<slug>` inside the hub. `prepare` clones only that last kind (with your own git credentials, into a folder that ignores itself) and writes `.spec-wave/workspace-<n>.json`, the file spec-wave's own tools read. A missing override or sibling is your folder: it is reported, never cloned.

### Estimating and refining an item

Two read-only sessions propose something about an item, and **nothing is written until you read the proposal and say yes**:

```
agent-mux project estimate <n>              propose its effort in story points
agent-mux project refine <n>                propose a clearer description
agent-mux project estimate|refine <n> --yes write the proposal that is waiting
agent-mux project estimate|refine <n> --discard
```

- **Estimate** (flow `project-estimate`): the session reads the item, what is under it and, for a Feature, its merged spec and plan, and may open the files they name. It answers a number, why, and what could make the item larger. agent-mux fits the number to the options of the board's `Story Points` field (1, 2, 3, 5, 8, 13, 21 when the Project's file does not list them). `--yes` sets that field.
- **Refine** (flow `project-refine`): the session rewrites the description so that what is asked, for whom and why are explicit, with acceptance criteria as a list. It does not add or remove a requirement; what the original leaves open comes back as questions. agent-mux refuses a proposal that changes a `Depende de:` line, drops an HTML comment or unchecks a box. `--yes` replaces the issue's description, unless it changed since the proposal was made.
- **The proposal waits** in a file of agent-mux's scratch folder and is shown on the item's card. `E` or `W` on the screens starts the session when nothing waits, and asks to write the proposal when one does. With `--yes` and a proposal waiting, no session runs: what you read is what is written. `--yes` with none runs the session and writes its answer at once.

### Talking about an item

`c` on an item starts an interactive session in the hub's folder, on Claude Code when a profile runs it (else the first traced harness that has one). Its opening message invokes the `/project` skill with the item's number and card. The skill drives `agent-mux project …`: it shows `project next` before any step, asks before anything that writes, and keeps spec-wave's rules (never write `spec.md`, `plan.md` or `bug.md` by hand, never create a Story or Task outside a decomposition, never call `gh issue create` or merge with `gh`). The session opens in the Agents list like any other, and it is part of the item's trace with the role `chat`.

Any session in a spec-wave repository may invoke `/project` by hand (`$project` on Codex).

**As one of your agents.** `a` on an item lists your personas (the agents of `~/.agent-mux/agents/` and of the hub's `.agent-mux/agents/`, and the built-in ones; an agent with a task or a schedule of its own is not listed). The session starts as the one you pick, with its instructions, tools and model, and the item's card as its first message in place of the `/project` skill. It is told to change nothing on GitHub before you say yes. It is part of the item's trace with the role `agent`.

### Insights

```
agent-mux project insights                          where the Project stands (scope progress)
agent-mux project insights --scope tech|daily|brainstorm [--topic "…"]
agent-mux project insights <n> --scope tech         the same, for one item and what is under it
agent-mux project insights --dry-run                what the session would be given
```

A read-only session (flow `project-insights`) is given a digest of the Project as agent-mux last read it: open items per stage, the milestones, and one line per open item with its stage, priority, pull requests and next step or block. It answers a short reading in Markdown for the scope asked: `progress` (milestones at risk, bottlenecks, three recommendations), `tech` (what is stuck in spec or plan, blocked items, pull requests waiting), `daily` (what is going and the next Story) or `brainstorm` (ideas for the backlog). The answer is printed and kept as a file; `I` on the screens asks for one or opens the last. Nothing is written to GitHub, and no issue is created from an idea.

### The sessions of an item

Every session a step starts records the work item it runs for on its launch row (`launches.metadata.project`: hub, item, type, Feature, step, attempt, role, run). One command reads them back as one trace:

```
agent-mux trace project <n>            a tree: each step's run, and its sessions with turns, tools, tokens and cost
agent-mux trace project <n> --timeline one row per session on a time axis, a bar where it ran
agent-mux trace project <n> --log      one line per session, oldest first
agent-mux trace project <n> --json
```

A Feature is read with its Stories and their Tasks. The hub is the one of the folder's `.spec-wave.json`; `--hub owner/repo` names another. Steps run before sessions carried this record are not listed.

**On the cards.** An item's card, on the screens and in `agent-mux project show <n>`, ends with what its steps' sessions add up to: how many, their tokens, cost and running time. A Feature's line also gives the total with what is under it. The numbers come from the trace store, never from GitHub.

**Heimdall.** In a spec-wave repository, Heimdall's briefing has a `projects` section: what the Project's steps ran in the last day (runs per kind of step, sessions, items, tokens, cost) and where the fleet's cards are, with the reason of each Blocked one.

The same trace is shown in four more places:

- **The item's Trace tab** (above).
- **History.** A past session a step started is marked with its item and step (`#412 build`) in the History list and on the History screen, where `p` lists only those sessions. `Enter` resumes one like any other, to ask it what it did.
- **MCP.** `agent_mux_get_project_item` (`item`, and `hub` when the workspace is not the Project's) answers the report with its sessions grouped into runs, for a session or Heimdall. Three more tools answer from the cache, with no call to GitHub: `agent_mux_project_list` (`type`, `stage`, `milestone`, `all`), `agent_mux_project_item` (the card and the next step) and `agent_mux_project_next`.
- **Langfuse.** With Langfuse configured, every turn of a step's session carries `session.id = <hub>#<n>` and the tags `project`, `project:<step>` and `milestone:<m>`, so a work item is one Langfuse session. After a build, a fix or a fix-pr finishes, agent-mux sends the item's **summary trace**: a span for the item and one per run (its sessions, tokens, cost and their trace ids). `agent-mux langfuse export-project <n> [--hub owner/repo] [--dry-run]` sends an item again, or for the first time when its steps ran with Langfuse off.

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
agent-mux project workspace map|prepare --issue <n> [--json]
agent-mux project dispatch <n> [--harness H] [--to <login>]
agent-mux project cancel <n>
agent-mux project fleet status [--json]
agent-mux project fleet run [--once] [--dispatch <n>] [--max-runs N] [--max-failures N] [--poll SECONDS] [--label] [--delivery pr-stack|agent]
agent-mux project qa <n> [--only a,b] [--severity P] [--harness H] [--dry-run]
agent-mux project insights [<n>] [--scope progress|tech|daily|brainstorm] [--topic "…"] [--harness H] [--dry-run]
agent-mux project estimate <n> [--yes] [--discard] [--harness H] [--dry-run] [--json]
agent-mux project refine <n> [--yes] [--discard] [--harness H] [--dry-run] [--json]
agent-mux project run <n> [--step …] [--harness H] [--critic H] [--dry-run] [--apply] [--ignore-dependencies] [--keep-drafts]
agent-mux project run <feature> --step qa-plan [--harness H] [--critic H] [--dry-run]
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

- Closing the hub issue of a cross-repository Story when it reaches Done, and a Bug or a Task across several repositories.
- The QA of a milestone (the trail and its cycles), and the milestone's preflight and audit.
- Starting the QA plan from the `spec-wave:qa` label, and the comments spec-wave leaves when the step is refused: the command prints the reason instead.
- In the fleet: dispatching to a named developer from the screens (the command does it), the extra tracks of spec-wave's queue label (`devAgent.queues`), and rounds inside a run.
- On the Observability screen: cost by model, critique attempts per document, and the time from Ready to Review.
- Recording a Tech Leader's decision on a finding (`risk-accepted`).
- Driving the repository's own Actions by trigger label instead of agent-mux's flows.
- Editing an item's title, body, milestone or priority from agent-mux.
- The mouse on the Project screens.
- A repository with no `.spec-wave.json`.
