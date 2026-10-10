# Shared Memory Implementation Plan — phase 1, the memory model

**Goal:** One memory per workspace that every harness and agent launched by agent-mux reads at start and can write to: a folder of Markdown notes with a Rust-built index, delivered to Claude Code through the `SessionStart` hook agent-mux already registers and to Codex through its developer instructions, a `memory` skill that saves, recalls and forgets, a CLI and a Configuration view section. Nothing changes for a user who never saves a note.

**Architecture:** a new `src/memory/` module (paths, note, index, fence, context, log, cli) with no new dependencies; the environment and the skill at `spawn_traced_full`; the hook answer in `tracing::cli::hook_run`; the fallback flags in the launch plan; `[memory]` in `profiles.toml`; `[memory]` prompts in `prompts.toml`; `Kind::MemoryNote` in the catalog so the `C` view lists, edits and deletes notes; `agent-mux memory …`.

**Tech Stack:** Rust 2024, ratatui; frontmatter through the splitter `src/skill/mod.rs:317` already has; `time` for timestamps; no new crates.

**Spec:** [Shared memory and hand-off design](../specs/2026-10-10-shared-memory-and-handoff-design.md), sections 4, 5, 9, 10, 14 (phase 1). Phases 2–4 (the hand-off, imports and evidence, Antigravity and the keeper) get their own plans once this one has shipped and real notes exist.

## How it works, drawn

The model: one folder per workspace, written by the sessions through the `memory` skill and by Rust, read by every harness through the channel it has.

```mermaid
flowchart TD
  subgraph writers [who writes]
    skill[/memory skill\nsave · forget · brief]
    cli[agent-mux memory\nadd · rm · check]
    user[the user\nC view, $EDITOR]
  end
  subgraph folder [~/.agent-mux/memory/slug/]
    notes[notes/*.md\none fact per file]
    index[MEMORY.md\nindex, 60 lines, byte-stable]
    briefs[briefs/*.md]
  end
  subgraph rust [Rust, at spawn and in the hook]
    ctx[session_context\nhint + fenced index + brief\nrebuilds the index when a note is newer]
  end
  subgraph readers [who reads]
    claude[Claude Code\nSessionStart hook, sync]
    codex[Codex\n-c developer_instructions]
    agy[Antigravity\n$AGENT_MUX_MEMORY only]
  end
  skill --> notes
  skill --> briefs
  cli --> notes
  user --> notes
  notes --> index
  index --> ctx
  briefs --> ctx
  ctx --> claude
  ctx --> codex
  ctx --> agy
```

One Claude Code session, from its start to a saved note:

```mermaid
sequenceDiagram
  participant App as agent-mux App
  participant Claude as claude (pty)
  participant Hook as agent-mux trace hook
  participant Mem as memory folder
  App->>Mem: ensure, expire, ensure_fresh
  App->>Claude: spawn with AGENT_MUX_MEMORY, --settings (SessionStart sync), /memory skill installed
  Claude->>Hook: SessionStart {source: startup | resume | fork}
  Hook->>Mem: read MEMORY.md (rebuild if stale)
  Hook-->>Claude: hookSpecificOutput.additionalContext = hint + fenced index
  Note over Claude: the model starts knowing the index; notes stay behind the pointer
  Claude->>Mem: /memory save: write notes/slug.md (temp + rename)
  Claude->>Hook: agent-mux memory check
  Hook->>Mem: parse notes, rebuild MEMORY.md, append log.md
  Note over App,Mem: the next session on any harness in this folder gets the new index
```

## Decisions this plan takes (amend the spec when implementing)

