# One screen, no attach — keyboard design

Status: implemented 2026-10-01 (steps 1-4; the terminal probe of section 9 is still to run per terminal with `agent-mux keys`). Replaces the Control / Attached modes of `src/app.rs` and the "Attached to a session" rows of `docs/keyboard.md`. Everything else in `docs/keyboard.md` (the verb table, text fields, the keys by place) stays as it is.

## 1. Why

agent-mux starts in Control mode: the pane shows the selected session but typing does nothing to it until `Enter` attaches, and every list key needs `Ctrl+Q` first. That is tmux's prefix model wearing a sidebar, and it costs the user on every turn:

- **Two states to track.** The same key (`n`, `d`, `x`, `q`) is a verb in one state and text in the other. The user has to look at the status bar before typing.
- **A dead first screen.** The session is on screen, the cursor is in it, and keystrokes go nowhere.
- **A detach that is not a key.** `Ctrl+Q` is a terminal flow-control byte, and the `Ctrl+Q Ctrl+Q` literal-send chord exists only to undo the interception.
- **Not the keys of the OS.** Mac users reach for ⌘, Windows and Linux users for `Ctrl+Shift`. GUI terminals already reserve exactly that layer for themselves (Ghostty, kitty, WezTerm, Windows Terminal, GNOME Terminal). agent-mux should sit in the same layer, not invent a prefix.

## 2. Principles

1. **No modes.** There is one screen with two places focus can be: the **list** (the sidebar) and the **pane** (the selected session). Keys go where the focus is. Nothing is ever "detached".
2. **The pane is a terminal.** With the pane focused, every plain key, `Ctrl+letter`, `Alt`, `Esc`, `Tab` and `Enter` reaches the harness unchanged. Claude Code's `Ctrl+B`, `Ctrl+O`, `Ctrl+R`, `Esc Esc`, `Shift+Tab`, Codex's `Ctrl+T`, `Ctrl+G`: all theirs. `Ctrl+Q` too.
3. **One chord layer, the OS's.** agent-mux's own keys that must work while typing into a harness live in the layer GUI terminals reserve for the application: **⌘** on macOS, **`Ctrl+Shift`** on Windows and Linux. `Ctrl+Shift` also works on macOS, for terminals that do not pass ⌘ through. The layer is small (section 4.1); everything else is a plain key in the list.
4. **The list keeps its verbs.** `n`, `e`, `d`, `x`, `s`, `r`, `f`, `j`/`k`, `1`-`9`, `?`, `q` mean what `docs/keyboard.md` says, when the list has focus. No existing list key changes meaning.
5. **Start ready to type.** agent-mux opens with the pane focused on the selected session when one exists.
6. **The status bar always says where you are** and shows the chords in the glyphs of the OS (`⌘B` on a Mac, `Ctrl+Shift+B` elsewhere).

## 3. The model

```
┌─ list ───────┬─ pane ──────────────────────────────┐
│ CLAUDE CODE  │ ❯ the selected session, live        │
│  ├ api  ▸    │                                     │
│  └ web       │   focus here: keys go to the agent  │
│ SCHEDULED 1  │   focus in the list: keys are verbs │
│ FLOWS 3      │                                     │
├──────────────┴─────────────────────────────────────┤
│ api · claude · working      ⌘E list · ⌘↑↓ session   │
└────────────────────────────────────────────────────┘
```

`App.mode` keeps its overlays (dialogs, the trace browser, the runs view, the editors) and loses `Control` and `Attached`; the main screen is `Mode::Main` with a `focus: Focus` of `List` or `Pane`.

| Focus | What the pane shows | What keys do |
| --- | --- | --- |
| **Pane** | the selected session, its cursor live | forwarded to the harness, except the chord layer |
| **List** | the selected session, cursor hidden, row highlighted | the list verbs; the chord layer too |
| no session at all | a welcome card with the keys | the list verbs (focus is in the list) |

Moving focus:

| From | To the pane | To the list |
| --- | --- | --- |
| keyboard | `Enter` on a session row (what `Enter` does today, minus the word "attach"); `Esc` in the list | `⌘E` / `Ctrl+Shift+E` from anywhere |
| mouse | click in the pane; click a session row (selects it and focuses the pane, the way a tab click does) | click a heading, a fold arrow, or a non-session row (selects it, focus stays in the list) |
| the app | a session you start, respawn, fork or resume | the session under the cursor exits (the pane shows its last screen; `r` respawns, `d` removes) |

