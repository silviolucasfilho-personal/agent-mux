# Skills in agent-mux

agent-mux ships **Heimdall**, a skill that briefs you on active sessions and evaluates skills and agents from the local SQLite trace store. Skills are the only kind of package agent-mux manages: one harness-neutral `SKILL.md`, installed into each harness's own skill directory in the syntax that harness reads, and launched from the Agents sidebar. The Skills view (`S`) is a read-only look at every skill, where it is installed and when it ran.

---

## 1. Package layout

```text
<id>/
├── SKILL.md          harness-neutral skill: frontmatter `name`, `description`, then the body
├── skill.toml        agent-mux metadata (optional)
└── reference/*.md    files installed next to SKILL.md, read by the model on demand
```

`SKILL.md` frontmatter:

| Field | Required | Rules |
| :--- | :--- | :--- |
| `name` | Yes | Equals the directory name; `^[a-z0-9][a-z0-9_-]*$`. Every harness matches the two. |
| `description` | Yes | What the skill is for and the quoted phrases that trigger it. A plain YAML scalar: no `: ` and no ` #`, so every harness parses it the same way. |

`skill.toml`:

| Field | Default | Meaning |
| :--- | :--- | :--- |
| `name` | id capitalized | Display name in the sidebar and the session name `<name> (<harness>)`. |
| `icon` | `⚡` | Sidebar icon. |
| `harnesses` | all | Harnesses offered in the picker: `claude`, `codex`, `agy`, `opencode`, `grok`, `pi` (default: every harness agent-mux installs skills for). |
| `default_harness` | first | Preselected in the picker; must be in `harnesses`. |
| `capabilities` | none | `trace.read` switches the preview pane to the telemetry dashboard. |
| `startup_prompt` | none | Appended to the invocation as the session's first message. |
| `[agent] hydrate` | none | Snapshots Rust writes before launch; `["dossier"]` (schema 2 startup dossier) or `["briefing"]` (schema 1 legacy briefing). Needs `trace.read`. |
| `[agent] mcp` | `auto` with `trace.read`, else `off` | Recorded on the package, but the server is attached to every session whatever a package says: the store is local, read-only and scoped to the launch's workspace. Claude Code and Codex take it on the command line, OpenCode in `OPENCODE_CONFIG_CONTENT`, Pi through agent-mux's MCP-client extension (`-e`); Antigravity and Grok read a global entry, which agent-mux writes before the first agy or grok session of a run (what `agent-mux mcp install agy|grok` does). `[agents] mcp = "off"` in profiles.toml is the one switch that turns it off, for every session and every harness. |
| `hidden` | `false` | Not listed in the Agents sidebar or the Skills view; still installed and launched by workflows (the `wf-*` step skills, `workflow-author`). |
| `writes` | `false` | The skill edits files; a workflow step running it must be isolated in a worktree (`docs/workflows.md`). |
| `auto_approve` | `false` | The session launches with every tool permission pre-granted, whichever harness runs it (`--dangerously-skip-permissions` on Claude Code and Antigravity, `--yolo` on Codex, `--auto` on OpenCode, `--always-approve` on Grok; Pi never asks). The package's own demand: it wins over a profile that leaves approvals on, never the other way round. |

Loop skills (`loops/skills/loop-*`) are **not** packages of this kind: they are installed at project level into a workspace by the loop scaffolder (`<workspace>/.claude/skills/<name>/SKILL.md`, `<workspace>/.codex/skills/<name>/SKILL.md`, plus the `loop-verifier` agent) and never appear in the Agents sidebar. See `docs/loops.md`.

## 2. Discovery

- Compiled-in: Heimdall (`skills/heimdall` in the repository, embedded with `include_str!`). Always present.
- User packages: `~/.agent-mux/skills/<id>/` (or `AGENT_MUX_SKILLS_DIR`). A user package with the same id shadows the compiled-in one. The Configuration view (`C`) and `agent-mux config edit heimdall/SKILL.md` create that copy for you and open it in your editor; `n` / `agent-mux config new skill <name>` scaffold a new package (`docs/configuration.md`).
- The Agents sidebar rescans when you switch to it with Tab; the Skills view (`S`) rescans when it opens and on `r`. Packages that fail to load are reported by `agent-mux skill list`.

## 3. Harness syntax

