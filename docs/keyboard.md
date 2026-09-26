# Keyboard

agent-mux has one keymap for every screen: each key means one thing, and a screen may leave a key out but never gives it another meaning. The table lives in `src/keymap.rs` (`VERBS`); the help overlay (`?`) prints it from there, with macOS labels (`⌃J`, `↩`) on a Mac.

It is built for a Mac keyboard: nothing needs `Home`, `End`, `PgUp`, `PgDn`, forward `Delete`, an F-key or an Option chord. Those keys still work where a keyboard has them.

## Everywhere

| Key | Means |
| --- | --- |
| `Enter` | open, choose, confirm |
| `Esc` / `q` | back one level; close at the top (`q` outside text fields) |
| `n` | new, in the place you are: a session, a loop, a flow, a step, a field, a pattern, an agent |
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
| Main screen | `b` sidebar, `Tab` section, `l` session logs, `t`/`T` tracing / traces, `S` skills, `C` configuration, `E` loops view, `W` workflows view, `I` inbox, `v` about, `K` loops kill switch, `X` clear exited sessions |
| Loops section | `p` pause, `o` the loop's pattern in the loop builder |
| Workflows section | `c` compose a workflow for a task |
| Trace browser | `/` search, `v` detail view, `Space` fold, `a` all projects, `+` verdict |
| Skills view | `v` validate, `1`-`3` harness filter |
| Configuration view | `u` push loop skills to workspaces |
| Flow builder | `1` steps, `2` what passes, `3` review; `Space` makes a field required; `[` `]` previous / next step |
| Loop builder | `c` copies a pattern |
| Attached to a session | `Ctrl+Q` detach; every other key goes to the harness |
