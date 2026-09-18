# Development workbench UX plan

Date: 2026-09-18

Status: proposed product and UX roadmap; implementation is not started.

## Product direction

Make agent-mux a terminal workbench where developers build, try, inspect,
improve, and automate skills and agent behavior in a workspace.

The central journey is:

```text
Choose workspace → Open definition → Edit → Validate → Try
                                      ↑                 ↓
                                      └── Compare ← Inspect
                                              ↓
                                    Use in workflow or loop
```

Assumptions: retain the Rust/ratatui terminal application, external editors,
existing harness integrations, local trace storage, and file-based definitions.
The primary audience is a developer iterating on behavior across repeated runs.
This proposal is grounded in repository inspection, not observed usability tests.

## What exists and where the journey breaks

| Current capability | UX gap inferred from its structure | Proposed improvement |
| --- | --- | --- |
| Skills view shows installations and executions; Configuration edits files; Agents launches packages | One development task crosses three different entry points | A skill detail page connects definition, validation, launch, and results |
| Agents sidebar lists skill packages; traces contain actual subagents | “Agent” describes different things | Label skill launchers as Skills; use Agents for harness-native definitions and observed child agents |
| Configuration reports built-in, override, user, and workspace copies | Editing a file does not make deployment scope obvious | Show effective source, editable target, consumers, and stale copies together |
| Experiments compare variants and record pass/fail/unknown | Repeatable evaluation is not part of the main authoring journey | Add saved trial cases and compare results from definition pages |
| Loop readiness, budgets, inbox, files, and executions already exist | Understanding a blocked run requires navigating several representations | Show reason, evidence, and the next available action together |
| Loop “applied” removes the worktree and preserves a branch for manual merging | The word suggests integration has already happened | Use explicit review and cleanup language; distinguish retained work from integrated work |

Evidence: `docs/skills.md`, `docs/configuration.md`, `docs/loops.md`,
`docs/workflows.md`, `src/app.rs`, `src/app/skills_view.rs`,
`src/app/loops.rs`, and `src/tracing/experiments.rs`.

## Alternatives and recommendation

1. **Connected terminal workbench — recommended.** Give existing capabilities a
   shared navigation and development journey, then add trial provenance and
   comparisons. Preserves the runtime investment and delivers useful increments.
2. **Navigation polish only.** Improve names, help, and shortcuts. Lowest scope,
   but leaves repeatable testing and definition-to-result attribution unresolved.
3. **New desktop/web studio.** More space for editors and visual graphs, but adds
   a UI platform and service boundary before validating the development journey.
   Reconsider only after terminal usability studies identify concrete limits.

## Information architecture

Use a persistent workspace selector and four destinations. A visible scope
indicator distinguishes the selected workspace from “All workspaces.” Global
definitions remain visible with their scope labeled.

```text
Workspace: agent-mux                         Harness: inherited per run

Build                 Run                 Review             Settings
  Skills                Sessions           Needs attention     Profiles
  Agents                Workflows          Comparisons         Library defaults
  Loops                 History            Run results        Diagnostics
```

Build contains definitions; Run contains execution and launch entry points;
Review contains outcomes and decisions. A loop's definition and its runs link
to each other. Workflows remain the composition mechanism for multi-step work.
Start with links and labels in the current shell; migrate navigation gradually.

Use one detail-page pattern: Overview, Definition, Trials, Runs, and Usage.
Only show relevant tabs; loop pages also expose Schedule and Gates. Usage shows
known consumers with evidence, without claiming a complete dependency graph.

Keep important actions visible in a contextual footer and searchable action
palette. Offer New, Edit, Validate, Try, Compare, and View runs by name. Preserve
existing shortcuts as aliases during migration. Palette keys must be selected
after auditing current bindings; attached terminal input remains owned by the
harness, with the existing detach boundary.

At narrow terminal widths, show one pane with a breadcrumb and Back action.
Remember selection, tab, and scroll position on returning from an editor or run.
Statuses must use words or symbols as well as color. Empty states provide a
specific next action; unsupported actions explain the missing capability.

## Core development journeys

### 1. Develop a skill

