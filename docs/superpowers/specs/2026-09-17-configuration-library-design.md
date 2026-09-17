# Configuration library: every prompt, skill, loop and agent is editable from agent-mux

Status: Implemented on 2026-09-17 (see the plan of the same date).
Date: 2026-09-17
Baseline: `f04389d` on `feat/loop-engineering`.

## 1. Problem and outcome

agent-mux ships every piece of text a harness reads as an `include_str!` constant: the Heimdall package (`skills/heimdall`), the seven loop patterns (`loops/registry.toml`), the nine loop skills (`loops/skills/*/SKILL.md`), the verifier agent (`loops/agents/loop-verifier.md`) and the eight workspace templates (`loops/templates/*`). The two prompts agent-mux composes itself, the loop run prompt (`src/app/loops.rs`) and the hydration hint (`src/skill/launch.rs`), are `format!` strings. Only the Heimdall package can be overridden today (a package in `~/.agent-mux/skills/heimdall/` shadows the compiled-in one), and nothing in the TUI or the CLI edits any of it.

Outcome: one **configuration library** under `~/.agent-mux/` that shadows every compiled-in asset file by file, a **Configuration view** (`C`) that lists each item with its source and validation state, opens it in the user's editor, resets it to the built-in text, creates new skills, loop skills and loop agents, and pushes edited loop skills and agents into the workspaces of registered loops, and a matching `agent-mux config …` command line. After an edit the running agent-mux uses the new text: the next skill launch, loop run and scaffold read the library.

Out of scope: an in-TUI multi-line text editor (the external editor is the editor), editing harness-native skills that agent-mux does not own, editing the MCP tool descriptions (protocol text, not prompts), and editing the pricing table (already configurable through `[[tracing.models]]`).

## 2. The library

```text
~/.agent-mux/                       ($AGENT_MUX_LIBRARY_DIR overrides the root)
├── prompts.toml                    the prompts agent-mux composes (section 3)
├── profiles.toml                   harness profiles, tracing, agents, loops, editor (existing)
├── skills/<id>/…                   skill packages; shadow a compiled-in id (existing)
└── loops/
    ├── registry.toml               patterns; same id replaces the built-in, new ids are added
    ├── skills/<name>/SKILL.md      loop skills; same name replaces the built-in, new names are added
    ├── agents/<name>.md            loop agents; same name replaces the built-in, new names are added
    └── templates/<file>            the workspace templates, by file name
```

Resolution is per file: a file present in the library is the effective text, otherwise the compiled-in one is. Nothing is merged inside a file, except `prompts.toml` and `loops/registry.toml`, which are keyed documents: a prompt key or a pattern id present in the library replaces that key or id only.

`$AGENT_MUX_SKILLS_DIR` keeps its meaning (the skills subtree only) so existing setups do not move.

### The catalog (`src/assets.rs`)

`Catalog::load(root)` enumerates every configurable item: the built-in ones, and every file the library adds. Each `Asset` carries a `Kind` (`Prompts`, `Settings`, `Skill`, `LoopPattern`, `LoopSkill`, `LoopAgent`, `LoopTemplate`), an `id` that is its path under the library root (`loops/skills/loop-triage/SKILL.md`), the compiled-in text when there is one, the library path, and a `Source` (`Builtin`, `Override`, `User`). The catalog reads effective text, creates an override (copies the built-in text into place), resets (deletes the library file), scaffolds a new item from a skeleton, validates, and reports where a loop skill or agent is installed and whether those copies match.

`Settings` is `profiles.toml`: the file `config::load()` accepted, or `~/.agent-mux/profiles.toml` when none exists. It has no built-in text; a reset is refused.

Validation per kind, all read-only and never fatal:

| Kind | Checks |
| --- | --- |
| Prompts | parses as `prompts.toml`; `loop.run` names `{invocation}`; every `{placeholder}` is known |
| Settings | `config::parse` succeeds |
| Skill | `skill::load_skill_dir` succeeds (frontmatter `name` equals the directory, description rules, `skill.toml` shape) |
| LoopPattern | parses as a registry; ids unique; every listed skill resolves to a loop skill; `loop-rules` listed; state file known; interval ≥ 300 s |
| LoopSkill | frontmatter `name` equals the directory name; non-empty description |
| LoopAgent | frontmatter `name` equals the file stem; non-empty description |
| LoopTemplate | every `{{PLACEHOLDER}}` is one the scaffolder fills; `gate.yaml` and `loop-ledger.json` parse |

## 3. Prompts

`src/prompts.toml` (compiled in) is the built-in `prompts.toml`:

```toml
[loop]
# Placeholders: {invocation} {pattern} {state_file} {workspace} {level} {harness}
run = "{invocation} Run the {pattern} loop for this workspace. Facts for this run are in $AGENT_MUX_LOOP_CONTEXT (read it first). Update the state file at {state_file}. Finish with a loop-result block."

[skill]
hydration_hint = "Read the briefing snapshot at $AGENT_MUX_BRIEFING … for anything newer."
```

`prompts::load(root)` returns the built-in values with the library's keys applied. A pattern may carry its own `prompt` in `registry.toml`, which replaces `loop.run` for that pattern. A skill's opening sentence stays where it is, `startup_prompt` in `skill.toml`, already per package. The loop run and the hydrated skill launch read the prompts at launch time, so an edit takes effect on the next run without a restart.

## 4. Consumers