`⌘E` toggles: in the list it returns to the pane. `Esc` in the list is "back one level", and from the list the level below is the pane; `q` quits as today. Selecting another session with `⌘↑`/`⌘↓` or `j`/`k` never moves focus, so the list can be walked while typing is parked, and the pane can switch sessions without leaving the keyboard.

## 4. The keys

### 4.1 The chord layer (works with either focus)

The whole set. Every row is in the layer the OS reserves for applications, and every row has the `Ctrl+Shift` form on all three systems.

| Does | macOS | Windows, Linux, and macOS fallback | Today |
| --- | --- | --- | --- |
| focus list ⇄ pane | `⌘E` | `Ctrl+Shift+E` | `Enter` / `Ctrl+Q` |
| sidebar show / hide | `⌘B` | `Ctrl+Shift+B` | `b`, `Ctrl+Shift+B` |
| previous / next session | `⌘↑` / `⌘↓` | `Ctrl+Shift+↑` / `Ctrl+Shift+↓` | `j`/`k` after detaching |
| find in the pane | `⌘F` | `Ctrl+Shift+F` | `Ctrl+Shift+F` |
| copy the selection | `⌘C` (the host does it) | `Ctrl+Shift+C` | `Ctrl+Shift+C` |
| paste into the pane | `⌘V` (the host does it) | `Ctrl+Shift+V` | `Ctrl+Shift+V` |
| help | `⌘/` | `Ctrl+Shift+/` | `?` in Control |
| scroll three lines | `Shift+↑` / `Shift+↓` | same | same |
| page | `PgUp` / `PgDn` (`Fn+↑`/`Fn+↓`) | same | same |
| oldest / newest | `Shift+Home` / `Shift+End` (`Fn+Shift+←`/`→`) | same | same |

Not in the layer, on purpose:

- **`⌘N`, `⌘W`, `⌘T`, `⌘Q`, `⌘1`-`9`, `⌘,`** and their `Ctrl+Shift` twins. Every host terminal takes these for new window, close, new tab, quit, tab N and preferences, and takes them before agent-mux sees them. `⌘Q` in particular would quit the terminal, not agent-mux. Creating, stopping, numbering and quitting are list verbs (`n`, `x`, `1`-`9`, `q`), one `⌘E` away.
- **Plain `Ctrl+letter`, `Alt+key`, `Esc`, `Tab`.** The harness's.
- **A session-number chord.** `⌘↑`/`⌘↓` plus the list cover it.

### 4.2 With the list focused

The verb table of `docs/keyboard.md` and the keys by place, unchanged, plus:

| Key | Does |
| --- | --- |
| `Enter` on a session | focus the pane (today: attach) |
| `Esc` | focus the pane (back one level) |
| `q` | quit (asks when sessions are working), as today |
| `b` | sidebar show / hide (kept as the plain-key twin of `⌘B`) |
| `Ctrl+F` | find (kept; nothing is forwarded from the list) |

Hiding the sidebar does not move focus. With the sidebar hidden and the list focused, the status bar names the selected session and `j`/`k`, `1`-`9` still select.

### 4.3 With the pane focused

Everything not in section 4.1 is encoded by `keys::encode_key_with_mode` and written to the pty, as today. The `Ctrl+Q` interception, `just_detached` and `SendLiteralDetachKey` go away: `Ctrl+Q` is 0x11 to the harness like any other control key.

Mouse in the pane as today (`mouse::route_wheel`, drag selects, release copies, `Alt`+click moves the cursor), with one change: a click in the pane focuses it.

### 4.4 Overlays

The dialogs and views (`?`, `S`, `C`, `W`, `I`, `T`, `l`, `v`, the editors, the confirms) are unchanged: they take the keyboard while open and `Esc`/`q` closes them. They are opened from the list (plain keys) or from either focus by the help chord. Closing an overlay returns to the focus it was opened from.

## 5. Where the host terminal gets in the way

A terminal passes a chord to agent-mux only if (a) it does not bind the chord itself and (b) it reports the modifiers. Neither is under agent-mux's control, so the design detects and tells rather than assumes.

### 5.1 Modifier reporting

