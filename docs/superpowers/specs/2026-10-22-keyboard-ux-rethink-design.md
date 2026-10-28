# Keyboard UX rethink

Status: proposal, 2026-10-22. Builds on `2026-10-11-consistent-keyboard-ux-design.md`. This spec keeps that design's action registry and replaces how the user reaches it.

## Why again

The 10-11 pass added a registry, a Commands overlay, footer controls and a shortcut probe. Driving the real binary (isolated `HOME`, a copy of the store, a bash "harness", 140×40) and taking an inventory of every key handler shows that the pass added a layer and left the problems under it in place.

**Leaving a session depends on keys that don't arrive.**
- Every way out of a running harness is F1, F2, F3 or a ⌘ / Ctrl+Shift chord.
- On this Mac `com.apple.keyboard.fnState` is unset, so every F-key needs `fn`.
- The chords are "candidates" until Test shortcuts has observed them.
- The most frequent thing a multiplexer does is switch to another session and type. Today that takes fn+F2, then ↓↓, then Enter. Alternatively it takes a ⌘↑ that may never arrive. Nothing switches sessions by number or by name.

**The registry only gates letters.**
- `handle_local_command` maps `n e d r x s R` to actions before the screens see them, and it swallows the key when the action is unavailable. The screens still `match` on keys themselves.
- As a result, several handled keys are dead:
  - Skills `x` (Codex filter) is swallowed as Stop.
  - Trace reader `n` (find next) is swallowed as New.
  - `d` on a running session is swallowed.
  - The Flow Review `⌃D/⌃U` shown in its footer are dropped by the Ctrl filter.
  - The flow builder's `1–3` are taken by the agent editor's `1–5`.
- `Mode::Help` cannot be reached.
- The Actions `SelectSession`, `OpenTraceBrowser` and three others are never produced.

**One letter means many things.**
- `K` means kill switch, move item, or "up".
- `E` and `W` are the same action.
- `a` means approve, all projects, Antigravity, or ask.
- `x` means kill, cancel, stop, Codex, or "pick the 2nd harness".
- `v` means About, cycle view, or validate.
- `q` quits on the main list, goes back elsewhere, and closes the whole loop editor from a field.
- `Esc` on the main list focuses the pane instead of going back.
- `Tab` means pane, field, indent, or harness cycle.
- About 15 actions have a letter and no Commands entry: `f` fork, `p` pause, `K`, `t`, `T`, `X`, `m`, `c`, `v`, `u`, approve `a`, `z`…
- Memory, Change verdict and Test shortcuts are the reverse: they are in Commands and have no key.

**There is no way back.**
- Every screen exits to Main, so Inbox → Runs → Esc lands on Main, not the Inbox.
- The same happens for Skills → Traces and for Memory → Markdown.

**What is shown is noise or wrong.**
- The Commands overlay opens on "Help", followed by "Submit dialog — Unavailable…", followed by three "Filter skills — Unavailable" rows. Sessions and agents are not in it at all.
- Help is the registry printed raw: "Commands  Open skills (candidates: Commands)".
- The footer on a fresh start says "Agents • Keyboard focus … Type straight into the session". It offers `[Focus Agents]` while Agents already has focus, and labels `x` "Stop selected run" on a session.
- Stale hints:
  - `[Tab] Switch Section`.
  - `[n] to create a new session`. `n` opens the agent chooser.
  - Inbox `[x]`. The key is `d`.
  - A section header row ("▾ FLOWS") shows the generic "No sessions yet" page.

## Principles

1. **One reserved key, everywhere.** A tmux-style leader works in every terminal with no modifier reporting and no `fn`. It is the only key taken from the harness, and pressing it twice sends it through.
2. **Show the next keys, don't make the user memorise them.** After the leader, a which-key strip lists what comes next. Help is that same table, grouped by screen.
3. **One grammar for every list.** The rules:
   - Arrows or `j/k` move.
   - `Enter` does the default thing.
   - `Esc` goes back one level.
   - `n e d r x s` are the only single-letter verbs.
   - `.` opens "everything else you can do with this row".
   - Screen-specific letters survive only inside that screen's own namespace, after `.`.
