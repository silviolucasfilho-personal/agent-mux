# Markdown viewer and editor

agent-mux reads and writes Markdown in the terminal: rendered, with Mermaid diagrams drawn as text, or as source with a live preview beside it.

```
agent-mux md README.md              # read it rendered
agent-mux md notes.md --edit        # open the source; a new file starts empty
agent-mux md SKILL.md --print       # the rendering on stdout, plain text (--width N)
```

Inside agent-mux, a click on a Markdown file in a session's pane opens it in the same viewer: an OSC 8 `file://` link (Claude Code, agy) or a path printed as plain text (`docs/plan.md`, `README.md:12`), resolved against the session's directory and opened only when the file exists. `q` returns to the pane.

## Reading

| Key | Does |
| --- | --- |
| `j` / `k`, `↓` / `↑` | scroll a line |
| `Space` / `b`, `Ctrl+D` / `Ctrl+U`, `PgDn` / `PgUp` | a page |
| `g` / `G` | top / bottom |
| `]` / `[` | next / previous heading |
| `←` / `→` (`h` / `l`), a sideways swipe, `Shift`+wheel | slide the diagrams and code blocks wider than the window |
| `e` (`i`, `Enter`) | edit, at the block on top of the view |
| `s` | save |
| `Ctrl+R` | reload from disk |
| `Ctrl+O` | open in `$EDITOR` (`editor` in profiles.toml, `$VISUAL`, `$EDITOR`, `vi`), reload after |
| `q` / `Esc` | back to the file this one was reached from, else close |

A diagram or code block wider than the window says `←/→ scroll` in its frame and marks a cut-off edge with `‹` or `›`; sliding moves only those blocks (the text around them, and the frame's `│`, stay put). The swipe and `Shift`+wheel also slide the preview beside the editor.

A click on a link follows it: another Markdown file opens here (`Esc` comes back), `#heading` jumps, anything else goes to the platform's handler. The title row shows the heading you are under and how far down you are.

## Writing

| Key | Does |
| --- | --- |
| `Esc` | back to the rendered view, at the cursor's block |
| `Ctrl+S` | save |
| `Ctrl+Z` / `Ctrl+Y` | undo / redo (a typed word is one step) |
| `Ctrl+P` | preview beside the editor on or off (shown from 100 columns) |
| `Enter` | new line; continues a list (`- `, `1. ` → `2. `, `- [ ] `), ends it on an empty item |
| `Tab` / `Shift+Tab` | indent four spaces / dedent |
| `Ctrl+A` / `Ctrl+E`, `Ctrl+W`, `Ctrl+U` | line start / end, delete a word, delete to line start |
| `Ctrl+Home` / `Ctrl+End`, `PgUp` / `PgDn` | document start / end, a page |
| `Ctrl+V` or the terminal's paste | paste |

The preview follows the cursor: the block under it stays level with it. Nothing is written until you save; closing with unsaved changes asks (`s` save and close, `d` discard, `Esc` keep editing), and leaving for another file or `$EDITOR` waits for a save.

## What renders

CommonMark plus GitHub tables, task lists and strikethrough, and YAML front matter (a `SKILL.md`'s header shows as a framed block). Headings are coloured by level, with a rule under the first two; lists keep hanging indents; quotes keep their bar; tables are boxed, aligned and shrunk to the width; code blocks are framed and do not wrap.

A ```` ```mermaid ```` block is drawn as a diagram (`src/markdown/mermaid.rs`):

- `flowchart` / `graph`, every direction (`TD`, `TB`, `BT`, `LR`, `RL`): node shapes, solid, dotted and thick links, arrow, circle and cross heads, labels, chains and `&`. A `subgraph` is a frame with its title around its nodes, nested ones inside; a link to a subgraph leaves its last node and enters its first. Styling statements and `direction` inside a subgraph are ignored.
- `stateDiagram` / `stateDiagram-v2`: on the flowchart engine; `[*]` as `●` and `◉`.
- `sequenceDiagram`: participants and actors, every arrow, self-messages, `autonumber`, notes, and `loop` / `alt` / `opt` / `par` / `critical` / `break` blocks as dashed frames.
- `pie`: a bar chart with percentages.

Any other type (class, ER, Gantt, journey, git graph, mind map …) shows its source with the reason. The layout is layered: ranks by longest path, cycles broken and drawn with the arrow turned back, long edges routed through the ranks in between, barycentre ordering, and one track per jog between ranks.

## Code

`src/markdown/mod.rs` renders a document to styled lines and reports where links, headings and source blocks landed; `src/markdown/mermaid.rs` draws diagrams; `src/app/markdown_view.rs` holds the state and the keys (shared by the App's `Mode::Markdown` and the CLI); `src/ui/markdown.rs` draws it; `src/markdown/cli.rs` is `agent-mux md`. Pane links: `links::markdown_path_at` and `App::open_link`.