| Platform / terminal | `Ctrl+Shift+letter` distinct from `Ctrl+letter` | ⌘ / Super delivered |
| --- | --- | --- |
| Windows (crossterm reads the console input records) | yes, always | n/a |
| Ghostty, kitty, WezTerm, foot, iTerm2 ≥ 3.5, Alacritty ≥ 0.13, Rio (kitty keyboard protocol) | yes, with the flags agent-mux already pushes (`DISAMBIGUATE_ESCAPE_CODES`, `src/main.rs`) | reported as `KeyModifiers::SUPER` for chords the terminal leaves unbound (to probe per terminal, section 9) |
| GNOME Terminal and other VTE ≥ 0.78, Konsole ≥ 24 | yes (kitty protocol) | not applicable |
| Terminal.app, older VTE, plain xterm | **no**: `Ctrl+Shift+B` arrives as `Ctrl+B` (0x02) | **no** |

`crossterm::terminal::supports_keyboard_enhancement()` already answers (a) at start (`KEYBOARD_ENHANCED`). Windows answers yes without it.

### 5.2 When the chord layer cannot arrive

In Terminal.app and other terminals without modifier reporting, `Ctrl+Shift+B` is indistinguishable from Claude Code's `Ctrl+B` and must be forwarded. Those terminals get a third, protocol-free form of the two chords that matter, and a message:

| Does | Key everywhere, no protocol needed |
| --- | --- |
| focus list ⇄ pane | `F2` |
| help | `F1` (already bound) |

`F1`/`F2` are escape sequences every terminal sends distinctly and no harness binds. On a MacBook they are `Fn+F1`/`Fn+F2` unless the user has set function keys to standard. The status bar in such a terminal shows `F2 list` instead of `⌘E list`, and the About view (`v`) states: "This terminal does not report modifier keys; ⌘ and Ctrl+Shift chords cannot reach agent-mux. F2 switches focus. Ghostty, iTerm2, kitty and WezTerm report them." The sidebar can also always be reached by click.

### 5.3 Host bindings that shadow a chord

Where the terminal binds a chord itself, the host wins and agent-mux never sees it. Known defaults, to confirm with the probe (section 9):

| Chord | Shadowed by default in |
| --- | --- |
| `⌘F` | Terminal.app, iTerm2, Ghostty (host find); `Ctrl+Shift+F` in GNOME Terminal (find) and kitty (move window forward) |
| `⌘E` | Terminal.app, iTerm2 (Use Selection for Find) |
| `⌘↑` / `⌘↓` | Terminal.app, iTerm2 (previous / next mark) |
| `Ctrl+Shift+B` | kitty (move window backward) |
| `Ctrl+Shift+↑/↓` | Windows Terminal with panes open (resize) |

The rule: a shadowed chord's `Ctrl+Shift` twin or `F2` still works, the status bar shows the form the probe found deliverable, and the user may rebind the host (kitty and Ghostty make unbinding a one-liner). agent-mux does not fight the host.

### 5.4 Paste

The host's own paste (`⌘V`, `Ctrl+Shift+V`, middle click) must land in the pane as one event, never as typed characters that the list would read as verbs. agent-mux enables bracketed paste in the terminal (`EnableBracketedPaste`) and handles `Event::Paste`: pane focused → `paste_bytes` into the pty (bracketed when the child asked for it); list focused → paste into the pane anyway and move focus there, because pasting is typing.

## 6. Startup

| State at start | Focus | Pane |
| --- | --- | --- |
| restored or running sessions | pane, on the persisted selection (else the first live session) | that session |
| no sessions | list, on the first harness row | a welcome card: the three ways to start (`Enter` on a harness, `n`, a skill) and the chord layer in the OS's glyphs |
| `agent-mux` launched with a command to run | pane, on the new session | it |

The first run after this change shows a one-time notice in the status bar: "No more attach: type straight into the session. ⌘E (Ctrl+Shift+E) reaches the list." It is dismissed by any key and recorded in the state file so it never returns.

## 7. The status bar, the help, the labels

