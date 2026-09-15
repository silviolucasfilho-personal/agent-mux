# Agent Facts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give built-in agents their facts from Rust: a briefing snapshot at launch, a read-only stdio MCP server over `TraceService`, per-launch registration for Claude and Codex, an installed entry for Antigravity, and the CLI as the fallback.

**Architecture:** `src/mcp/{mod,server,register,install}.rs` wraps the existing `TraceService` in a five-method JSON-RPC stdio loop and produces the harness registration fragments; `src/skill/launch.rs` grows hydration and MCP wiring driven by `skill.toml [agent]`; `LaunchPlan` carries the MCP fragment the way it carries hook registrations today.

**Tech Stack:** Rust 2024, serde_json, schemars (already present), Tokio (already present). No MCP SDK in v1.

**Spec:** [Built-in agents: Rust facts, Markdown agents, MCP on demand](../specs/2026-09-15-agent-facts-design.md)

## Global Constraints

- All tools are read-only; the server never creates or migrates a store; no raw SQL, shell or write tool is exposed.
- Stdout of `mcp serve` carries JSON-RPC lines only.
- A failed hydration or registration never blocks a launch; it produces one status notice and the CLI fallback remains.
- Harness flags are the ones probed on 2026-09-15 (`claude --mcp-config`, `codex -c mcp_servers.<name>.…`, `agy mcp add/remove`); re-probe before changing them and record the probe in the module docs.
- Tests use temporary stores, temporary homes and fake harness scripts; the real `agy` is only invoked under `AGENT_MUX_LIVE=1`.

---

## Execution map

Baseline: `25752d2` on `docs/onboarding-readme`.

| Task | Files | Verify |
| --- | --- | --- |
| 1 Package contract | `src/skill/mod.rs`, `src/skill/cli.rs`, `skills/heimdall/skill.toml` | `tests/skill_package.rs` |
| 2 MCP server | `src/mcp/{mod,server}.rs`, `src/main.rs`, `src/lib.rs` | `tests/mcp_protocol.rs` |
| 3 Registration | `src/mcp/{register,install}.rs`, `src/tracing/mod.rs` (`LaunchPlan.mcp`), `src/tracing/cli.rs` (`doctor`), `src/main.rs` (`mcp install|uninstall|status`) | `tests/mcp_register.rs` |
| 4 Hydration | `src/skill/launch.rs`, `src/app.rs` (`launch_skill`, exit cleanup, startup sweep), `src/config.rs` (`[agents]`) | `tests/skill_hydrate.rs` |
| 5 Heimdall and docs | `skills/heimdall/*`, `README.md`, `docs/skills.md`, `AGENTS.md` | `cargo test`, review |

### Task 1: Package contract

- [x] `SkillMeta` gains `agent: Option<AgentMeta { hydrate: Vec<String>, mcp: Option<String> }>` with `deny_unknown_fields`; `SkillDefinition` gains `hydrate: Vec<Hydration>` (enum, v1 `Briefing`) and `mcp: McpMode { Auto, Off }`.
- [x] Validation: unknown `hydrate` names are a parse error; `hydrate` or `mcp = "auto"` without `trace.read` is a diagnostic reported by `skill list` and the package still loads with both disabled.
- [x] `skills/heimdall/skill.toml` declares `[agent] hydrate = ["briefing"]`, `mcp = "auto"`.
- [x] Tests: parse, defaults by capability, diagnostics, `skill show` unchanged.

### Task 2: MCP server

- [x] `src/mcp/server.rs`: `serve_stdio(config: ServiceConfig) -> anyhow::Result<()>`: line-delimited JSON-RPC reader on stdin, writer on stdout, methods `initialize` (protocol version `2025-11-25`, server info `agent-mux/<version>`, `capabilities.tools`), `notifications/initialized`, `ping`, `tools/list`, `tools/call`; error codes `-32700`, `-32600`, `-32601`, `-32602`; tool errors as `isError` results with the `ServiceError` code and message.
- [x] `tools/list` catalog: name, description, `inputSchema` from `schemars::schema_for!` on each argument struct, annotations `readOnlyHint`, `destructiveHint: false`, `openWorldHint: false`.
- [x] `tools/call`: `Request::from_tool_call`, `TraceService::execute` on a blocking thread, envelope as `structuredContent` and as JSON text.
- [x] `agent-mux mcp serve --stdio [--db PATH] [--workspace DIR | --all-workspaces]` dispatched in `main` before terminal setup; database precedence per spec section 6; `--workspace-from-env` reads `AGENT_MUX_WORKSPACE`.
- [x] Tests (`tests/mcp_protocol.rs`): spawn the built binary with piped stdio against a temporary store; initialize, version rejection, tools/list shape, each tool round trip equal to an in-process `TraceService::execute`, invalid arguments, unknown tool, malformed JSON, EOF; assert stdout contains only JSON lines.

