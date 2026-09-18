# Skill Workbench Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connect skill editing, validation, launch, and recorded executions in the existing Skills view while preserving the selected skill and harness.

**Architecture:** Keep `SkillsViewState` responsible for selection and presentation, and let `App` perform filesystem and launch actions. Add stable selection restoration to the view, use the existing configuration catalog to materialize built-in skill overrides, and launch the selected row's harness directly. The editor round-trip stays in `SkillsView` and reloads the same selection.

**Tech Stack:** Rust 2024, ratatui, crossterm, existing skill/configuration library and trace store.

**Spec:** `docs/superpowers/plans/2026-09-18-development-workbench-ux.md`

## Global Constraints

- Preserve the terminal UI, external editor flow, existing harness integrations, and local trace store.
- Agent-mux continues to manage skills as its only package format.
- Native harness skills remain inspectable but are not silently adopted or overwritten.
- A launch, observed skill use, and a passing expectation remain separate facts.
- Existing keyboard shortcuts remain valid.

---

### Task 1: Stable skill selection and baseline isolation

**Files:**
- Modify: `src/app/skills_view.rs`
- Modify: `tests/persistent_sessions.rs`
- Test: `tests/skill_ui.rs`

**Interfaces:**
- Produces: `SkillsViewState::select_package(&mut self, id: &str, harness: Harness) -> bool`
- Produces: `SkillsViewState::selected_identity(&self) -> Option<(String, Harness)>`

- [x] **Step 1: Write a failing test** proving a reopened Skills view can restore a package and harness by stable identity.
- [x] **Step 2: Run** `cargo test --test skill_ui workbench_restores_the_selected_skill_and_harness -- --exact` and confirm it fails because the selection API is absent.
- [x] **Step 3: Implement** public stable identity access and selection using the existing row key logic, then reload executions and details through `on_selection_changed`.
- [x] **Step 4: Run the focused test and** `cargo test --test persistent_sessions`.
- [x] **Step 5: Commit** the stable navigation behavior.

### Task 2: Edit and validate from the Skills view

**Files:**
- Modify: `src/app.rs`
- Modify: `src/ui.rs`
- Test: `tests/skill_ui.rs`

**Interfaces:**
- Consumes: selected managed package from `SkillsViewState::selected_package`.
- Produces: `e` materializes a built-in override or opens a user package's `SKILL.md`; `v` reloads the selected package with `skill::load_skill_dir` and reports a concrete validation result.

- [x] **Step 1: Write failing tests** proving `e` creates and opens `skills/<id>/SKILL.md`, the editor return stays on the selected skill, and `v` reports valid or invalid package content.
- [x] **Step 2: Run the focused tests** and confirm they fail because Skills view keys do not edit or validate.
- [x] **Step 3: Implement** `edit_selected_skill`, `validate_selected_skill`, and Skills-view editor reload. Built-ins use `Catalog::create_override`; user packages open their own `SKILL.md`. Native skills receive a read-only notice.
- [x] **Step 4: Update** the Skills footer and empty execution hint to advertise named actions.
- [x] **Step 5: Run** `cargo test --test skill_ui` and commit.

### Task 3: Launch, rerun, and inspect without losing context

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/skills_view.rs`
- Modify: `src/ui.rs`
- Modify: `docs/skills.md`
- Test: `tests/skill_ui.rs`

**Interfaces:**
- Consumes: `(skill_id, harness)` from the selected package row.
- Produces: `l` launches or attaches using `App::launch_skill`; opening `S` again restores the most recent workbench selection; the Executions tab continues to refresh and `T` opens the selected trace.

- [x] **Step 1: Write failing tests** proving `l` launches the selected harness and reopening `S` restores that row with Executions selected.
- [x] **Step 2: Run the focused tests** and confirm the missing behavior.
- [x] **Step 3: Add** an app-level skill workbench bookmark, update it before edit/launch/trace transitions, and apply it in `open_skills_view`.
- [x] **Step 4: Document** the edit/validate/launch/trace journey and shortcuts in `docs/skills.md`.
- [x] **Step 5: Run** `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo build`.
- [x] **Step 6: Commit** the complete slice.