Verified against the installed CLIs (Claude Code 2.1.x, Codex CLI 0.154.x, Antigravity 1.2.x, OpenCode 2.0.32, Grok 1.0.70 and Pi 0.73.1, the last two on 2026-11-10 with a test skill under a temporary `HOME`):

| Harness | Skill directory | Invocation |
| :--- | :--- | :--- |
| Claude Code | `~/.claude/skills/<name>/SKILL.md` | `/<name>` as a slash command, including as the opening prompt |
| Codex CLI | `~/.codex/skills/<name>/SKILL.md` | `$<name>` mentioned in the prompt |
| Antigravity | `~/.gemini/config/skills/<name>/SKILL.md` | `/<name>` slash expansion, including through `--prompt-interactive` |
| OpenCode | `~/.config/opencode/skill/<name>/SKILL.md` (`$OPENCODE_CONFIG_DIR/skill/`; project level `.opencode/skill/`) | no syntax: the model loads a skill through its `skill` tool when asked, so the opening prompt says `load skill <name>` (through `--prompt`). OpenCode 2.0.32 also reads `skills/` and `.claude/skills/` at both levels. |
| Grok | `~/.grok/skills/<name>/SKILL.md` (`$GROK_HOME/skills/`; project level `.grok/skills/`, read only in a folder the user trusted) | `/<name>` slash command, including as the positional opening prompt. Grok also reads `.agents/skills/`, `.claude/skills/` and `.cursor/skills/`, deduplicated by name. |
| Pi | `~/.pi/agent/skills/<name>/SKILL.md` (`$PI_CODING_AGENT_DIR/skills/`; project level `.pi/skills/`) | `/skill:<name>`, which pi expands into the skill's body, including in the positional opening prompt. Pi also reads `~/.agents/skills/` and `.agents/skills/`. |

DeepSeek has no harness in agent-mux yet (`dsh 0.2.2-rc.1` was probed but deferred), so it has no skill directory or invocation. Note also what is not yet supported for Grok and Pi: while both support skill installation and MCP registration, sessions running on Grok or Pi do not have tracing pipelines yet, so turn-level telemetry and tool events will not appear in the Skills view Executions tab.

The installed `SKILL.md` is the canonical file with one difference: the description ends with that harness's invocation (`Invoke with /<name>.`, `Invoke with $<name>.`, `Invoke with /skill:<name>.`, …). `reference/` files are copied unchanged. A `.agent-mux.json` manifest marks directories agent-mux wrote; a directory without it is never overwritten or removed unless `--force`.

## 4. Launching from the Agents sidebar

Every package is listed in the Agents section of the sidebar; a `trace.read` package such as Heimdall previews its telemetry briefing in the main pane. Enter opens the harness picker. On confirm agent-mux:

1. installs or refreshes the skill in that harness's directory (manifest hash check; unchanged packages are left alone),
2. builds the harness command line: the model from your profile for that harness and the permission flags from your profile or the package's `auto_approve`, then the opening prompt `<invocation> <startup_prompt>` as the positional prompt (Claude Code, Codex, Grok, Pi), via `--prompt-interactive` (Antigravity) or via `--prompt` (OpenCode),
3. for a package with `[agent] hydrate`, computes the briefing in Rust and writes it to a snapshot file; the prompt ends with a sentence pointing at `$AGENT_MUX_BRIEFING`,
4. registers the read-only MCP server for the harness, as it does for every session (`$AGENT_MUX_MCP` says `registered`, `installed` or `unavailable`),
5. spawns the session with `AGENT_MUX_SKILL_ID`, `AGENT_MUX_BIN` (this executable), `AGENT_MUX_TRACE_DB` (the store the TUI writes) and `AGENT_MUX_WORKSPACE` in its environment, and records `skill_id` / `skill_harness` on the launch row so the Executions tab can find it later.

The same preparation runs for sessions restored at startup and for respawns.

A skill is a **singleton**: one live session per skill id. While it runs, the sidebar shows `<name> [<harness>]` and the Skills view shows `running [<harness>]`, Enter attaches to it, and asking for another harness attaches with a warning. Close the session to start it on another harness. Restored sessions count.

### Following a skill's session

`i` on a session's row in the Agents list opens its run screen, in the layout of a flow's Run screen (`docs/workflows.md`): the session where the list is, with its sub-agents as rows under it, and the selected row in the main pane.

