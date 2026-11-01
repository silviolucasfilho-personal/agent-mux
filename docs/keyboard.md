# Keyboard

agent-mux uses one command registry (`src/keymap.rs`) for the Commands menu, availability checks and shared hints. Ordinary keys in a session pane go to the coding harness; application screens use the controls below.

## Commands and focus

**F1 opens Commands on every screen**, including inside a text field or search. Type to filter commands, use arrows to choose, Enter to execute and Esc to close. The underlying screen, text and selection remain intact. Each command shows its shortcut at the right: one observed by **Test shortcuts**, else the function key or this platform's chord (`⌘` on macOS, `Ctrl+Shift` elsewhere) in grey until a test confirms it reaches agent-mux, else the screen's plain key (`n`, `e`, …). Disabled commands explain why they cannot run and still show their key. Help is available from Commands, the help chord, or `?` outside text fields.

The persistent footer provides clickable Commands, Focus Agents and Return to session controls when space and availability permit. Its text says where the keyboard goes, and the focused pane has a distinct border. At narrow widths Commands and focus take priority over other hints.

| Action | Behavior |
| --- | --- |
| Focus Agents | Reveal the sidebar and focus the Agents list; repeating it never hides the list. |
| Return to session | Return to the session from which the app screen was opened, falling back to the selected session if the original is gone. |
| Show/hide sidebar | Change layout on Main or the Trace Browser; it does not leave an editor. Hiding the focused sidebar transfers focus to the pane. |
| F2 | Legacy list/pane focus toggle on Main, separate from sidebar visibility. |

Leaving an editor with unsaved changes through Commands asks before discarding. `y` continues; `n` or Esc keeps the editor. Enter does not discard changes.

Selecting a harness renders its session list immediately. Recap facts load in the background with a “Loading recap…” message, once each time the harness is selected (and again when its sessions change), not on a timer while it stays selected; a reload keeps the previous facts on screen, and results from a different harness or session list are discarded.

## Shortcut reliability

The terminal may intercept a key or collapse its modifiers. Protocol support alone does not establish that a particular binding reaches agent-mux. **Test shortcuts** in Commands observes the actual delivered events without executing actions. Space skips a probe; Esc cancels. Verified preferences last for the current app session. Help lists candidate aliases separately from observed bindings. Essential actions remain accessible through F1 and the menu even without modifier reporting.

| Action | Candidate aliases |
| --- | --- |
| Commands | F1, Command+P, Ctrl+Shift+P |
| Focus Agents | Command+L, Ctrl+Shift+L |
| Return to session | Command+Enter, Ctrl+Shift+Enter |
| Sidebar | Command+B, Ctrl+Shift+B |
| Help | Command+/, Ctrl+Shift+/ |
| Traces | F3, Command+I, Ctrl+Shift+I |
| Previous / next session | Command+Up / Down, Ctrl+Shift+Up / Down |
| Quit | F10, Ctrl+Shift+Q |

**Close all sessions** (`Q` in the Agents list, or F1 → Commands) kills every session in the Agents list, whatever its harness, after a `y` on the confirmation. An exited session leaves the list at once; a live one leaves once its exit is recorded, so its trace, loop or workflow run finishes the normal way, and none of them is restored at the next start.

`agent-mux keys` is the raw terminal event diagnostic. F1 now opens Commands instead of opening Help directly. Existing Command+E / Ctrl+Shift+E and F2 retain their Main focus-toggle meaning.

## Application screens

| Key | Meaning |
| --- | --- |
| Up / Down | Move selection or scroll the focused reader. |
| Tab / Shift+Tab | Next / previous field or pane. |
| Enter | Open the selected item or begin editing a field. |
| Esc | Cancel the innermost interaction, then back one level. |
| n / e / d | New / Edit / Delete or dismiss, where supported. |
| r / x | Run or resume / Stop. |
| s | Save; it never sets a trace verdict. Use Change verdict in Commands. |
| Ctrl+R / R | Reload / Restore built-in. |
| Ctrl+O | Open in the configured external editor. |
| 1–9 | Select a visibly numbered tab. They no longer silently select sessions on Main. |
| g / G | Top / bottom where supported. |
| Ctrl+D / Ctrl+U | Page down / up outside text fields. |
| ? | Help outside text fields. |

`q` remains a Back alias outside text fields and a legacy Quit alias in Main's list. Context-specific actions remain reachable through Commands; unavailable actions cannot run against a stale selection.

## Text and sessions

Letters remain literal while editing. Enter finishes a single-line field; in multiline instruction/task editors it inserts a newline. Ctrl+J remains a newline alias. Ctrl+S saves an enclosing editor, including an in-progress field. Esc ends or cancels field editing before leaving the editor; field hints show which applies. Existing cursor, word deletion, clipboard and external-editor controls remain available.

Compose task and argument fields use Enter for a newline and Ctrl+Enter (or Submit dialog in Commands) to submit. Destructive confirmations require `y`; Enter does not approve them.

With session focus, ordinary characters, Ctrl letters, Alt, Esc, Tab and Enter go to the harness. Only recognized application chords and the explicit session scroll controls are intercepted. Host copy/paste shortcuts continue to use the terminal's clipboard path. Application editors never scroll a background session in response to their navigation keys.