4. **A real back stack.** Opening a screen from a screen pushes it, so `Esc` returns to where the user was.
5. **Attention first.** A multiplexer exists to put the user on the session that needs them. Make that one key.

## The leader

Default leader: **`Ctrl+\`**, configurable as `leader = "ctrl+\\"` in `profiles.toml`. `Ctrl+]`, `Ctrl+Space` and `Ctrl+B` are rejected:
- Claude Code uses `Ctrl+]` and `Ctrl+B`.
- macOS takes `Ctrl+Space` for input-source switching when several sources are enabled, as they are here.

`Ctrl+\` is 0x1C in raw mode and SIGQUIT in cooked mode, and the harnesses run raw. Before implementation, **probe** that `claude`, `codex` and `agy` do not bind it, and that Ghostty, iTerm2 and Terminal.app deliver it (`agent-mux keys`). Record the result in `keymap.rs` docs, per AGENTS.md.

| After the leader | Action |
| --- | --- |
| `1`–`9` | Go to session N. Numbers are shown next to session rows. |
| `Space` / `Tab` | Previous session, toggling between the last two. |
| `j` / `k`, `↓` / `↑` | Next / previous session. |
| `!` | Next session that needs attention (bell, permission prompt, exited). |
| `a` | Focus the Agents list. Pressed again, back to the session. |
| `p` or `/` | Switcher: fuzzy over sessions, agents, flows, screens and commands. |
| `n` | New session on the current harness. `N`: new agent. |
| `f` | Fork or hand off this session. |
| `x` | Stop this session (asks). |
| `z` | Zoom: hide or show the sidebar. |
| `t` `r` `i` `s` `c` `h` `m` | Go to Traces, Runs, Inbox, Skills, Config, History, Memory. |
| `?` | Help for the current screen. |
| `q` | Quit (asks if anything is working). |
| leader again | Send the leader byte to the harness. |
| `Esc` / timeout | Cancel. |

The which-key strip replaces the footer for as long as the leader is pending: `1-9 session · ␣ last · ! attention · a agents · p switch · n new · z zoom · ? help`.

F1 stays, as an alias for the switcher, and F2 stays. ⌘ / Ctrl+Shift chords stay as optional aliases. **Test shortcuts** and the unverified-candidate labelling become unnecessary on the main path and can be retired, or left in Help as a diagnostic.

## The switcher (replaces the Commands list)

One fuzzy list, opened by leader `p`, F1 or ⌘P.

- **Empty query, in this order:**
  - Sessions that need attention.
  - Recent sessions.
  - The actions for the current screen and its selected row, with their keys.
  - "Go to" screens.
- **Unavailable actions are hidden.** They appear, greyed with their reason, only when the query names them, so a search still explains why something can't run.
- **Items:** sessions (`3 agent-mux · Claude · working`), agents, flows, skills and personas from the list, screens, and actions. Enter goes, runs or opens.
- **Prefixes narrow the list:** `>` for actions only, `@` for sessions only. This follows the VS Code and Zed muscle memory.

## Lists: one grammar

| Key | Everywhere |
| --- | --- |
| `↑↓` `j k`, `g G`, `⌃D ⌃U` | Move, top/bottom, page. These are fixed on the main list, where they do nothing today. |
| `→` / `←` (`l` / `h`) | Expand / collapse a tree node, or go into / out of a pane. This replaces `m` "more agents" and Space-fold. |
| `Enter` | Default action: attach, run, open. |
| `Esc` | Back one level, via the back stack. On the main list it does nothing, so it no longer means "focus pane". |
| `Tab` | Next pane or field. It never means indent or cycle outside an editor. |
| `n e d r x s` | New, edit, delete (always asks), run, stop, save. **Unavailable means the key falls through to the screen**, not that it is swallowed with a notice. |
| `.` | Action menu for the row: every action, each with its key. This is where `p` pause, `o` pattern, `K` kill switch, `f` fork, `c` compose, `v` validate, `u` push, `a` approve and the filters live. |
| `/` | Filter the list or search the reader. `n`/`N` give next/previous match while a search is active. |
| `?` | Help. |
| `q` | Back, the same as Esc. It never quits, and quitting is leader `q`. |

The changes this implies:
- **Uppercase screen letters** `S C I E W K X T L V` move to leader + lowercase. They are kept for one release as hidden aliases, with a one-time notice.
- **`r` in editors** becomes "Save and run" only through `.` or Ctrl+Enter, so `r` never surprises the user with a run.
- **The harness filters in Skills** move to `.` or `/claude`.
- **The inbox** uses `d` for reject (matching its footer) and `.` for approve. Alternatively, `a` stays as an inbox-local verb and is shown in its footer.

## Focus, made explicit

There are three modes. The footer starts with a badge that names the mode, so focus is never carried by colour alone:

- `SESSION · Claude`: keys go to the harness, and only the leader is taken.
- `LIST`: the Agents list has the keyboard.
- `SCREEN Runs`, or `EDIT field`: an app screen has the keyboard.

On a fresh start with no sessions, the badge reads `LIST` and the footer offers `↩ new session · ⌃\ p switch · ? help`. It no longer says "type straight into the session". `[Focus Agents]` is not shown when the list already has focus.

## Back stack

`App.nav: Vec<Mode + its state handle>`. Opening a screen from a screen pushes onto the stack, and Esc / `q` pop it. The leader's go-to keys switch to the target screen and do not push. Leader `a` and Return to session clear the stack. Dirty editors keep their existing confirmation.

## Sidebar

- Session rows show their leader number (`1`–`9`) and an attention glyph.
- Remove the double cursor (`>   ❯`).
- Section headers become tree nodes, expanded with `→` and collapsed with `←`. This replaces the `m` "more agents / harnesses only" toggle. Selecting a header shows that section's summary: the flows with their last run, not "No sessions yet".
- The footer's right edge shows an attention count when sessions need the user: `● 2 waiting  ⌃\ !`.

## Text input

Every single-line input uses one `TextField` with cursor, word delete, `⌃U` and `⌃W`, and paste:
- NewSession
- Config's new name
- Runs' save-as
- Trace search
- The Memory query
- The scheduled-agent fields

Today some of these push raw characters, including Ctrl characters. Enter submits; Esc cancels. In a multiline field Enter inserts a newline, and Ctrl+Enter (or Ctrl+S in an editor) submits. The hint names the submit key.

## Attention signal

`Status::NeedsAttention` comes today only from a bell on an unfocused session (`status.rs`). Add the hook channel's signals where the store has them: Claude's `Notification` (a permission or idle prompt), Codex's equivalent, and an exit with a non-zero code. That way `!` and the waiting count do not depend on the harness ringing a bell. agy has no hooks and stays bell-only.

## Phases

1. **Fix what is broken, with no new concepts.** This is small, safe and worth doing regardless of the rest.
   - Let unavailable registry verbs fall through to the screen.
   - Fix the stale hints and the dead branches listed above.
   - Give `g/G/⌃D/⌃U` on the main list an effect.
   - Exempt the editor review from the Ctrl filter.
   - Resolve the editor/flow-builder digit clash.
   - Make Esc on the main list not move focus.
   - Delete the dead Actions and `Mode::Help`, or make Help reachable.
2. **Leader, which-key strip and switcher.** Probe the leader key first. Add numbered sessions, last-session and attention jump, and move Commands into the switcher.
3. **The grammar.**
   - The `.` action menu.
   - The back stack.
   - Retire the uppercase screen letters behind aliases.
   - Shared `TextField`.
   - The Help overlay generated per screen from the registry.
4. **Sidebar and footer polish**, plus hook-based attention.

Each phase ends with:
- A screen-matrix test (the 10-11 list), with key-delivery tests for the leader.
- An updated `docs/keyboard.md` and the AGENTS.md keyboard paragraph.
- A real-binary drive in an isolated HOME (see the "Driving the TUI safely" note).

## Open decisions

- The leader key: `Ctrl+\` (proposed), `Ctrl+A` (screen, but it is readline's line-start in every harness prompt), or the user's own choice.
- Whether to keep Test shortcuts at all once the leader exists.
- Whether `q` may stop quitting from the main list, which is a habit break.
- Whether inbox approve stays `a` (local verb) or moves to `.`.