- **Card** says who is running and how far it got: the skill or persona and what it is for, the harness, profile and model, the tools (a persona's list; a skill takes the harness's own), the folder, what it was asked, and turns, tokens, cost and the tool running now.
- **Live** is the session's own terminal. `Enter` gives it the keyboard; the Focus chord returns to the tree.
- **Trace** has its turns and tools from the trace store; a sub-agent's row shows its own part of it.
- **Changes** lists the files its tools wrote, then the folder's uncommitted changes (`git diff --stat`), which may include changes the session did not make.

`1`–`4` pick a tab, `↑`/`↓` a row, `Ctrl+D`/`Ctrl+U` scroll, and `i` or `Esc` returns. Starting a skill or a persona still lands in its terminal; the screen is one key away. Any session opens it, also a plain harness session, whose card has no skill or persona line. A session that is not traced has a card and a terminal, and no trace or files.

The session's pane title names the skill when its profile does not already.

## 5. The Skills workbench

Press `S` for the workbench view of every skill under each harness it declares: agent-mux packages and the harness's own native skills, each with its install state (`installed ✓`, `stale`, `not managed`, `not installed`, or `running [harness]`). The Details tab shows the package, where it is installed for that harness and the store's statistics; the Executions tab lists the sessions launched with it and the turns that loaded it.

For an agent-mux package, `e` opens its `SKILL.md` in the configured external editor. A compiled-in skill is first copied to the configuration library as an override. The workbench validates the saved package when the editor returns and keeps an invalid edit selected so it can be repaired. `v` validates again without launching. Harness-native skills remain read-only because agent-mux does not own their source.

`r` (run) installs the selected package for that row's harness and launches it, or attaches when that singleton skill is already running. Opening `S` again returns to the same skill and the Executions tab. `T` opens the selected execution in the Trace Browser. `1`-`3` filter by harness and `r` rescans.

## 6. CLI

```sh
agent-mux skill list                          # compiled-in and ~/.agent-mux/skills packages
agent-mux skill show heimdall --harness codex # SKILL.md as written for that harness
agent-mux skill install heimdall [--harness claude|codex|agy|opencode|grok|pi|all] [--force]
agent-mux skill uninstall heimdall [--harness …]
agent-mux skill status [heimdall]             # installed / stale / not managed, per harness
agent-mux skill import <dir> [--force]        # bring a package written for one harness into ~/.agent-mux/skills
```

### Importing

`agent-mux skill import <dir>` copies a skill package written for one harness (a `SKILL.md` with frontmatter, plus `reference/*.md` or `references/*.md`) into `~/.agent-mux/skills/<name>/`, from where the sidebar and `skill install` write it into every harness. The name comes from the frontmatter (the directory name when there is none) and must satisfy the id rule of section 1. The description is rewritten into the one plain YAML scalar every harness parses the same way: block scalars and continuation lines are flattened, `: ` becomes ` - ` and ` #` is dropped. Every other frontmatter key (`allowed-tools`, `tools`, `metadata`, `license`, …) stays in the package as written; the installed copy carries `name` and `description` only. A `skill.toml` with the display name and the default icon is written when the package has none and is kept on a `--force` re-import. The import prints a content lint (secret-looking strings, prompt-injection phrasing, a shell tool next to a write tool with no sentence saying what may be touched); findings are warnings, never a refusal. The package stays under your library, so nothing from another project is compiled in.

## 7. Heimdall

Heimdall declares `auto_approve = true`: it only reads, and it is usually launched to report rather than to be watched, so every harness starts it with tool permissions already granted (Claude Code and Antigravity `--dangerously-skip-permissions`, Codex `--yolo`, OpenCode `--auto`, Grok `--always-approve`; Pi never prompts). Heimdall starts from the startup dossier in `$AGENT_MUX_BRIEFING` (`$AGENT_MUX_BRIEFING_SCHEMA=2`, details in `docs/heimdall-dossier.md`), answering its default startup briefing and evaluation reports without tool calls. It asks follow-up drill-down questions through the ten `agent_mux_*` MCP tools when `$AGENT_MUX_MCP` is not `unavailable`, and otherwise uses the `agent-mux trace` CLI (`doctor`, `briefing`, `ls`, `show`, `search`, `loops`, `skills`, `skills lint`, `agents`, `compare`, `sql`). It never writes. `SKILL.md` carries the tool-to-CLI table and four playbooks (session briefing, skill evaluation, agent evaluation, drill-down); the `reference/` files hold thresholds and report shapes. Ad-hoc SQL lives in `docs/trace-sql-examples.md`. See `skills/heimdall/`.
