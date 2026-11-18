# Projects

A **Project** is a repository configured with spec-wave, opened in agent-mux: its epics, features, stories, tasks and bugs, read from its GitHub issues and its Projects v2 board, each with its stage, its documents, its pull requests and its next step.

Phase 1 is built, and it only reads. agent-mux changes nothing on GitHub and never writes `.spec-wave.json`. Running a step, moving a card, triage and the dev-agent fleet are the later phases of the design: `docs/superpowers/specs/2026-11-18-issue-based-project-management-design.md`.

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

**The item screen** (`Enter` on an item) has five tabs: Card, Documents, Children, Pull requests and Thread. `Enter` opens what the cursor is on: a document on the default branch in the Markdown viewer (from the hub's checkout), a document still in review as its pull request in the browser, a child as its own item screen, a pull request in the browser. `Esc` goes back.

A `!` at the right of a row means the item waits on you. A `~` beside a stage means it was derived, because the issue is not on the board.

## 3. The next step

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

## 4. Commands

```
agent-mux project doctor [--json]
agent-mux project sync
agent-mux project ls [--type T] [--stage S] [--milestone M] [--all] [--json]
agent-mux project show <n> [--json]
agent-mux project next <n> [--json]
agent-mux project order [<feature>] [--milestone M] [--json]
```

Every command takes `--dir DIR`, `--refresh` and `--cached`. A stage is typed as on the board or shorter: `--stage "code review"`, `--stage ready`.

`doctor` exits 1 when a line fails. Its marks: `✓` works, `!` works with something to know, `✗` must be fixed, `○` could not be checked. It reports, among others, issues that are not on the board, columns outside the flow, labels this version does not know, whether spec-wave's workflows are armed, and a disagreement between `execution.mode` in the file and the repository variable `SPEC_WAVE_EXECUTION`.

## 5. The cache

A read of GitHub takes a few seconds per hundred issues, so it is kept in the trace store (`project_items`, `project_boards`, schema v30) for `cache.ttlSec` of `.spec-wave.json`, 600 seconds when unset. The screens open on the cache at once and read again in the background when it is old. `--refresh` and `Ctrl+R` read now; `--cached` never reads.

The cache holds every open issue and the ones closed in the last 30 days. It is only a cache: delete the rows and `agent-mux project sync` brings everything back.

## 6. What is read, and how

Everything goes through `gh`; agent-mux reads no token.

| Read | With |
| --- | --- |
| Issues: labels, milestone, parent, sub-issues, blocked-by, linked pull requests, and the board's `Etapa`, `Status` and other fields | one GraphQL query per 50 issues |
| Which documents exist on the default branch | one GraphQL query over `docs/features`, `docs/bugs`, `docs/rfcs` |
| Documents still in review | the open pull requests and the branches under `spec-wave/` |
| Pull requests of each code repository | `gh pr list`, linked by the hub issue each names (`Closes owner/hub#12`); GitHub's own link does not cross repositories |
| Milestones | the REST list |

A read that fails never becomes a fact. A document whose listing failed is `unknown`, not `missing`, and the snapshot carries a warning that Home and Health show.

## 7. Not in phase 1

- Any write: creating an item, moving a card, triage, applying a decomposition, running a step, merging.
- The other screens of the design (Specification, Technical review, Decomposition, Development, Code review, Bugs, QA, UAT and release, Observability).
- The mouse on the Project screens.
- A repository with no `.spec-wave.json`.