### Task 3: Registration

- [x] `src/mcp/register.rs`: `server_argv(exe, db, workspace) -> Vec<String>`; `claude_mcp_config_json(argv) -> String` (`{"mcpServers":{"agent-mux":{"type":"stdio","command":…,"args":[…]}}}`); `codex_mcp_overrides(argv) -> Vec<String>` (`-c 'mcp_servers.agent-mux.command="…"'`, `-c 'mcp_servers.agent-mux.args=[…]'`, TOML-escaped through the existing `toml_string`).
- [x] `App::prepare_agent_launch` (not the tracing planner) appends the Claude or Codex arguments when the launch is a skill launch with `mcp = auto` and the global `[agents] mcp` is `auto`; the child environment gains `AGENT_MUX_MCP=registered|installed|unavailable` and `AGENT_MUX_WORKSPACE`.
- [x] `src/mcp/install.rs`: `install_agy(home, exe)` runs `agy mcp add agent-mux <exe> -- mcp serve --stdio --workspace-from-env`, `uninstall_agy` runs `agy mcp remove agent-mux`, `agy_status(home, exe)` reads `~/.gemini/config/mcp_config.json`; the `agy` executable is resolved from `PATH` or `AGENT_MUX_AGY_BIN` (tests point it at a script that records its argv).
- [x] `agent-mux mcp install|uninstall|status [claude|codex|agy]` in `main`; `trace doctor` gains an `mcp` section that runs `initialize` and `agent_mux_get_health` against the binary.
- [x] Tests: fragment shapes, the absolute-binary and store requirements and plan gating are unit tests in `src/mcp/register.rs`; the agy file shape in `src/mcp/install.rs`; the end-to-end `--mcp-config` argument, `AGENT_MUX_MCP` and `AGENT_MUX_WORKSPACE` values in `tests/skill_hydrate.rs`. Invoking the real `agy` is left to the live matrix script.

### Task 4: Hydration

- [x] `config.rs`: `[agents] hydrate = true`, `mcp = "auto"` resolved into `ResolvedTracing` (or a sibling `ResolvedAgents`).
- [x] `src/skill/launch.rs`: `hydrate(def, db, cwd, runtime_dir, launch_id) -> Option<HydrationResult { path, as_of }>`; runs `Request::Briefing` on a `TraceService` scoped to `cwd`, writes `<runtime>/briefings/<launch_id>.json` atomically with owner-only permissions, writes the typed error envelope on failure; `build_skill_launch_with_db` appends the one-sentence pointer to the prompt and sets `AGENT_MUX_BRIEFING` and `AGENT_MUX_BRIEFING_AS_OF`.
- [x] `App::launch_skill` calls hydration between install and spawn on a blocking thread with the service deadline; a failure becomes one notice. `handle_pty_exit` removes the launch's snapshot; `App::new` sweeps files older than 24 hours.
- [x] Tests (`tests/skill_hydrate.rs`): with a fake `claude` and a temporary home and runtime dir, the picker launch writes the snapshot, sets the variables, appends the sentence; missing store yields an error envelope; exit removes the file; a package without `trace.read` writes nothing; the sweep removes an old file and keeps a fresh one.

### Task 5: Heimdall and documentation

- [x] `skills/heimdall/SKILL.md`: playbooks start from `$AGENT_MUX_BRIEFING`, prefer the MCP tools, keep the CLI equivalents for `AGENT_MUX_MCP=unavailable`; `reference/sql.md` moved to `docs/trace-sql-examples.md` with a table naming the tool or command that answers each question.
- [x] README sections 2, 3, 4.1, 6, 9, 10.1, 11, 14; `docs/skills.md` sections 1 and 4; `AGENTS.md`; spec status line.
- [ ] `scripts/verify-trace-matrix.sh`: optional Heimdall launch per harness asserting one `tools/call` under `AGENT_MUX_MCP_DEBUG` (not done; needs authenticated CLIs).
