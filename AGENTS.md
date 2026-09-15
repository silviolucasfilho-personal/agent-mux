# AGENTS.md — repository guide for coding agents

`agent-mux` is a terminal multiplexer for AI coding harnesses (Claude Code `claude`, Codex CLI `codex`, Google Antigravity `agy`) with a local SQLite trace store (`~/.agent-mux/traces.db`) and a read-only analysis CLI (`agent-mux trace …`).

## Skills

The only packages agent-mux manages are **skills**: a harness-neutral `SKILL.md` plus optional `skill.toml` and `reference/*.md`, installed into each harness's own skill directory and launched from the Agents sidebar; the Skills view (`S`) is a read-only look at skills and their usage. Details in `docs/skills.md`; code in `src/skill/`.

### Heimdall (`skills/heimdall`)

Heimdall briefs the user on active and recent sessions and evaluates skills and agents (subagents) in depth. Rust computes its facts: a briefing snapshot is written before launch (`$AGENT_MUX_BRIEFING`) and the read-only MCP server (`agent-mux mcp serve`) answers follow-ups where the harness allows it, with the `agent-mux trace` CLI as fallback. Its `SKILL.md` holds the tool/command table and playbooks; `reference/sessions.md`, `reference/skills.md` and `reference/agents.md` hold thresholds and report shapes; ad-hoc SQL is in `docs/trace-sql-examples.md`. It is compiled into the binary; a package at `~/.agent-mux/skills/heimdall/` shadows it.

Invocation per harness: `/heimdall` (Claude Code, Antigravity), `$heimdall` (Codex). Install with `agent-mux skill install heimdall`; the Agents sidebar launcher installs it automatically before each launch.

## Working in this repository

- `cargo build`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt`.
- Trace store schema: `src/tracing/store/schema.rs` (migrations through `PRAGMA user_version`). Never write to a user's store from analysis code.
- Harness command lines are checked against the installed CLIs (`--help`), not documentation; record what you probed in the module docs.