New skill → choose scope and template → edit in the existing editor → validate
→ enter a trial prompt and expected result → select harness/profile → run →
inspect result and evidence → edit and rerun → compare with baseline.

- Start with managed skill packages and existing scaffolding. Show native skills
  in inventory, with their source path and ownership. Editing an unmanaged source
  must be explicit; never silently adopt or overwrite it.
- Show syntax/metadata errors before launch, with file and location when known.
- Preview workspace, exact source revision, harness, profile, permissions,
  installation changes, and supported limits before starting a trial.
- Treat a successful launch, observed skill use, and passing an expectation as
  separate facts. Missing trace evidence means unknown, not “skill not used.”
- Keep the current interactive skill singleton behavior visible. Initial trials
  can run sequentially; concurrent variants require separately scoped execution
  support and must not silently replace a running skill session.
- Deployment shows destination paths and diffs. Scope choices reflect supported
  installation behavior; project-level package deployment needs an explicit
  extension beyond today's user-level package installer.

### 2. Develop agent behavior

Agents means harness-native agent definitions plus observed subagent executions;
it does not introduce a second agent-mux package format.

- Inventory definitions by workspace, harness, and path. Separate definitions
  from running instances and trace observations.
- Show instructions, model/tool settings where available, skills referenced,
  source provenance, and harness support. Unsupported fields stay explicit.
- First support editing existing loop verifier definitions through the current
  library and inspecting observed subagents. Add native definition authoring per
  harness only after probing installed CLIs and validating their format.
- A trial invokes the agent through its supported parent harness flow. Show
  parent prompt, delegation input where captured, child result, tool calls, and
  verifier verdict where available.
- Link observations to definitions only when identity/version evidence permits;
  ambiguous names show candidate matches, not a fabricated exact association.

### 3. Develop a loop

Choose pattern → inspect skills/agent dependencies → configure workspace and
bounds → check readiness → run once → review output → enable cadence.

- Make “Run once” and “Enable schedule” separate, explicit actions. Creating a
  development draft should not accidentally start recurring work.
- Separate preflight preview, which starts no model, from a real report-only
  rehearsal, which consumes resources and can update permitted state files.
- Explain configured versus effective level in plain language, including why
  a run was downgraded. Example: “Report-only: today's token usage reached 80%.”
- Show next run with timezone, concurrency status, budget remaining, pause
  reason, and kill-switch state in one overview.
- Review displays proposed diff, verifier evidence, gate results, branch, and
  worktree path. “Reviewed; keep branch” must not claim the change was merged.
- Cleanup must account for uncommitted changes before removing a worktree.
  Reject/cleanup previews its exact effects; failure leaves a recoverable state.
- Promotion to a higher level remains an explicit user action subject to the
  existing Rust readiness and harness capability checks. Preserve no-push rules.

## Repeatable trials and comparison

Extend the existing experiment model instead of creating another runner.
Each saved case identifies prompt, fixture/workspace revision, expected result,
check command or manual rubric, and execution settings. Runs retain a snapshot
or content hash of relevant definitions and references, actual harness/model
settings, input revision, and trace links. Historic runs without these facts are
labeled as lacking provenance.

Present task outcome first, followed by evidence, changes, duration, tokens, and
cost when available. Do not present missing cost as zero or fewer tokens as
better quality. Baseline comparisons show sample counts and unknown outcomes;
comparisons with different inputs/settings are flagged as non-equivalent.

Check commands are user-selected executable commands and must be visible before
running. Failed checks, cancelled runs, unavailable harnesses, and incomplete
traces remain distinct states. A rerun creates a new result and preserves the old
one. “Rerun same snapshot” and “Try current definition” are separate actions.

Heimdall can explain recorded evidence and suggest a next experiment. Keep its
read-only analysis role; the workbench owns edits, installations, and launches.

## Delivery sequence and acceptance criteria

Each phase should be split into implementation tasks after its UX is reviewed.
The paths below are existing integration points, not a mandate to expand the
already large `src/app.rs` and `src/ui.rs`; extract focused navigation/action
modules as needed when implementing.

