# Keyboard

agent-mux has one keymap for every screen: each key means one thing, and a screen may leave a key out but never gives it another meaning. The table lives in `src/keymap.rs` (`VERBS`); the help overlay (`?`) prints it from there, with macOS labels (`⌃J`, `↩`) on a Mac.

It is built for a Mac keyboard: nothing needs `Home`, `End`, `PgUp`, `PgDn`, forward `Delete`, an F-key or an Option chord. Those keys still work where a keyboard has them.

## Where the focus is

The main screen has no modes and nothing to attach to. The keyboard is either in the **list** (the sidebar) or in the **pane** (the selected session), and agent-mux starts in the pane when there is a session to type into. With the pane focused every plain key, `Ctrl+letter`, `Alt`, `Esc`, `Tab` and `Enter` goes to the harness, `Ctrl+Q` included. With the list focused the plain keys are the verbs below.

| Moves the focus | To |
| --- | --- |
| `Enter` on a session row, `Esc` in the list, a click in the pane, a click on a session row, a session you start, respawn, fork or resume, a paste | the pane |
| `⌘E` (`Ctrl+Shift+E`, `F2`) from either side; the pane's session exiting | the list |

The keys that work with either focus live in the layer GUI terminals reserve for the application: `⌘` on macOS, `Ctrl+Shift` on Windows and Linux (and on macOS too, for terminals that keep `⌘` for themselves). They never reach the harness.

| Does | macOS | Windows, Linux, and macOS fallback |
| --- | --- | --- |
| focus list ⇄ pane | `⌘E` | `Ctrl+Shift+E` (`F2` everywhere) |
| sidebar show / hide | `⌘B` | `Ctrl+Shift+B` (`b` in the list) |
| previous / next session | `⌘↑` / `⌘↓` | `Ctrl+Shift+↑` / `Ctrl+Shift+↓` |
| find in the pane | `⌘F` | `Ctrl+Shift+F` (`Ctrl+F` in the list) |
| copy the selection / paste | the host's `⌘C` / `⌘V` | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| help | `⌘/` | `Ctrl+Shift+/` (`F1` everywhere; `?` in the list) |
| scroll three lines, a page, to the oldest / newest | `Shift+↑/↓`, `Fn+↑/↓`, `Shift+Fn+←/→` | `Shift+↑/↓`, `PgUp/PgDn`, `Shift+Home/End` |

Not in the layer, on purpose: `⌘N`, `⌘W`, `⌘T`, `⌘Q`, `⌘1`-`9` and `⌘,`. Every host terminal takes those first (new window, close, new tab, quit, tab N, preferences), and `⌘Q` would quit the terminal. New, stop and quit are list verbs, one `⌘E` away.

**What the terminal lets through.** A chord reaches agent-mux only if the terminal does not bind it itself and reports its modifiers. Windows always reports them; on macOS and Linux it takes the kitty keyboard protocol (Ghostty, kitty, WezTerm, foot, iTerm2 3.5+, Alacritty 0.13+, VTE 0.78+). Terminal.app and plain xterm report none, so there `Ctrl+Shift+B` is Claude Code's `Ctrl+B` and is forwarded: use `F2` and `F1`, the mouse, or another terminal; the status bar and the About view (`v`) say which applies. A chord the host binds (`⌘F` find in Terminal.app, iTerm2 and Ghostty; `Ctrl+Shift+F` in GNOME Terminal and kitty; `⌘↑`/`⌘↓` marks in Terminal.app and iTerm2) never arrives; its twin does. `agent-mux keys` prints what your terminal delivers.

**Links.** A click on a link in the pane opens it (`src/links.rs`). While the pointer rests on a link, and only then, agent-mux pushes a second kitty keyboard mode that reports every key with its press and release, modifiers alone included, so it can see `⌘` held: the pointer becomes a hand (OSC 22: Ghostty, kitty, foot) and goes back when `⌘` is released or the pointer leaves the link. `keys::normalize_event` makes keys typed in that mode read like ordinary ones (a repeat is a press, Caps Lock applies, `Ctrl+Shift+B` keeps its Shift), and the first key typed leaves the mode until the pointer moves again. A `⌘` already held when the pointer reaches the link is not seen; press it there. A terminal without the protocol shows no hand; the click still opens.

## Everywhere

