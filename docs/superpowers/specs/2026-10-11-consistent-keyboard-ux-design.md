# Consistent keyboard UX

## Intent and scope

The user reports inconsistent shortcuts and actions that do not work across screens. The feature gives agent-mux one discoverable command system, predictable navigation and explicit keyboard focus while preserving ordinary keyboard input to the coding harness. The approved direction is a searchable command menu, stable local verbs, visible focus and shortcuts verified against the terminal.

This changes action dispatch and presentation across existing screens. It does not change session execution, scheduling or workflow semantics. It is based on the merged shared-memory baseline, commit 685284e.

## Current evidence

`src/keymap.rs` defines verbs and chords, but `App::handle_ux_key` in `src/app.rs` mainly handles chords in Main. TraceBrowser separately accepts sidebar and traces chords. Other screens do not share that global path. `chord_label_for` can advertise a modifier chord without reporting support when the action has no bare fallback. The Trace Browser uses `s` for a verdict despite the shared Save meaning. Tab changes sections on Main and tabs in some editors, while elsewhere it changes fields. F2 changes focus; Command+B and b change sidebar visibility, so these are distinct actions that need distinct labels.

## Chosen approach

Use a shared action registry and a command menu as the backbone. Keeping only existing aliases would leave discoverability and screen dispatch unresolved. Moving every action to Command chords would depend on terminal interception and reporting. The registry preserves efficient local keys and supplies a complete menu path to essential actions.

## Actions and availability

Add a registry alongside `src/keymap.rs`, with stable action IDs, labels, optional key bindings, context scope and availability. Availability is either enabled or disabled with a specific reason. Context carries the screen, focused control, selected entity and originating session where one exists. Menu execution rechecks availability; a stale selection must not execute an action against a different item.

Dispatch, menu entries, help and footer hints read the same definitions. Existing screen handlers implement action effects, but registered shortcuts must not also have conflicting independent matches. Controls with specialized editing behavior keep their input handlers.

App-wide actions include Commands, Help, Focus Agents, Return to Session, Toggle Sidebar and Quit. Screen-specific actions include Open Traces, Open Runs, New, Edit, Delete, Run, Stop, Save, Reload, Restore and Open in Editor. Scope-specific operations use their own named actions: Set Verdict must not borrow Save's `s` key. Do not reinterpret a modified character as a plain local verb when its chord is unsupported in that context.

## Navigation and focus

Focus Agents reveals the sidebar, returns to the main Agents list and focuses its selected row. It never hides the sidebar. Return to Session returns to Main and focuses the originating session, or the selected session when no origin exists; it is disabled if neither exists. Leaving a dirty editor through these actions uses the existing unsaved-change decision and preserves the intended destination until the decision completes.

Toggle Sidebar changes visibility without navigating away from a form or another screen. It is disabled with an explanation on screens without a sidebar. Hiding a sidebar that holds focus transfers focus to the content area. Retain F2 as the legacy list/pane focus toggle on Main, with its explicit label. It must not be labeled as Toggle Sidebar. Focus Agents and Return to Session provide the unambiguous menu paths.

Within agent-mux controls, arrows move selection, Enter opens or begins editing, Esc cancels the innermost interaction then goes back one level, and Tab/Shift+Tab move focus between fields or panes. Number keys select visibly numbered tabs only. Switching editor tabs uses those numbers or clicking tab labels, rather than overloading Tab. Preserve session selection via commands and list navigation; digits must not silently select sessions when no numbered tabs are shown.

Local verbs outside text fields are n New, e Edit, d Delete, r Run, x Stop, s Save, Ctrl+R Reload, R Restore and Ctrl+O Open in Editor. Existing g/G and paging aliases remain where applicable. q remains a Back alias outside text fields and a legacy Quit alias on Main's list. Confirmation defaults are safe: destructive actions need the existing confirmation; Enter must not unexpectedly accept an unselected destructive choice.

Text fields receive literal letters. Enter submits single-line fields; multiline fields insert newlines with Enter, with Ctrl+J retained as an alias. Ctrl+S saves the enclosing editor, and Esc ends field editing before leaving the editor. Enter in a review screen confirms its explicitly selected action. This distinction is visible in field hints.