1. **The index is injected, not only pointed at.** The `SessionStart` answer carries the hint and the whole `MEMORY.md` inside the trust fence, so a session needs no tool call to know what the memory holds. The cap (60 lines) and byte-stability make this cheap and cacheable; the notes themselves stay behind the pointer. Claude Code accepts 10,000 characters of `additionalContext` (longer output is spilled to a file with a 2,000-character preview), so the answer is cut at 9,000 and the index cap is the real limit.
2. **`SessionStart` becomes synchronous when memory is on.** Claude Code delivers an async hook's `additionalContext` "on the next conversation turn" (docs, hooks reference, 2026-10-10), which is too late. The registration (`CLAUDE_EVENTS`, `src/tracing/hooks/register.rs:138`) keeps `SessionStart` async unless the launch has memory; then it registers it synchronous with a 3 s timeout. `SessionStart` fires with `source` ∈ `startup | resume | clear | compact | fork`, so a resume, a fork and a compaction all get the current index.
3. **The hook learns the memory from its environment.** The hook process inherits the session's environment, so `AGENT_MUX_MEMORY` and `AGENT_MUX_MEMORY_BRIEF` tell it what to answer; no new hook argument, nothing in the store. For tests, `--memory <dir>` on `trace hook` overrides the variable, the way `--home` and `--launch` do.
4. **The hook answers before it touches the store.** Today `hook_run` (`src/tracing/cli.rs:2563`) returns "tracing disabled" before anything else; the memory answer is computed first, from the environment alone, so a locked or absent store never costs the session its memory.
5. **A note records the launch, not the harness's conversation id.** `source.launch` is `$AGENT_MUX_SESSION_ID`, which the skill always has; the trace store maps a launch to its conversation (`launches` → `sessions`), so nothing is lost and the skill never guesses an id.
6. **The index is rebuilt by whoever finds it stale.** `memory::index::ensure_fresh(root)` compares the newest mtime under `notes/` with `MEMORY.md` and rebuilds when needed: at spawn, in the hook, in the CLI. The skill ends a save with `agent-mux memory check` when the binary is on `PATH`; otherwise the next start rebuilds. No watcher, no daemon.
7. **Codex gets the hint through `developer_instructions` in phase 1.** The Codex lifecycle hooks (`~/.codex/hooks.json`) are an opt-in install today and the session-start answer channel is unprobed on 0.162.1; that probe and the hook path move to the phase that does Antigravity. When an agent launch already passes `-c developer_instructions=…` (`src/agents/launch.rs:174`) the hint is appended to that value, never a second `-c`.
8. **Antigravity reads the environment only in phase 1.** No instruction flag exists; the `PreInvocation` answer is phase 4. An agy session still has `$AGENT_MUX_MEMORY` and the installed `/memory` skill.

## Execution map

| Task | Files | Verify |
| --- | --- | --- |
| 1 Settings and prompts | `src/config.rs`, `src/prompts.toml`, `src/prompts.rs`, `profiles.example.toml` | unit tests |
| 2 The memory module | `src/memory/{mod,paths,note,index,fence,context,log}.rs`, `src/lib.rs`, `tests/it/main.rs` (`mod memory;`) | unit tests |
| 3 Spawn: environment, fallback flags, skill | `src/app.rs`, `src/tracing/mod.rs`, `src/tracing/hooks/register.rs`, `src/skill/mod.rs`, `skills/memory/` | `tests/it/memory.rs` (spawn) |
| 4 The hook answer | `src/tracing/cli.rs`, `src/tracing/hooks/register.rs` | `tests/it/memory.rs` (hook), `tests/it/trace_hooks.rs` |
| 5 CLI | `src/memory/cli.rs`, `src/main.rs` | `tests/it/memory.rs` (cli) |
| 6 Configuration view | `src/assets.rs`, `src/app/config_view.rs`, `src/config_cli.rs` | `tests/it/config_ui.rs`, `tests/it/config_library.rs` |
| 7 Docs | `docs/memory.md`, `README.md`, `AGENTS.md`, `docs/configuration.md`, `docs/skills.md` | review |

Order: 1 → 2 → 3 and 4 (independent) → 5 → 6 → 7. Each task lands with its tests green (`cargo test`, `cargo clippy --all-targets`, `cargo fmt`), committed on master as the repository works.

### Task 1: Settings and prompts