| Consumer | Before | After |
| --- | --- | --- |
| `loops::patterns::all()` | `OnceLock` over the embedded TOML | built-in patterns with the library's `registry.toml` merged by id; cached, `reload()` after an edit; a library file that fails to parse is reported and ignored |
| `loops::scaffold` | `embedded_skill`, `verifier_body`, `template` | the catalog's effective text; every library agent is installed next to the verifier |
| `skill::load_skills` | unchanged | unchanged (`skills/` already shadows) |
| `App` loop launch | `format!` | `prompts::render_loop_run` |
| `skill::launch` | `HYDRATION_HINT` | `prompts.skill.hydration_hint` (the constant stays as the built-in) |
| `config` | — | top-level `editor = "…"`; `App` reloads profiles, agents and loops settings after `profiles.toml` is edited |

`Pattern` gains `prompt: Option<String>`.

## 5. The Configuration view (`C`)

```text
┌─ Configuration (27) ────────────────┬─ loops/skills/loop-triage/SKILL.md ─────────────────────────┐
│ Prompts                             │ Kind        loop skill                                      │
│ > prompts.toml           built-in   │ Source      override  ~/.agent-mux/loops/skills/loop-triage │
│ Settings                            │ Built-in    loops/skills/loop-triage/SKILL.md               │
│   profiles.toml          user       │ Status      valid                                           │
│ Skills                              │ Used by     daily-triage · installed in 2 workspaces (1 differs) │
│   skills/heimdall/SKILL.md override │ ──────────────────────────────────────────────────────────── │
│   skills/heimdall/skill.toml built-in│ ---                                                        │
│ Loop patterns                       │ name: loop-triage                                           │
│   loops/registry.toml    built-in   │ description: Use when agent-mux runs the daily-triage …     │
│ Loop skills                         │ …                                                            │
│   loops/skills/loop-triage/SKILL.md override                                                      │
└─────────────────────────────────────┴─────────────────────────────────────────────────────────────┘
 [Enter/e] edit  [n] new  [R] reset  [u] push to workspaces  [r] rescan  [←/→] pane  [Esc] close
```

- **Left pane.** Rows grouped under a header per kind in the order Prompts, Settings, Skills, Loop patterns, Loop skills, Loop agents, Loop templates. The right-hand column is the source: `built-in`, `override`, `user`. A row whose validation failed is red.
- **Right pane.** Kind, source and library path, the repository path of the built-in text, status (or every problem), where the item is used (which patterns list a loop skill, which registered loop workspaces hold a copy and whether it matches), the placeholders a prompt or template accepts, then the effective text. `←/→` moves focus; `j/k` scroll the text when the right pane has focus.
- **`Enter` / `e`.** Creates the override when the row is built-in, then opens the file in the editor. The editor is `editor` from `profiles.toml`, else `$VISUAL`, else `$EDITOR`, else `vi`. The TUI leaves the alternate screen, pauses its input thread, runs the editor, and comes back; sessions keep running. On return the catalog rescans, the item is validated, and the consumers reload: skills, patterns, or the settings. The notice reports the result; problems keep the file (nothing is reverted) and stay visible in the row.
- **`n`.** In the Skills, Loop skills and Loop agents groups, asks for a name in the footer and creates the item from a skeleton, then opens it. Elsewhere the notice explains where new items go (patterns are added inside `registry.toml`).
- **`R`.** After `y`, deletes the library file: an override falls back to the built-in text; a user item is removed. Refused for Settings.
- **`u`.** After `y`, rewrites the loop skills and agents in every registered loop's workspace for its harness with the effective text. Contract files are never touched; the notice reports the counts.
- **`r`** rescans; `Esc`/`q` unwinds focus, then closes.

The editor round-trip lives in `main.rs` as a request the `App` records (`editor_request`) and the loop fulfils between events and drawing, so `App` stays testable without a terminal: tests observe the request and call `editor_finished` themselves.

## 6. Command line

```sh
agent-mux config ls [--json]              every item with its kind, source and status
agent-mux config show <id> [--builtin]    the effective (or compiled-in) text
agent-mux config path [<id>]              the library root, or an item's library path
agent-mux config edit <id>                create the override if needed and open the editor
agent-mux config reset <id>               delete the override (or the user item)
agent-mux config new skill|loop-skill|loop-agent <name>
agent-mux config check                    validate everything; exit 1 on problems
agent-mux config push [--dry-run]         rewrite loop skills and agents in registered workspaces
```

An `<id>` may be abbreviated to any unique suffix or substring (`loop-triage`, `registry.toml`).

## 7. Alternatives

| Approach | Benefit | Limitation |
| --- | --- | --- |
| Library of override files plus external editor (chosen) | Every file is plain text under one directory; matches how `skills/` already works; no editor to maintain | Needs a terminal round-trip; a headless environment uses the CLI |
| In-TUI multi-line editor | Never leaves the TUI | A second editor to maintain, without the user's key bindings, undo or syntax highlighting |
| Only `profiles.toml` keys for the prompts | Smallest change | Does not cover skills, agents, templates or patterns |

## 8. Testing

Unit tests in `src/assets.rs`, `src/prompts.rs` and `src/loops/patterns.rs` on temporary roots; `tests/config_library.rs` for the catalog, the merged patterns, the scaffolder reading the library and the CLI; `tests/config_ui.rs` for the view (rows, navigation, the editor request, reset, new item, push). The invariant test of the compiled-in registry stays on the built-in set.
