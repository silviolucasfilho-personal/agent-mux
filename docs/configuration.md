# Configuring agent-mux: prompts, skills, loops and agents

Everything a harness reads from agent-mux is text you can change: the prompts agent-mux composes, the Heimdall skill, the loop patterns, the loop skills, the loop agents and the workspace templates. agent-mux ships them compiled in and reads your copies from one **configuration library** under `~/.agent-mux/`. The **Configuration view** (`C`) and `agent-mux config …` list every item, open it in your editor, reset it, create new ones and push edited loop files into your workspaces.

Design: `docs/superpowers/specs/2026-09-17-configuration-library-design.md`.

---

## 1. The library

```text
~/.agent-mux/                       ($AGENT_MUX_LIBRARY_DIR overrides the root)
├── prompts.toml                    the prompts agent-mux composes (section 3)
├── profiles.toml                   harness profiles, tracing, agents, loops, editor
├── skills/<id>/…                   skill packages ($AGENT_MUX_SKILLS_DIR still applies)
└── loops/
    ├── registry.toml               patterns: same id replaces the built-in, a new id is added
    ├── skills/<name>/SKILL.md      loop skills: same name replaces the built-in, a new name is added
    ├── agents/<name>.md            loop agents: same name replaces the built-in, a new name is added
    └── templates/<file>            the workspace templates, by file name
```

Resolution is per file: when the library has the file, that is the effective text; otherwise the compiled-in text is. Two files are keyed documents merged key by key: `prompts.toml` (a key you set replaces that key) and `loops/registry.toml` (a pattern id you set replaces that pattern; a new id is appended).

The compiled-in text never changes on disk; a reset only deletes your copy.

## 2. The Configuration view (`C`)

```text
┌─ Configuration (26)  ~/.agent-mux ──┬─ loops/skills/loop-triage/SKILL.md ─────────────────────────┐
│ Prompts                             │  Kind        loop skill                                      │
│ > prompts.toml           built-in   │  Source      override  ~/.agent-mux/loops/skills/loop-triage │
│ Settings                            │  Built-in    loops/skills/                                   │
│   profiles.toml          user       │  Status      valid                                           │
│ Skills                              │  Used by     patterns daily-triage                           │
│   heimdall/SKILL.md      built-in   │  Workspaces  2 workspace copy(ies), 1 differ or missing (u…) │
│   …                                 │  ───────────────────────────────────────────────────────────  │
│ Loop skills                         │  ---                                                         │
│   loop-triage/SKILL.md   override   │  name: loop-triage                                           │
└─────────────────────────────────────┴─────────────────────────────────────────────────────────────┘
 [Enter/e] edit  [n] new  [R] reset  [u] push to workspaces  [r] rescan  [←/→] pane  [Esc] close
```

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k`, `PgUp`/`PgDn` | Move over items (headers are skipped); in the right pane, scroll the text. |
| `←`/`→`, `Tab` | Focus the list or the detail pane. |
| `Enter`, `e` | Edit: a built-in item is first copied into the library, then the file opens in your editor. agent-mux leaves the screen, waits, and comes back; sessions keep running. On return the item is re-validated and whatever reads it reloads. |
| `n` | New item, in the Skills, Loop skills or Loop agents group: type a name, `Enter` creates it from a skeleton and opens it. New patterns are `[[patterns]]` tables inside `loops/registry.toml`. |
| `R` | Reset after `y`: deletes the library file. An override falls back to the built-in text; a user item is removed. `profiles.toml` is never deleted. |
| `u` | Push after `y`: rewrites the loop skills and agents in every registered loop's workspace with the effective text. Contract files (`LOOP.md`, state files, `gate.yaml`, …) are never touched. |
| `r` | Rescan the library. |
| `Esc`, `q` | Back to the list, then close. |

The source column says `built-in`, `override` or `user`; a red row failed validation and the detail pane lists why. The detail pane also shows where a loop skill or agent is installed and whether each workspace copy matches the effective text.

The editor is `editor` in `profiles.toml` (`editor = "code --wait"`), else `$VISUAL`, else `$EDITOR`, else `vi`. A GUI editor must block until the file is closed (`--wait`, `-w`).

## 3. Prompts (`prompts.toml`)

```toml
[loop]
# {invocation} {pattern} {state_file} {workspace} {level} {harness}
run = "{invocation} Run the {pattern} loop for this workspace. Facts for this run are in $AGENT_MUX_LOOP_CONTEXT (read it first). Update the state file at {state_file}. Finish with a loop-result block."

[skill]
hydration_hint = "Read the briefing snapshot at $AGENT_MUX_BRIEFING …"
```

- `loop.run` opens every loop run. It must contain `{invocation}` (the triage skill as the harness invokes it: `/loop-triage` or `$loop-triage`); the other placeholders are optional. A pattern's own `prompt` in `registry.toml` replaces it for that pattern.
- `skill.hydration_hint` is appended to a skill launch that received a briefing snapshot (`[agent] hydrate = ["briefing"]` in the package's `skill.toml`).
- A skill's opening sentence is `startup_prompt` in its `skill.toml`, already per package; Heimdall's is `skills/heimdall/skill.toml` in the library once you edit it.
- `agent.preamble` is inserted as a `## Baseline` section right after the frontmatter of every loop agent the scaffolder or `config push` installs (inside the instructions for Codex). Leave it empty to install agents as written.

