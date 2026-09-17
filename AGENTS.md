# AGENTS.md — repository guide for coding agents

`agent-mux` is a terminal multiplexer for AI coding harnesses (Claude Code `claude`, Codex CLI `codex`, Google Antigravity `agy`) with a local SQLite trace store (`~/.agent-mux/traces.db`) and a read-only analysis CLI (`agent-mux trace …`).

## Skills

The only packages agent-mux manages are **skills**: a harness-neutral `SKILL.md` plus optional `skill.toml` and `reference/*.md`, installed into each harness's own skill directory and launched from the Agents sidebar; the Skills view (`S`) is a read-only look at skills and their usage. Details in `docs/skills.md`; code in `src/skill/`.

### Heimdall (`skills/heimdall`)

Heimdall briefs the user on active and recent sessions and evaluates skills and agents (subagents) in depth. Rust computes its facts: a briefing snapshot is written before launch (`$AGENT_MUX_BRIEFING`) and the read-only MCP server (`agent-mux mcp serve`) answers follow-ups where the harness allows it, with the `agent-mux trace` CLI as fallback. Its `SKILL.md` holds the tool/command table and playbooks; `reference/sessions.md`, `reference/skills.md` and `reference/agents.md` hold thresholds and report shapes; ad-hoc SQL is in `docs/trace-sql-examples.md`. It is compiled into the binary; a package at `~/.agent-mux/skills/heimdall/` shadows it.

Invocation per harness: `/heimdall` (Claude Code, Antigravity), `$heimdall` (Codex). Install with `agent-mux skill install heimdall`; the Agents sidebar launcher installs it automatically before each launch.

## Loops (`src/loops/`, `loops/`)

Loop Engineering: scheduled, bounded, gated runs against one workspace from the Loops sidebar section (`Tab`, `E` for the view, `K` kill switch) and `agent-mux loop …`. The nine loop skills under `loops/skills`, the verifier under `loops/agents` and the templates under `loops/templates` are agent-mux's own and are installed at project level into the workspace by the scaffolder; they are not Agents-sidebar packages. Facts a run reasons about (budget, breaker, readiness, tokens, files touched) are computed in Rust and handed over in `$AGENT_MUX_LOOP_CONTEXT`; the `PreToolUse` guard enforces the `gate.yaml` denylist, report-only runs and the no-push rule. Claude Code and Codex only; Antigravity is deferred (spec section 16). Guide: `docs/loops.md`; design: `docs/superpowers/specs/2026-09-15-loop-engineering-design.md`. Nothing from another vendor is vendored or executed.

## Configuration library (`src/assets.rs`, `src/prompts.rs`, `src/config_cli.rs`, `src/app/config_view.rs`)

Every compiled-in text (the prompts in `src/prompts.toml`, the Heimdall files, `loops/registry.toml`, the loop skills, the verifier, the templates) is shadowed file by file from `~/.agent-mux/` (`$AGENT_MUX_LIBRARY_DIR`); `prompts.toml` and `registry.toml` merge by key and by pattern id. The Configuration view (`C`) and `agent-mux config ls|show|path|edit|reset|new|check|push` list, edit (external editor: `editor` in profiles.toml, `$VISUAL`, `$EDITOR`, `vi`), reset, create and push items. Consumers read the effective text: `loops::patterns::all()` (cached, `reload()`), `loops::scaffold` (`scaffold_with_library`), the loop launch prompt (`prompts::render_loop_run`, a pattern's `prompt` first) and the hydration hint. When you add a compiled-in asset, register it in `Catalog::load` so it is editable. Guide: `docs/configuration.md`; design: `docs/superpowers/specs/2026-09-17-configuration-library-design.md`.

## Working in this repository

- `cargo build`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt`.
- Trace store schema: `src/tracing/store/schema.rs` (migrations through `PRAGMA user_version`). Never write to a user's store from analysis code.
- Harness command lines are checked against the installed CLIs (`--help`), not documentation; record what you probed in the module docs.