- [ ] `config.rs`: `MemoryConfig { enabled: Option<bool>, location: Option<String>, index_lines: Option<usize>, imports: Vec<String>, handoff: Option<String>, brief_timeout_s: Option<u64>, keeper: Option<bool> }` on `Config.memory` (`src/config.rs:330`); `ProfileMemory { enabled: Option<bool> }` on `Profile.memory`, beside `Profile.tracing` (`:14`). Phase 1 reads `enabled` (default true), `location` (`"library"` default, `"workspace"`), `index_lines` (default 60); the rest is parsed and validated so a phase-2 file already loads. Loading reports an unknown `location` as it reports any other bad value.
- [ ] `memory::Settings::from_config(&Config, &Profile) -> Option<Settings>`: `None` when memory is off globally or for the profile; `Some { location, index_lines }` otherwise.
- [ ] `prompts.toml` gains `[memory]` with `hint` (section 4.2's text, placeholders `{memory}`, `{index}`, `{brief}`), `notice` (the one-line trust notice of 6.6) and `index_footer` ("{n} older notes: `agent-mux memory ls --all`"). `prompts.rs`: `Prompts.memory`, `MEMORY_PLACEHOLDERS`, `render_memory_hint`, validation of unknown placeholders as for the loop prompts (`src/prompts.rs:156-214`).
- [ ] `profiles.example.toml`: a commented `[memory]` block.

### Task 2: The memory module

`src/memory/mod.rs` re-exports and holds `Settings`; registered in `src/lib.rs`.

- [ ] `paths.rs`: `Root { dir: PathBuf, kind: Library | Workspace }`; `root_for(settings, workspace) -> Root` = `assets::root()/memory/<history::project_slug(workspace)>/` (`src/history.rs:19`, so `$AGENT_MUX_LIBRARY_DIR` isolates tests) or `<workspace>/.agent-mux/memory/`; `user_root()` = `assets::root()/memory/_user/`; `ensure(root)` creates `notes/`, `briefs/`, `imports/` and an empty index without touching anything that exists. Constants for the file names.
- [ ] `note.rs`: `Note { name, description, kind: Kind, source: Source { harness, launch, session: Option, when }, confidence: Confidence, pinned: bool, expires_at: Option<Date>, body }`; `Kind` = `user | feedback | project | reference | decision | handoff`; `parse(text) -> Result<Note, Problem>` on top of the frontmatter splitter (`split_frontmatter`, `src/skill/mod.rs:317`, made `pub(crate)`; the nested `source:` map is read as the flattened `source.harness` keys `tracing::inventory::parse_frontmatter` (`src/tracing/inventory.rs:101`) already produces); `render(&Note) -> String` writes the exact shape of spec 4.1; `write(root, &Note)` = temp file + rename in `notes/` (`persistence::save_sessions`, `src/persistence.rs:44-52`); `slug(name)` = `^[a-z0-9][a-z0-9-]*$`; `template(kind)` for `n` and `memory add`. A Claude auto-memory file (`name`, `description`, `metadata.type`) parses with `confidence: imported` so phase 3's import is a parse.
- [ ] `index.rs`: `rebuild(root, cap) -> String`: read every `notes/*.md`, order pinned → kind → name, one line `- [name](notes/x.md) — description`, cut at `cap` with the footer line; `## User` lines from `_user/` when the root is a workspace root (so one injected block covers both); nothing time-dependent in the text. `ensure_fresh(root, cap) -> bool` rebuilds when any note is newer than `MEMORY.md` or the index is missing. `expire(root, today) -> Vec<name>` moves notes past `expires_at` to `notes/.expired/`. Byte-stability test: two rebuilds of the same folder are identical; a rebuild after `touch` is identical.
- [ ] `fence.rs`: `fence(notice, body) -> String` with the markers `<!-- agent-mux:memory:start -->` / `end`; `escape(body)` breaks any `-->` and any line equal to a marker; `unfence` is not needed. Test: a note body that contains the end marker cannot close the fence.
- [ ] `context.rs`: `session_context(root, brief: Option<&Path>, prompts) -> String`: the rendered hint, then the fenced index (and, when `brief` is set, the fenced brief file with the one-line footer of spec 6.2 — written now so phase 2 only has to set the variable), cut at 9,000 characters with a last line saying the index was cut. This is the one text the hook, the Claude fallback flag and the Codex instructions all use, so every harness reads the same words.
- [ ] `log.rs`: `append(root, line)` to `log.md`, `2026-10-10T12:30 <harness> <launch> <op> <path>`; best effort, never an error for the caller.

### Task 3: Spawn: environment, fallback flags, skill

- [ ] `App::memory_for(&profile, &dir) -> Option<memory::Launch>`: resolves `Settings`, the root, calls `ensure`, `expire`, `ensure_fresh`; returns the environment triple of spec 4.2 (`AGENT_MUX_MEMORY`, `AGENT_MUX_MEMORY_INDEX`; `AGENT_MUX_MEMORY_BRIEF` only when a brief is addressed, which phase 2 sets) and whether the hook will deliver the context (`plan.hooks_registered` and provider Claude).
- [ ] `spawn_traced_full` (`src/app.rs:3553`): after the plan, before `Session::spawn`, push the memory environment for every launch (interactive, restored, respawned, loop, workflow step) and add the fallback:
  - Claude without a registered hook (`plan` is `None`, hooks off, or the profile is in `injection_disabled`): `--append-system-prompt <hint with pointers only>` (never the index: the system prompt is snapshotted on resume, spec principle 2);
  - Codex: `-c developer_instructions=<hint>`; when `extra_args` already holds one (an agent launch), replace its value with the agent's text + a blank line + the hint (`memory::codex_instructions(&mut extra_args, hint)`);
  - Antigravity: environment only.
- [ ] `tracing::plan_launch` (`src/tracing/mod.rs:745-785`): `Registration` gains `memory: bool`; `claude_hooks` registers `SessionStart` as synchronous (`"async"` absent, `"timeout": 3`) when `reg.memory`; `LaunchPlan.hooks_registered` already tells the App whether the fallback is needed. The App decides `memory` before `plan_launch` (settings only, no I/O), so `plan_launch(profile, dir, memory: bool)` or a field on the profile overlay; pick the smaller change when editing.
- [ ] `skills/memory/SKILL.md` + `skill.toml` (`name = "Memory"`, `hidden = true`, `writes = true`, `harnesses = ["claude", "codex", "agy"]`): the playbooks of spec 5.2 (save: check the index for a note that covers it and update that one, one fact per file, the frontmatter of 4.1 with `source.launch = $AGENT_MUX_SESSION_ID`, never edit `MEMORY.md`, then `agent-mux memory check` when available; recall: the index, then the matching notes, answer with sources under the trust notice; forget: only on the user's word; brief: the shape of 6.1 to `$AGENT_MUX_MEMORY/briefs/<id>.md`, written now so phase 2 only wires it). Registered in `skill::builtin_packages` (`src/skill/mod.rs:88`), which puts it in the catalog (`src/assets.rs:374`) and lets `~/.agent-mux/skills/memory/` shadow it.
- [ ] `App::ensure_memory_skill(harness)`: installs the package user-level once per harness per run (`skill::install::install`, the pattern of `src/app/new_agent.rs:283`), remembered in `App.memory_skill_installed: HashSet<Harness>`; a failure is a notice, the launch goes on. Called from `spawn_traced_full` when memory is on.
- [ ] Tests (`tests/it/memory.rs`, spawn): a fake harness script that dumps argv and env (as `fake_claude` in `tests/it/persistent_sessions.rs`): an App with memory on spawns with `AGENT_MUX_MEMORY` pointing under `$AGENT_MUX_LIBRARY_DIR/memory/<slug>/` and the index file in place; with hooks off the Claude line carries `--append-system-prompt` with the pointer and no note text; a Codex agent launch has exactly one `developer_instructions` holding both texts; `[profiles.memory] enabled = false` gives neither; `location = "workspace"` puts the folder under the workspace.

### Task 4: The hook answer

- [ ] `hook_run` (`src/tracing/cli.rs:2563`): right after parsing the payload, when `source == Claude` and the event is `SessionStart`, resolve the memory dir (`--memory`, else `AGENT_MUX_MEMORY`), and if it exists answer `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext": session_context(...)}}` into `outcome.response` before the config and store steps; `ensure_fresh` runs here too; the brief path comes from `AGENT_MUX_MEMORY_BRIEF`. The store part keeps its current flow and its "tracing disabled" result on `outcome.error`, which does not suppress the response (`src/tracing/cli.rs:256` prints the response whenever there is one). `log.rs` records `read` with the launch id.
- [ ] `HookOutcome` gains nothing; the existing `response` field carries the answer. A `SessionStart` with `source = compact` gets the same answer (the context survives compaction that way).
- [ ] Tests: `hook_run` with `--memory <dir>` and a SessionStart payload answers `additionalContext` whose text holds the hint, the fence markers and every index line; the answer stays under 10,000 characters for a folder with 500 notes (the cap and the cut); no `--memory` and no variable → no response, rows land as today (`tests/it/trace_hooks.rs` unchanged); the store locked (`hook_gives_up_within_budget_when_the_store_is_locked`) still answers the memory. Registration: `claude_settings_json` with `memory = true` has `SessionStart` without `async` and with `timeout: 3`, with `memory = false` as today (`register.rs` unit tests).
- [ ] Probe on the installed Claude Code 2.1.296 and record in `src/memory/mod.rs` docs, as `AGENTS.md` asks: an inline `--settings` `SessionStart` answering `additionalContext` reaches the first turn (ask the session what its memory index says); a `--resume` and a `--resume --fork-session` start get it again; a 9,000-character answer is taken whole. The probe is a one-off script under the scratchpad, not a test.

### Task 5: CLI

- [ ] `src/memory/cli.rs`, dispatched from `src/main.rs:95-103` as `agent-mux memory …`: `ls [--all] [--json]` (the index order, `--all` past the cap, `--workspace DIR` defaults to the current dir, `--user` for `_user`), `show <name>` (the note as is), `add <name> --kind K --description D [--body text | from stdin] [--pinned] [--expires YYYY-MM-DD]` (source `agent-mux`, confidence `stated`; `--launch` for the skill's use), `rm <name>` (moves to `notes/.removed/`, never deletes), `check` (parse every note, report problems, expire, rebuild; exit 1 on problems), `path` (which root is in use and why). Usage text in the same voice as `config_cli.rs`.
- [ ] Tests: add → ls shows one line → show prints it → check rebuilds the index after a hand-edited note → rm moves it and the index drops it → `path` reflects `location`.

### Task 6: Configuration view

- [ ] `assets.rs`: `Kind::MemoryNote` (header `Memory`, kind name `memory note`); `Catalog::scan_memory()` lists `<root>/memory/*/notes/*.md` and `_user/notes/*.md` as `Source::User` named `<slug> · <name>` (`_user · <name>`), validated through `memory::note::parse`; `new_item` for the kind writes `note::template` into `_user/notes/<name>.md` (a workspace note is born in a session or with `memory add`); the reset question on a user item already deletes it (`Pending::Reset`), which for a note moves it to `.removed/` like `rm`. `Kind::MemoryNote` joins the `Kind::ALL` order after `Kind::Agent`.
- [ ] `config_view.rs`: the detail pane shows the frontmatter as rows (kind, source, confidence, pinned, expires) above the body; nothing else changes (`e` edits in `$EDITOR`, `n` creates, `R` removes, `Ctrl+R` reloads). `agent-mux config ls` lists the notes with the rest (`config_cli.rs` needs no change beyond the kind name).
- [ ] Tests: `config_ui.rs` — a note under the library root appears under `Memory`, `n` on the header creates a `_user` note from the template, `R` on it moves it to `.removed/`; `config_library.rs` — `config ls --json` includes `kind: "memory note"` and `config check` reports a note with a bad `kind`.

### Task 7: Docs

- [ ] `docs/memory.md`: the folder, a note, the index, what a session is given per harness (with the probed facts and what is still fallback), the `memory` skill's commands per harness (`/memory`, `$memory`), the CLI, `[memory]` settings, the trust fence, what phase 2–4 will add.
- [ ] `README.md`: a Memory paragraph in the features and the settings table; `AGENTS.md`: a `## Memory (src/memory/, skills/memory)` section in the style of the others; `docs/configuration.md`: the `Memory` group and the `[memory]` prompts; `docs/skills.md`: the `memory` package among the hidden built-ins.

## Later plans (not in this one)

- **Phase 2, the hand-off** (spec 6): the Continue dialog, the brief request to a live session, the facts-only brief, `f` on History rows, `continued_from` as a struct and `launches.metadata.continued_from`, `move` on the same harness. Depends on `context.rs` carrying a brief and the skill's `brief` playbook, both written here.
- **Phase 3, imports and evidence** (spec 7, 9): `imports/claude.md` and `imports/codex.md`, `memory_events` (schema vNext) fed from the hook rows that carry a path, the dossier line, the `✎ n notes` sidebar mark.
- **Phase 4, Antigravity and the keeper** (spec 5, 8): the `PreInvocation` answer once probed on 1.3.3, Codex session-start through `~/.codex/hooks.json` once probed on 0.162.1, the `memory-keeper` agent.
