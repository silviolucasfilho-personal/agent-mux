# Consistent Keyboard UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make keyboard actions discoverable and consistent across agent-mux screens without disturbing harness input.

**Architecture:** A shared action registry supplies context-sensitive dispatch, menu entries and hints. Independent overlays preserve the underlying mode, search and text buffers; navigation uses existing dirty-editor decisions. Session-specific key forwarding remains the final input path.

**Tech Stack:** Rust, crossterm, ratatui, existing App state and integration fixtures; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-11-consistent-keyboard-ux-design.md`

## Global Constraints

- In a session pane, ordinary keys, Ctrl letters, Alt, Esc, Tab and Enter remain harness input.
- No new plain Ctrl, Alt, Esc or Tab binding is reserved on the main session pane.
- A binding is verified only when its actual event reaches the app.
- Store the preferred observed binding per action in the current app session.
- Destructive actions retain confirmation and dirty editors retain their unsaved-change decision.
- Work on `feature/consistent-keyboard-ux`; never commit or push to master.

## Review Focus

- A session exits or is removed while Commands is open: recheck availability before execution (task 1/2).
- Opening Commands inside search or a text field: preserve the buffer and cursor on dismissal (task 2).
- Canceling navigation from a dirty editor: preserve the editor and discard the pending destination (task 3).
- Ctrl+Shift collapses into Ctrl: forward the ordinary key and never treat it as a verified chord (task 5).
- A narrow terminal hides hints: retain Commands and a textual focus indicator without partial key labels (task 6).

## File map

- `src/keymap.rs`: ActionId, ActionDef, scopes and binding metadata; existing text and normalization helpers.
- `src/app/commands.rs` (new): availability, context, overlays and action execution.
- `src/app/shortcut_probe.rs` (new): session-local observed shortcut state and test flow.
- `src/ui/commands.rs` (new): menu and probe rendering.
- `src/app.rs`: minimal module declarations, App fields and dispatch integration.
- Existing `src/app/{agent_editor,flow_builder,runs_view,markdown_view}.rs`: local controls and dirty-state navigation.
- `src/ui.rs` and existing per-screen UI modules: shared hints, focus and numbered tabs.
- `tests/it/keyboard_ux.rs` (new), registered in `tests/it/main.rs`: behavioral screen matrix using existing fixtures.
- `docs/keyboard.md`, `AGENTS.md`: the new keyboard contract.

### Task 1: Shared actions and availability

**Files:** `src/keymap.rs`, new `src/app/commands.rs`, `src/app.rs`, new `tests/it/keyboard_ux.rs`, `tests/it/main.rs`.

**Interfaces:** Produce `ActionId`, `ActionDef { id, label, keywords, scope }`, `ACTIONS`, `Availability::{Enabled, Disabled(String)}`, and `App::command_availability(&self, id: ActionId) -> Availability`. Expose `App::execute_command(&mut self, id: ActionId, now: Instant)`; it checks current availability before effects. Action IDs cover Commands, Help, FocusAgents, ReturnToSession, ToggleSidebar, Quit, Traces, Runs, New, Edit, Delete, Run, Stop, Save, Reload, Restore, Editor, SetVerdict, TestShortcuts and previous/next session.

- [ ] Add failing `command_availability_tracks_current_context`: ReturnToSession is disabled with no sessions, enabled with a live session; Save is disabled in read-only screens; ToggleSidebar is disabled in an editor without a sidebar; a removed selected item makes its command unavailable.
- [ ] Run `cargo test --test it keyboard_ux::command_availability_tracks_current_context`; expect failure until the API exists.
- [ ] Implement the registry and availability adapter using existing selected-item and screen state. Reuse existing action effects; do not duplicate launches or confirmation logic.
- [ ] Run the new test and existing dispatch tests with `cargo test dispatch_tests`; expect pass. Commit `feat: centralize keyboard action availability`.

### Task 2: Global command and help overlays

**Files:** `src/app/commands.rs`, new `src/ui/commands.rs`, `src/app.rs`, `src/ui.rs`, `src/keymap.rs`, `tests/it/keyboard_ux.rs`.

**Interfaces:** Consume task 1 APIs. Produce `CommandMenu { query: String, selected: usize }`, `Overlay::{Commands(CommandMenu), Help}`, an `App::command_overlay: Option<Overlay>` independent of Mode, and `App::handle_command_overlay(&mut self, key: &KeyEvent, now: Instant) -> bool`. Renderer consumes filtered ActionDefs and current availability.

- [ ] Add failing `commands_open_on_every_screen` over Main list/pane, Trace Browser, Skills, Configuration, Agent Editor, Task Library, Flow Builder, Runs, Inbox and Markdown: F1 opens Commands; Esc restores each screen. Add `overlay_preserves_text_and_search` asserting unchanged buffers/cursors after opening and dismissing. Add `disabled_command_cannot_execute` and `help_and_commands_do_not_stack`.
- [ ] Run `cargo test --test it keyboard_ux`; expect new tests to fail.
- [ ] Implement F1 as Commands, retain help chords and local ? as Help, process overlay input before search and screen handlers, and filter action labels/keywords. Keep disabled reasons visible and recheck before Enter execution. Preserve underlying Mode instead of replacing it with Help. If an action opens a confirmation, close the menu and use the existing confirmation.
- [ ] Run `cargo test --test it keyboard_ux` and existing help/search tests; update only expectations intentionally changed by the spec. Commit `feat: add a global searchable command menu`.

### Task 3: Explicit navigation and dirty-state handling

**Files:** `src/app/commands.rs`, `src/app.rs`, `src/app/agent_editor.rs`, `src/app/flow_builder.rs`, `src/app/markdown_view.rs`, `tests/it/keyboard_ux.rs`.

**Interfaces:** Produce `NavigationTarget::{Agents, Session(String)}` and `App::request_navigation(&mut self, target: NavigationTarget, now: Instant)`. Use stable session IDs, not list offsets. Preserve originating session ID when entering app screens. Retain F2's legacy toggle on Main; explicit actions remain separate.

- [ ] Add failing `focus_agents_reveals_without_hiding`, `return_to_originating_session`, `hidden_sidebar_transfers_focus`, and `dirty_navigation_cancel_preserves_editor`: repeated FocusAgents leaves Agents visible/focused; return selects the origin despite intervening list movement; hide transfers focus; cancel preserves text and prevents later unrelated navigation.
- [ ] Run `cargo test --test it keyboard_ux`; expect failures.
- [ ] Implement target tracking and navigation through existing unsaved-change decisions. On a missing origin, use the selected session if available; otherwise disable ReturnToSession. ToggleSidebar never exits an editor. Recheck target validity when a delayed decision completes.
- [ ] Run keyboard tests plus `cargo test --test it flow_builder_ui` and Markdown tests; expect pass. Commit `feat: make agent and session navigation explicit`.

### Task 4: Consistent local controls and text behavior

**Files:** `src/keymap.rs`, `src/app.rs`, existing app editor modules, `src/ui/{agent_editor,flow,runs,loop_builder,markdown}.rs`, `tests/it/keyboard_ux.rs`, existing control tests.

**Interfaces:** Consume ActionId registry. Expose `App::local_command(&self, key: &KeyEvent) -> Option<ActionId>` for non-text controls only. Existing text handlers remain responsible for cursor/editing semantics; registry maps Ctrl+S to enclosing-editor Save.

- [ ] Add failing `save_never_changes_verdict`, `tab_moves_focus_not_tabs`, `digits_select_visible_numbered_tabs`, `multiline_enter_inserts_singleline_enter_submits`, and `editor_arrows_do_not_scroll_session`. Test plain letters in text fields stay literal and unmatched modified letters never become local verbs.
- [ ] Run keyboard tests and targeted existing editor/control tests; expect the specified behavior changes to fail.
- [ ] Replace conflicting raw matches with shared commands. Move verdict selection to the named SetVerdict menu action; remove s/+ verdict aliases. Make Tab/Shift+Tab traverse controls, use visible numbered tabs for tab selection, remove undocumented session digit selection, and keep legacy q Back/Quit only outside text fields. Enter inserts in multiline fields and Ctrl+J remains its alias; Ctrl+S saves, Esc finishes field editing first. Keep existing confirmation semantics safe and explicit.
- [ ] Run `cargo test --test it keyboard_ux`, existing text-area tests and editor/trace tests. Commit `fix: unify local keyboard controls across screens`.

### Task 5: Observed shortcut preferences and probe

**Files:** new `src/app/shortcut_probe.rs`, `src/keymap.rs`, `src/app/commands.rs`, `src/ui/commands.rs`, `src/app.rs`, `src/keys_cli.rs`, `tests/it/keyboard_ux.rs`.

**Interfaces:** Produce `ShortcutState` storing session-local observed bindings keyed by ActionId, `ShortcutProbe` for Commands/FocusAgents/Sidebar/Help/Traces, `ShortcutState::observe(&mut self, id: ActionId, key: KeyEvent)` and `ShortcutState::preferred_label(&self, id: ActionId) -> Option<String>`. Probe consumes normalized events without running their actions. Reuse `keys::normalize_event`; retain F1 as the known menu entry.

- [ ] Add failing unit tests `probe_records_only_matching_events`, `collapsed_ctrl_is_not_verified`, `skip_and_cancel_preserve_preferences`, and integration test `probe_does_not_execute_actions`. Observed alias becomes preferred; unsupported candidate is never advertised as verified; cancel closes probe without navigation.
- [ ] Run `cargo test shortcut_probe`; expect failures before implementation.
- [ ] Implement TestShortcuts menu action and interactive probe with skip/cancel. Modifier protocol support alone records no preferred modifier chord. Distinguish candidate aliases in Help from observed preferences; actions lacking verified shortcuts remain reachable through Commands. Do not persist preferences across app launches.
- [ ] Run probe tests and keyboard integration tests. Commit `feat: verify delivered shortcuts before advertising them`.

### Task 6: Focus, shared hints, documentation and whole-feature verification

**Files:** `src/ui.rs`, `src/ui/commands.rs`, existing UI modules, `src/app.rs` mouse handling, `docs/keyboard.md`, `AGENTS.md`, `tests/it/keyboard_ux.rs` and UI tests.

**Interfaces:** Consume action availability and preferred labels. Produce shared hint entries from ActionDefs; preserve existing fit_hints truncation behavior. Register clickable Commands/FocusAgents/ReturnToSession footer hit regions using actual rendered geometry.

- [ ] Add failing `footer_uses_current_context`, `narrow_footer_keeps_commands_and_focus`, `focus_has_text_and_border`, and `footer_click_executes_same_action`. Assert backgrounds do not leak hints, disabled actions show no active shortcut, and narrow labels are never split.
- [ ] Run UI and keyboard tests; expect failures.
- [ ] Implement textual focus indicators and distinct border, one preferred binding per action, shared footer/help content, clickable controls and narrow layout priorities. Update keyboard docs and repository guidance for F1 Commands, preserved F2 toggle, explicit navigation, text behavior and shortcut testing.
- [ ] Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test`; record complete results. Investigate the known memory opt-out test's inherited AGENT_MUX_MEMORY environment separately; if needed rerun with that session-only environment removed and report both outcomes, without quietly changing memory code.
- [ ] Manually run `agent-mux keys` and exercise menu, dirty navigation, probe and harness forwarding in the installed terminal. Record terminal/version and actual events; if interactive terminal access is unavailable, report manual verification as outstanding.
- [ ] Review the whole diff against the spec, fix discovered defects, rerun only relevant checks after fixes, and commit `feat: expose consistent keyboard focus and action hints`. Keep the feature branch for review; publishing a PR is a separate user action.

## Self-review

All spec sections map to tasks above. Task 1's IDs and availability are consumed consistently by tasks 2–6; no new Mode is required for overlays. Navigation uses stable session IDs and existing save/cancel behavior. The five review conditions each have a named owning test. Verification includes dispatch, rendering, text preservation and real terminal observations rather than asserting terminal compatibility from documentation.
