# Projects

A **Project** is a repository configured with spec-wave, opened in agent-mux: its epics, features, stories, tasks and bugs, read from its GitHub issues and its Projects v2 board, each with its stage, its documents, its pull requests and its next step.

Phases 1 and 2 are built: reading a Project, and the first changes to it (create an item, move a card, repair a stage, triage a Bug, bring the board up to its pull requests). Every change is one you ask for and confirm. agent-mux never writes `.spec-wave.json`. Running a step (spec, plan, decomposition, build) and the dev-agent fleet are the later phases of the design: `docs/superpowers/specs/2026-11-18-issue-based-project-management-design.md`.

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
| Ready | The Stories that are ready, under their Feature, in execution order |
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
| `n` | New work item, under the selected one when it can have children |
| `m` | Move the selected item to a later stage |
| `M` | Repair its stage: any stage, earlier ones included, with a reason |
| `A` | Make the board moves its pull requests already justify |
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

## 4. The next step

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

## 5. Commands

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
```

The first block reads. The second writes to GitHub at once, with no confirmation: typing the command is the confirmation. `advance` alone prints its plan and needs `--yes`. `task start` refuses while another Task of the same Story is in progress.

Every command takes `--dir DIR`, `--refresh` and `--cached`. A stage is typed as on the board or shorter: `--stage "code review"`, `--stage ready`.

`doctor` exits 1 when a line fails. Its marks: `✓` works, `!` works with something to know, `✗` must be fixed, `○` could not be checked. It reports, among others, issues that are not on the board, columns outside the flow, labels this version does not know, whether spec-wave's workflows are armed, and a disagreement between `execution.mode` in the file and the repository variable `SPEC_WAVE_EXECUTION`.

## 6. The cache

A read of GitHub takes a few seconds per hundred issues, so it is kept in the trace store (`project_items`, `project_boards`, schema v30) for `cache.ttlSec` of `.spec-wave.json`, 600 seconds when unset. The screens open on the cache at once and read again in the background when it is old. `--refresh` and `Ctrl+R` read now; `--cached` never reads.

The cache holds every open issue and the ones closed in the last 30 days. It is only a cache: delete the rows and `agent-mux project sync` brings everything back.

## 7. What is read, and how

Everything goes through `gh`; agent-mux reads no token. A verb reads the issue it acts on at that moment, never from the cache.

| Read | With |
| --- | --- |
| Issues: labels, milestone, parent, sub-issues, blocked-by, linked pull requests, and the board's `Etapa`, `Status` and other fields | one GraphQL query per 50 issues |
| Which documents exist on the default branch | one GraphQL query over `docs/features`, `docs/bugs`, `docs/rfcs` |
| Documents still in review | the open pull requests and the branches under `spec-wave/` |
| Pull requests of each code repository | `gh pr list`, linked by the hub issue each names (`Closes owner/hub#12`); GitHub's own link does not cross repositories |
| Milestones | the REST list |

A read that fails never becomes a fact. A document whose listing failed is `unknown`, not `missing`, and the snapshot carries a warning that Home and Health show.

## 8. Not built yet

- Running a step: spec, plan, critique, decomposition, bug document, build, QA.
- The other screens of the design (Specification, Technical review, Decomposition, Development, Code review, QA, Observability).
- Editing an item's title, body, milestone or priority from agent-mux.
- The mouse on the Project screens.
- A repository with no `.spec-wave.json`.