Prompts are read at launch time: an edit applies to the next run without a restart.

## 4. Skills

Every file of a skill package is one item: `skills/heimdall/SKILL.md`, `skills/heimdall/skill.toml`, `skills/heimdall/reference/*.md`. Editing one copies it into `~/.agent-mux/skills/heimdall/`; from then on that directory is the Heimdall package (`docs/skills.md` section 2), and the Agents sidebar reinstalls it into each harness on the next launch. `n` creates a new package with a `SKILL.md` and a `skill.toml` skeleton.

## 5. Loops

- **Patterns** (`loops/registry.toml`): every `[[patterns]]` table, same fields as the built-in file (`docs/loops.md` section 3) plus the optional `prompt` and `agents`. Validation checks unique ids, that every listed skill is a built-in or library loop skill, that every listed agent is a built-in or library loop agent (and that a verifier pattern keeps `loop-verifier`), that `loop-rules` is listed, a state file the guard permits, and an interval of at least five minutes. A file that does not parse is reported and the built-in patterns stay in use.
- **Loop skills** (`loops/skills/<name>/SKILL.md`): frontmatter `name` must equal the directory name. A new skill becomes usable by listing it in a pattern's `skills`.
- **Agents** (`agents/<name>.toml`): workflow agents, the persona a workflow step runs as (`agent = "<name>"`): `name`, `description`, `instructions`, canonical `tools`, `model`, `effort` and `[backends.<harness>]`. Five ship built in (`reviewer`, `skeptic`, `planner`, `doc-writer`, `judge`, from `agents/builtin/`). Like every built-in here, `Enter` edits your copy in the library and `R` restores the built-in. `agent-mux agent new --template` starts from a built-in or a blank one, and a workspace's `.agent-mux/agents/` wins over the library. Guide: `docs/agents.md`.
- **Loop agents** (`loops/agents/<name>.md`): Claude's agent file shape (`name`, `description`, `tools`, `model`, then the body). Two ship built in, `loop-verifier` (runs the tests) and `loop-reviewer` (reads the diff only); a pattern receives the agents its `agents` list names, or the verifier alone when the list is empty and `verifier = true`, and listing both checkers makes a fix wait for both to approve. Every installed copy opens with the `## Baseline` section from `[agent] preamble` of `prompts.toml`. Codex receives the same text as `<name>.toml`: `name`, `description`, `developer_instructions` (the body), `model` when the frontmatter names one that is not a Claude alias, and the body again under `[system_prompt] content`; a Claude `tools` list has no Codex counterpart and is left out. `agent-mux agent import <file>` copies an agent file written for Claude Code into this directory after validating its frontmatter and printing a content lint (secret-looking strings, prompt-injection phrasing, over-broad tool lists).
- **Templates** (`loops/templates/<file>`): the scaffolder fills `{{PROJECT}}`, `{{PATTERN}}`, `{{CADENCE}}`, `{{LEVEL}}`, `{{STATE_FILE}}`, `{{HARNESS}}`, `{{GATES}}`, `{{ROW}}`, `{{GOAL}}`; any other marker is reported. `gate.yaml` and `loop-ledger.json` must parse.

The scaffolder never overwrites a workspace file, so an edited loop skill or agent reaches an existing workspace through `u` in the view or `agent-mux config push`.

## 6. Command line

```sh
agent-mux config ls [--json]              every item with kind, source and status
agent-mux config show <id> [--builtin]    the effective (or compiled-in) text
agent-mux config path [<id>]              the library root, or an item's library path
agent-mux config edit <id>                copy the built-in text if needed, open the editor, validate
agent-mux config reset <id>               delete the override (or a user item)
agent-mux config new skill|loop-skill|loop-agent|workflow|agent <name>
agent-mux config check                    validate everything; exit 1 on problems
agent-mux config push [--dry-run]         rewrite loop skills and agents in registered workspaces
agent-mux agent import <file> [--force]   copy a Claude-shaped agent file into loops/agents/
agent-mux agent ls                        workflow agents (docs/agents.md), then the loop agents
agent-mux agent new|show|check|templates  workflow agents: docs/agents.md
```

An `<id>` is the path under the library or any unique suffix or name: `loop-triage`, `registry.toml`, `prompts.toml`, `heimdall/skill.toml`.

## 7. Files and variables

| Location / variable | Meaning |
| --- | --- |
| `~/.agent-mux/` / `AGENT_MUX_LIBRARY_DIR` | The library root. |
| `~/.agent-mux/skills/` / `AGENT_MUX_SKILLS_DIR` | The skills subtree; the variable keeps its earlier meaning. |
| `editor` in `profiles.toml`, `$VISUAL`, `$EDITOR` | The editor, in that order; `vi` otherwise. |