- **Focus is visible.** The pane's border and the selected row use the accent colour where the focus is; the other side is dim. The status bar leads with the focused place: the session's `profile · folder · status` with the pane focused, `LIST` with the list focused.
- **Chords in OS glyphs.** `keymap` grows a `Chord` table beside `VERBS` with three labels per chord (macOS, other, protocol-free), and `label()` picks by `MACOS` and `KEYBOARD_ENHANCED`. The status bar, the help overlay (`?`), the welcome card and the About view all print from it.
- **Hints by focus.** Pane focused: `⌘E list · ⌘↑↓ session · ⌘B sidebar · ⌘F find · ⌘/ help`. List focused: the row's hint as today, with `Esc`/`Enter` meaning "to the pane".
- `docs/keyboard.md` gains a "Where the focus is" section and the chord table, and loses the attached row; `README.md` sections on Control/Attached modes are rewritten to focus.

## 8. What goes away

| Today | After |
| --- | --- |
| `Mode::Control`, `Mode::Attached` | `Mode::Main { focus }` |
| `Action::Attach`, `Action::Detach`, `Action::SendLiteralDetachKey`, `just_detached` | `Action::FocusPane`, `Action::FocusList`, `Action::ToggleFocus` |
| `Ctrl+Q` detach, `Ctrl+Q Ctrl+Q` literal | nothing; `Ctrl+Q` is forwarded |
| `handle_ux_key` matching `(code, shift, ctrl)` inline | `keymap::chord(key, platform, enhanced) -> Option<Chord>` from a table, including `SUPER` |
| `ATTACHED —` status line | focus-led status line |
| `tracker.on_attach()` on attach | `on_focus()` when the pane gains focus or the selection changes with the pane focused (clears NeedsAttention the same way) |

Nothing changes in the trace store, in the session model or in how a session is spawned.

## 9. Probe before building

The repository's rule is that key and command facts are probed, not assumed. Two probes precede implementation and their results go in the module doc of `src/keymap.rs`:

1. **`agent-mux keys`**, a new diagnostic subcommand: raw mode, keyboard enhancement pushed, prints every key event it receives (`code`, `modifiers`, `kind`) until `Ctrl+C`, and on exit a summary of which chords of section 4.1 were delivered. Run it in Ghostty (this machine, 1.3.1), Terminal.app, iTerm2, kitty, WezTerm, and on Windows Terminal and GNOME Terminal when a machine is at hand. It stays in the binary as the support tool for "my keys do not work".
2. **Harness keys.** Open Claude Code, Codex and Antigravity in a pane and confirm none binds `F1`, `F2`, `Shift+↑/↓` or `Shift+Home/End`; record the `Ctrl` and `Alt` keys each uses so the chord layer is never extended onto one.

Open questions the probe settles:

- Whether Ghostty and iTerm2 on macOS deliver unbound ⌘ chords as `SUPER`. If a major terminal does not, `Ctrl+Shift` becomes the primary macOS label and ⌘ the bonus, not the other way round.
- Whether `⌘E` survives in enough terminals to be the focus chord, or `⌘J` (unbound in every host checked, VS Code's panel toggle) should take its place.

## 10. Implementation plan

Four steps, each shippable, tests in `tests/it` beside the ones they replace (`app_flow`, `scroll_ux`, `skill_ui`, `skill_hydrate`, `persistent_sessions` reference `Mode::Attached` today).

1. **Chord table and probe.** `keymap::Chord`, the table with three labels, `keymap::chord()` reading `SUPER`; `agent-mux keys`; bracketed paste and `Event::Paste`. `handle_ux_key` calls the table. No behaviour change yet beyond ⌘ chords working where delivered. Probe, record.
2. **Focus replaces modes.** `Mode::Main { focus }`; `dispatch` takes the focus; `Enter`/`Esc`/`⌘E`/`F2`/click move it; `Ctrl+Q` interception removed; `on_focus`; the mouse routing reads focus instead of `Mode::Attached`; the runs view's "attach to a running run" becomes "select and focus". Status bar and help follow.
3. **Startup and welcome.** Focus rules of section 6, the welcome card, the one-time notice.
4. **Docs.** `docs/keyboard.md`, `README.md`, `AGENTS.md` (the Keyboard paragraph), the help overlay text, `keymap.rs` module doc with the probe results.

Risk to watch: a key that used to be a Control verb while the pane was visible (`n`, `q`, `x`) is now typed into the harness. The one-time notice and the focus colour are the mitigation; the probe of step 1 checks that the status bar is right on day one in the terminals users have.