In a session pane, ordinary keys, Ctrl letters, Alt, Esc, Tab and Enter remain harness input. Only recognized application chords and existing explicit scroll controls are intercepted. In application-owned editors, arrow keys navigate the editor rather than scrolling the background session.

## Command menu

F1 opens Commands on every screen, including a session pane, providing a modifier-independent entry. This intentionally changes F1 from opening Help directly; Help is the first discoverable menu action and keeps the existing help chord and local ?. Existing help chords work globally and restore the underlying screen when dismissed.

The menu is an overlay preserving the underlying state, with a search field, matching action labels, available shortcut labels and disabled reasons. Typing filters; arrows choose; Enter executes; Esc closes. Search matches labels and keywords. Disabled actions remain visible but cannot execute. Clear the filter when opening a new menu. Quit and destructive actions still open their normal confirmations. Opening Commands while editing a field or search preserves that buffer and cursor.

Provide clickable Commands and Focus Agents controls in the persistent footer; Return to Session appears on application screens with an available session. The menu itself does not intercept its opener into the underlying screen. Help and menu are mutually exclusive overlays and cannot stack recursively.

## Terminal shortcuts and verification

Keep existing Command and Ctrl+Shift aliases initially, but distinguish modifier support from a successfully observed binding. Add Test Shortcuts to Commands and build it on the event normalization used by `agent-mux keys`. Guide the user through Commands, Focus Agents, sidebar, help and traces. A binding is verified only when its actual event reaches the app; support alone leaves it unverified. Permit skip and cancellation, and never execute the probed action during the test.

Store the preferred observed binding per action in the current app session. Do not claim that a binding works across terminal launches based on an environment name. Until verified, present F1 for Commands and menu access for actions without a known binding. Help lists legacy aliases as unverified candidates and explains testing. On terminals without modifier reporting, never advertise a collapsed Ctrl+Shift chord as distinct from the harness's Ctrl key. No new plain Ctrl, Alt, Esc or Tab binding is reserved on the main session pane.

## Presentation

Use a distinct border and a textual focus label: Agents — Keyboard focus, Session — Keyboard goes to Codex, or the active editor/control name. Color alone must not carry focus. Footer actions follow the registry and current scope; hints do not bleed through from a background screen. Show one preferred shortcut per action, with aliases in Help. At narrow widths remove whole low-priority hints, retaining Commands and the focus indication.

## Implementation boundaries

`src/keymap.rs` owns action IDs and binding metadata. A dedicated app command module owns context, availability and dispatch, with a dedicated UI module rendering the command menu and shortcut probe. `src/app.rs` integrates global dispatch before normal screen input, while modal editors retain precedence for their own text controls. `src/ui.rs` and the existing per-screen UI modules consume the registry for hints and focus. `docs/keyboard.md`, AGENTS.md's keyboard guide and relevant help text are updated to match the new contract.

Global dispatch must resolve only app-wide commands first, not pane-specific scroll commands. It must preserve pending confirmations and unsaved editors. Failures produce the existing notice UI and restore the previous interaction; they must not discard buffers.

## Verification and acceptance

Add a screen matrix covering Main list/pane, Trace Browser, Skills, Configuration, Agent Editor, Task Library, Flow Builder, Runs, Inbox and Markdown. Exercise global menu/help access, enabled and disabled navigation, menu dismissal and originating-session restoration. Test dirty-editor navigation and destructive-action confirmations.

Verify typed text is preserved when opening and dismissing overlays. Verify ordinary pane input is forwarded unchanged, and unsupported chords cannot trigger plain-letter commands. Verify Tab has the same focus meaning, numbered tabs match labels, Save never changes verdict, and background session scrolling cannot capture editor navigation.

Probe tests feed enhanced and legacy key events, including collapsed modifiers, skipped probes and observed aliases. Footer assertions must check availability and verified labels at wide and narrow terminal sizes. Run cargo test, cargo fmt --check and cargo clippy --all-targets, then manually exercise the command menu and shortcut probe with `agent-mux keys` in the installed terminal. Record the observed terminal and delivered events; make no untested compatibility claims.

The existing full-suite baseline had one integration failure in `memory::a_profile_or_config_without_memory_gets_none` at memory.rs:261 while running inside a memory-enabled session. Record that separately and investigate the inherited environment before attributing it to this feature; do not silently broaden the UX change into a memory fix.
