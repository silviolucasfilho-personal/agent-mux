# Skills View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Skills sidebar section and the Trace Browser `K` pane with one dedicated Skills view (`S`) that lists skills per harness with install state and execution history, and keeps the main pane on the session terminal.

**Architecture:** A new `Mode::SkillsView` owning `SkillsViewState` (`src/app/skills_view.rs`), rendered by `draw_skills_view` in `src/ui.rs`, fed by `skill::load_skills`, `skill::install::status`, `inventory::inventory_all`, `inventory::skill_reports`, and two new read queries. Launch rows gain `metadata.skill_id` / `skill_harness` so executions are exact.

**Tech Stack:** Rust 2024, ratatui, rusqlite; no new dependencies.

**Spec:** [Skills view design](../specs/2026-09-15-skills-view-design.md)

## Global Constraints

- The main pane always shows the selected session terminal (or the history preview when there are no sessions). No skill content is drawn there.
- The view reads the store through `store::open_ro`; writes are limited to `skill::install` / `uninstall` on the filesystem. No schema migration.
- Open questions resolved for this plan: native harness skills are listed (read-only rows); the row's harness is the launch harness (no separate picker); uninstall is per row.
- Tests never spawn a real harness or write into the real home: launches use a temporary profile whose command is a fake `claude` script and an install home override on `App`.
- `agent-mux skill …` and `agent-mux trace skills` output is unchanged.

---

## Execution map

Baseline: `f5e5166` on `docs/onboarding-readme`.

| Task | Files | Verify |
| --- | --- | --- |
| 1 Store evidence and queries | `src/tracing/store/query.rs`, `src/tracing/mod.rs`, `src/app.rs` (spawn path), `src/skill/launch.rs` | `tests/skill_package.rs`, `tests/trace_store.rs` |
| 2 View state | `src/app/skills_view.rs`, `src/app.rs` (mode, actions, keys, mouse, tick, briefing gate) | `tests/skill_ui.rs` |
| 3 Rendering | `src/ui.rs` (`draw_skills_view`, remove sidebar section, `K` pane, launcher, hints, help) | `src/ui.rs` unit tests, `tests/persistent_sessions.rs` |
| 4 Test migration | `tests/skill_ui.rs`, `tests/trace_inventory.rs`, `tests/persistent_sessions.rs` | `cargo test` |
| 5 Documentation | `README.md`, `docs/skills.md`, `skills/heimdall/reference/agents.md`, `AGENTS.md`, spec status | review |

### Task 1: Store evidence and queries

- [x] `LaunchPlan` gains `pub skill: Option<(String, String)>` (`id`, harness label); both planners set `None`. `start_session` writes `skill_id` and `skill_harness` into the started launch row's metadata.
- [x] `App::spawn_traced_with_env` takes `skill_id: Option<&str>` and sets `plan.skill` with `Harness::detect(&profile.command)`; `launch_skill`, `restore_saved_sessions` and `respawn_selected` pass the session's skill id (respawn currently loses it).
- [x] `query::skill_launches(conn, skill_id, session_name, provider, limit)` per the spec's SQL, returning `SkillLaunch { id, provider, profile, cwd, started_ns, ended_ns, termination, exit_code, session_key, by_id, turns, total_cost_usd, live }`.
- [x] `query::traces_with_skill_detail(conn, skill, limit)` returning `SkillTurn { stat: TraceStat, attributed: bool }`.
- [x] `build_skill_launch` keeps the base profile's command when it already detects as the target harness, so a configured wrapper path is honoured.
- [x] Tests: `tests/trace_inventory.rs` covers id-first matching, the profile-name fallback and the live flag on a temp store written by `open_rw`.

### Task 2: View state and key handling

- [x] Create `src/app/skills_view.rs` with `SkillsViewState`, `SkillRow`, `SkillsPane`, `SkillsTab`, `SkillExecution`; loading (`reload`), row building grouped by harness with native rows deduplicated against installed packages, filter toggle, navigation that skips headers, executions loading, detail line building, `refresh_if_live` (500 ms, executions only).
- [x] `Mode::SkillsView(Box<SkillsViewState>)`, `Action::OpenSkillsView` (`S`), `Action::SkillsKey`; remove `Mode::SkillLauncher`, `SkillLauncherState`, `Action::OpenSkillLauncher`, `Action::SkillLauncherKey`, `SidebarSection::Skills`, `App.selected_skill`, `reload_skills` callers on `Tab`.
- [x] `App::handle_skills_key`: `Esc`/`q` unwind, `Tab`/`BackTab` tabs, `←/→` focus, `j/k` movement, `1-3`/`c x a` filter, `Enter` launch or attach, `i`/`I` install, `u` confirm then uninstall, `r` rescan, `T` open the Trace Browser on the selected execution, paging keys.
- [x] `App.skill_install_home: Option<PathBuf>` override used by `launch_skill`, install and uninstall (tests only).
- [x] `TraceBrowserState::focus_session(key, trace_id)` to position the browser.
- [x] Mouse: wheel in the view moves the focused list or scrolls; the uninstall confirmation is a `pending_uninstall` flag rendered in the view's footer.
- [x] `on_tick` refreshes the view; `refresh_briefing_if_needed` gates on the Briefing tab.

### Task 3: Rendering

- [x] `sidebar_areas(total_height) -> (Rect, Rect)` with `[Percentage(40), Min(4)]`; delete `draw_skills_sidebar`, `draw_generic_agent_preview`, `draw_skills_pane`, `draw_skill_launcher`; `draw_main` no longer branches on Skills.
- [x] `draw_skills_view`: geometry like the browser, left list with headers and state column, right pane with tab title, Details / Executions / Briefing bodies, footer hints; the briefing body reuses `draw_trace_briefing_preview` with its footer retargeted.
- [x] Status-bar hints and the help overlay mention `S` and the view's keys.

### Task 4: Test migration

- [x] Rewrite `tests/skill_ui.rs`: open with `S`, grouping and header skipping, filter, running row and attach, native row notice, launch on the row's harness through a fake `claude` and a temp install home, install/uninstall state changes, `Esc` unwind, tab count by capability.
- [x] `tests/trace_inventory.rs`: replace the browser `K` test with the view's Executions/Details expectations over the same seeded store.
- [x] `tests/persistent_sessions.rs`: two-section sidebar navigation and clicks.
- [x] `src/ui.rs` unit tests: help lists `S`, hints mention `[S] skills`.

### Task 5: Documentation

- [x] README sections 4, 8, 11, 14 per spec section 9; `docs/skills.md` section 4; Heimdall `reference/agents.md` query; `AGENTS.md`; spec status line.
