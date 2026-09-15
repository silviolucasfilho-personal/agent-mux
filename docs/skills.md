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
| `description` | Yes | What the skill is for and the quoted phrases that trigger it. A plain YAML scalar: no `: ` and no ` #`, so all three CLIs parse it the same way. |

`skill.toml`:

| Field | Default | Meaning |
| :--- | :--- | :--- |
| `name` | id capitalized | Display name in the sidebar and the session name `<name> (<harness>)`. |
| `icon` | `⚡` | Sidebar icon. |
| `harnesses` | all | Harnesses offered in the picker: `claude`, `codex`, `agy`. |
| `default_harness` | first | Preselected in the picker; must be in `harnesses`. |
| `capabilities` | none | `trace.read` switches the preview pane to the telemetry dashboard. |
| `startup_prompt` | none | Appended to the invocation as the session's first message. |

## 2. Discovery

- Compiled-in: Heimdall (`skills/heimdall` in the repository, embedded with `include_str!`). Always present.
- User packages: `~/.agent-mux/skills/<id>/` (or `AGENT_MUX_SKILLS_DIR`). A user package with the same id shadows the compiled-in one.
- The Agents sidebar rescans when you switch to it with Tab; the Skills view (`S`) rescans when it opens and on `r`. Packages that fail to load are reported by `agent-mux skill list`.

## 3. Harness syntax

Verified against the installed CLIs (Claude Code 2.1.x, Codex CLI 0.154.x, Antigravity 1.2.x):

| Harness | Skill directory | Invocation |
| :--- | :--- | :--- |
| Claude Code | `~/.claude/skills/<name>/SKILL.md` | `/<name>` as a slash command, including as the opening prompt |
| Codex CLI | `~/.codex/skills/<name>/SKILL.md` | `$<name>` mentioned in the prompt |
| Antigravity | `~/.gemini/config/skills/<name>/SKILL.md` | `/<name>` slash expansion, including through `--prompt-interactive` |

The installed `SKILL.md` is the canonical file with one difference: the description ends with `Invoke with /<name>.` or `Invoke with $<name>.` for that harness. `reference/` files are copied unchanged. A `.agent-mux.json` manifest marks directories agent-mux wrote; a directory without it is never overwritten or removed unless `--force`.

## 4. Launching from the Agents sidebar

Every package is listed in the Agents section of the sidebar; a `trace.read` package such as Heimdall previews its telemetry briefing in the main pane. Enter opens the harness picker. On confirm agent-mux:

1. installs or refreshes the skill in that harness's directory (manifest hash check; unchanged packages are left alone),
2. builds the harness command line: model and permission flags from your profile for that harness, then the opening prompt `<invocation> <startup_prompt>` as the positional prompt (Claude Code, Codex) or via `--prompt-interactive` (Antigravity),
3. spawns the session with `AGENT_MUX_SKILL_ID`, `AGENT_MUX_BIN` (this executable) and `AGENT_MUX_TRACE_DB` (the store the TUI writes) in its environment, and records `skill_id` / `skill_harness` on the launch row so the Executions tab can find it later.

A skill is a **singleton**: one live session per skill id. While it runs, the sidebar shows `<name> [<harness>]` and the Skills view shows `running [<harness>]`, Enter attaches to it, and asking for another harness attaches with a warning. Close the session to start it on another harness. Restored sessions count.

## 5. The Skills view

Press `S` for a read-only view of every skill under each harness it declares: agent-mux packages and the harness's own native skills, each with its install state (`installed ✓`, `stale`, `not managed`, `not installed`, or `running [harness]`). The Details tab shows the package, where it is installed for that harness and the store's statistics; the Executions tab lists the sessions launched with it and the turns that loaded it, and `T` opens the Trace Browser on one of them. `1`-`3` filter by harness, `r` rescans. Installing and launching happen elsewhere: the Agents sidebar and the CLI below.

## 6. CLI

```sh
agent-mux skill list                          # compiled-in and ~/.agent-mux/skills packages
agent-mux skill show heimdall --harness codex # SKILL.md as written for that harness
agent-mux skill install heimdall [--harness claude|codex|agy|all] [--force]
agent-mux skill uninstall heimdall [--harness …]
agent-mux skill status [heimdall]             # installed / stale / not managed, per harness
```

## 7. Heimdall

Heimdall reads the store through the `agent-mux trace` CLI (`doctor`, `briefing`, `ls`, `show`, `search`, `loops`, `skills`, `skills lint`, `agents`, `compare`, `sql`) and never through anything else. `SKILL.md` carries the command reference and four playbooks (session briefing, skill evaluation, agent evaluation, drill-down); the `reference/` files hold the thresholds, ready-made SQL and report shapes for each. See `skills/heimdall/`.