| Phase | Deliverable and integration points | Acceptance criteria |
| --- | --- | --- |
| 1. Connect current capabilities | Skill detail actions, return navigation, source/scope labels, explicit loop review copy. `src/app.rs`, `src/ui.rs`, `src/app/skills_view.rs`, `src/app/config_view.rs`, `src/app/loops.rs` | A developer finds a skill, edits its source, validates, launches, and opens its execution without searching for another top-level screen. Editor return preserves context. Loop review never claims a merge occurred. |
| 2. Establish the workbench shell | Workspace scope, named destinations, searchable actions, contextual help, narrow-layout behavior. `src/app.rs`, `src/ui.rs`, `src/keys.rs`, `src/persistence.rs` | Every phase-1 action is discoverable by name. Legacy shortcuts still work. Workspace switches do not retarget active runs. Attached input is unchanged. |
| 3. Close the skill iteration cycle | Saved trials, immutable provenance, baseline comparisons. `src/skill/`, `src/tracing/experiments.rs`, `src/tracing/scores.rs`, `src/tracing/store/schema.rs` | Run two definition revisions against one saved case; inspect both snapshots, outcomes, and evidence side by side. Missing metrics remain unknown. Existing stores migrate without rewriting historic evidence. |
| 4. Expose agent development | Existing verifier editing, definition inventory, parent/child drill-down, capability-aware native authoring. `src/tracing/inventory.rs`, `src/tracing/subagents.rs`, `src/assets.rs`, `src/app/config_view.rs` | A developer changes a verifier, runs a supported parent flow, and sees whether it delegated and what evidence the child produced. Unsupported harness flows explain why. No new package type is introduced. |
| 5. Make loops developable | Draft/run-once/schedule flow, preflight explanation, review and deployment diffs. `src/app/loops_view.rs`, `src/app/loops.rs`, `src/loops/readiness.rs`, `src/loops/scaffold.rs`, `src/loops/worktree.rs` | A developer rehearses a loop before scheduling, explains a blocked/downgraded run from one page, and reviews a proposed fix without losing uncommitted work. |
| 6. Connect composition and guidance | Definition-to-workflow navigation, dependencies, relevant Heimdall evidence links. `src/app/workflows_view.rs`, `src/app/workflows.rs`, `src/workflows/`, `src/tracing/analysis/` | From a workflow failure, reach the relevant step, definition, and trial with context preserved. Analysis remains read-only. |

Dependencies: phases 1–2 establish navigation; phase 3 establishes repeatable
evidence; phases 4–5 use those conventions; phase 6 connects the full journey.
The loop review wording and cleanup audit in phase 1 should not wait for phase 5.

## Validation and success measures

Before implementation, walk the current UI through three tasks and record the
baseline: revise a skill and inspect its result; investigate a verifier failure;
create and rehearse a loop before scheduling it. Repeat with a small prototype,
then with each implemented slice. Recruit both an experienced user and someone
unfamiliar with the shortcuts; a small sample identifies problems, not statistical
proof of usability.

Proposed targets, to calibrate against that baseline:

- First successful skill trial within five minutes, assuming a working harness.
- Locate definition source and latest result within three deliberate actions.
- Explain a blocked loop and identify its next step within one minute.
- Complete edit → rerun → compare without losing workspace or object selection.
- Every reviewed trial exposes its provenance status; every cleanup action names
  what is retained and removed.

Use Rust state/action tests for navigation and scope, ratatui render tests for
narrow screens and empty/error states, temporary-file integration tests for editor
return and deployment previews, and fixture-backed tests for provenance and
readiness explanations. Existing starting points include `tests/skill_ui.rs`,
`tests/scroll_ux.rs`, `tests/loop_cli.rs`, `tests/workflow_cli.rs`, and
`tests/trace_scores.rs`. Run build, test, clippy, and fmt checks for implementation
changes. Use opt-in live harness trials only for adapter behavior that fixtures
cannot establish; probe installed CLI help and record the results in module docs.

## Boundaries

No new GUI platform, built-in code editor, marketplace, autonomous promotion,
automatic merge, or universal cross-harness agent format in this roadmap.
Preserve CLI parity for new operations and keep analysis paths read-only.
The recommended first release is phases 1–2; the first complete skill-development
release includes phase 3.