| Key | Means |
| --- | --- |
| `Enter` | open, choose, confirm |
| `Esc` / `q` | back one level; close at the top (`q` outside text fields) |
| `n` | new, in the place you are: a session, a loop, a flow, a step, a field, a pattern, an agent; a new session starts on the harness under the cursor (Claude Code, Codex or Antigravity) |
| `e` | edit the selected item |
| `d` | delete, remove or dismiss, always with a `y`/`n` question |
| `x` | stop something running: kill a session, cancel a workflow run |
| `s` | save (`Ctrl+S` in forms) |
| `r` | run, run now, resume |
| `Ctrl+R` | reload, rescan |
| `R` | restore the built-in (a workflow, loop pattern, agent or configuration item you edited) |
| `J` / `K` | move the selected item down / up |
| `g` / `G` | top / bottom (`Home` / `End` too) |
| `Ctrl+D` / `Ctrl+U` | page down / up (`PgDn` / `PgUp` too) |
| `1`-`9` | a tab or a screen |
| `Tab` / `Shift+Tab` | next / previous field or pane |
| `Ctrl+O` | open in `$EDITOR` (`editor` in profiles.toml, `$VISUAL`, `$EDITOR`, `vi`) |
| `?` | help |

Confirmations are `y` / `Enter` for yes and `n` / `Esc` for no.

## In a text field

| Key | Does |
| --- | --- |
| `Enter` | submits; it never inserts a new line |
| `Ctrl+J` | a new line, in every terminal (`Option+Enter` and `Shift+Enter` too, where the terminal sends them) |
| `Ctrl+A` / `Ctrl+E` | start / end of the line |
| `Ctrl+W` or `Option+Backspace` | delete the word before the cursor |
| `Ctrl+U` | clear the field |
| `Ctrl+V` | paste |
| `Ctrl+O` | compose the field in `$EDITOR`; what you save comes back |

For `Option` combinations, turn on "Use Option as Meta key" (Terminal.app) or "Esc+" for the left Option key (iTerm2). agent-mux never needs them.

## By place

| Place | Keys of its own |
| --- | --- |
| Main screen, list focused | `Enter` on a session or `Esc`: the pane takes the keyboard. `b` sidebar, `Tab` section, `Space` fold a heading or agent (on a session, fold its parent); click a heading or its arrow to fold with the mouse. `l` session logs, `t` tracing on / off, `T` the trace strip under the session (off → one line → eight rows → the Trace Browser), `S` skills, `C` configuration, `E` or `W` runs view, `I` inbox, `v` about, `K` loops kill switch, `X` clear exited sessions, `f` on a session: continue it in a new session with its memory; `←` folds the row under the cursor (on a session, or a row with nothing to fold, it climbs to the parent), `→` unfolds (or steps onto the first child), `Space` toggles a fold, a click on the arrow folds too |
| Loops section | `p` pause, `o` its task in the task library (the editor's What tab), `f` a new pattern there |
| Workflows section | `c` compose a workflow for a task |
| Trace browser (a drawer in the pane) | `/` search, `v` detail view (list → tree → timeline → loop → summary), `Space` fold, `a` all projects, `+` verdict, `b` sidebar, `Esc` back to the session |
| Skills view | `v` validate, `1`-`3` harness filter |
| Configuration view | `u` push loop skills to workspaces |
| Agent editor | `R` restores a built-in you saved a copy of (it asks); `1`-`5` Who · What · When · Limits · Review (`Tab` / `Shift+Tab` in a scheduled agent or a persona); `Enter` types a field, `←` `→` `Space` change the others; a scheduled agent's What is the task library; a flow's fields: `Space` makes a field required, `[` `]` previous / next step, `Enter` on Answers with opens what passes |
| Task library (What tab of a scheduled agent) | `↑↓` picks the task, `→` edits it, `n` new pattern, `c` copies one, `R` restores a built-in, `d` deletes yours; `1`-`5`, `s` and `r` are the editor's |
| Runs view | `1`-`5` Report · History or Steps · Change · Result · Setup or Document; `p` pauses a loop; `s` saves a flow's document; `Enter` applies a change, runs a plan or attaches to a running run; `d` rejects or dismisses (a plan asks first); `r` runs a scheduled agent again or resumes a flow run; `x` stops; `e` edits the run's agent; `T` traces |
| Main screen, pane focused | every key goes to the harness except the chord layer above |
