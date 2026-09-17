# Configuration Library Implementation Plan

**Goal:** Make every prompt, skill, loop pattern, loop skill, loop agent and template agent-mux ships editable from the TUI and the CLI, through one library of override files under `~/.agent-mux/`.

**Architecture:** `src/assets.rs` (catalog: enumeration, resolution, override/reset/new, validation, push), `src/prompts.rs` + `src/prompts.toml` (the composed prompts), consumers read the library (`loops::patterns`, `loops::scaffold`, the loop launch, the skill launch), `src/app/config_view.rs` + `draw_config_view` (the `C` view), an editor round-trip in `main.rs`, and `src/config_cli.rs` (`agent-mux config …`).

**Tech Stack:** Rust 2024, ratatui; no new dependencies.

**Spec:** [Configuration library design](../specs/2026-09-17-configuration-library-design.md)

## Execution map

| Task | Files | Verify |
| --- | --- | --- |
| 1 Catalog and prompts | `src/assets.rs`, `src/prompts.rs`, `src/prompts.toml`, `src/lib.rs` | unit tests |
| 2 Consumers | `src/loops/patterns.rs`, `src/loops/mod.rs`, `src/loops/scaffold.rs`, `src/app/loops.rs`, `src/skill/launch.rs`, `src/config.rs` | `cargo test loop`, `skill` |
| 3 View and editor | `src/app/config_view.rs`, `src/app.rs`, `src/ui.rs`, `src/main.rs`, `src/events.rs` | `tests/config_ui.rs` |
| 4 CLI | `src/config_cli.rs`, `src/main.rs` | `tests/config_library.rs` |
| 5 Docs | `docs/configuration.md`, `README.md`, `AGENTS.md`, `docs/skills.md`, `docs/loops.md` | review |

### Task 1: Catalog and prompts

- [x] `assets::root()` (`$AGENT_MUX_LIBRARY_DIR`, else `~/.agent-mux`), `Kind`, `Source`, `Asset`, `Catalog::load(root)`.
- [x] Built-in table: `prompts.toml`, Heimdall files, `loops/registry.toml`, nine loop skills, `loop-verifier`, eight templates. Library scan adds user skills, loop skills, agents.
- [x] `effective`, `create_override`, `reset`, `new_item`, `validate`, `workspace_copies`, `push`.
- [x] `prompts::{Prompts, builtin, load, render_loop_run, placeholders}`.

### Task 2: Consumers

- [x] `patterns::all()` merges the library registry; `patterns::reload()`; `Pattern.prompt`.
- [x] `scaffold` reads the catalog; user agents installed; `codex_agent_toml(name, body)`.
- [x] Loop launch renders `prompts.loop.run` (pattern `prompt` first); skill launch appends `prompts.skill.hydration_hint`.
- [x] `Config.editor`.

### Task 3: View and editor

- [x] `ConfigViewState`: rows, selection, focus, scroll, pending confirm, name input, detail lines.
- [x] `Mode::ConfigView`, `Action::OpenConfigView` (`C`), `Action::ConfigKey`, `App::handle_config_key`, `editor_request`, `editor_finished`.
- [x] `draw_config_view`, hints, help.
- [x] `main.rs`: pausable input thread, `run_editor` between events and draw.

### Task 4: CLI

- [x] `agent-mux config ls|show|path|edit|reset|new|check|push`.

### Task 5: Docs

- [x] `docs/configuration.md`; README section; AGENTS.md; pointers in `docs/skills.md` and `docs/loops.md`.
