# Cursor CLI

agent-mux runs the Cursor CLI (`cursor-agent`) as one of its harnesses: sessions in the Agents list, history, tracing, skills, personas, flows, loops, swarms and memory. The harness is named `cursor` (in `--harness`, `[backends.cursor]`, a `skill.toml`, a flow document); its program is `cursor-agent`. The CLI's other name, `agent`, is not claimed, and `cursor` on `PATH` is the editor's launcher, which agent-mux never runs.

Everything here was probed on 2026-11-17 against Cursor CLI 2026.11.13 (`cursor-agent --help`, the help of its subcommands, and live runs in a temporary folder). `src/harness.rs` and the modules named below record the probes.

## Setting it up

```
curl https://cursor.com/install -fsS | bash     # links cursor-agent into ~/.local/bin
cursor-agent login
agent-mux trace hooks install cursor            # usage, tool timing, the guard (optional, recommended)
```

A profile is the command `cursor-agent`; the default configuration has one named `Cursor`. When `cursor-agent` is not on agent-mux's `PATH`, launches find it at `~/.local/bin/cursor-agent`.

The MCP entry is written for you before the first Cursor session (`agent-mux mcp install cursor` does it by hand).

## What agent-mux writes under `~/.cursor`

Cursor has no per-launch flag for hooks or MCP servers, so two user-level files carry agent-mux's entries. Both are merged: every other key in them is kept, and `uninstall` removes only agent-mux's.

| File | Written by | Holds |
| --- | --- | --- |
| `~/.cursor/mcp.json` | the first Cursor launch, or `agent-mux mcp install cursor` | `mcpServers.agent-mux`: this binary with `mcp serve --stdio --workspace-from-env` |
| `~/.cursor/hooks.json` | `agent-mux trace hooks install cursor` only | one handler per event: `sessionStart`, `beforeSubmitPrompt`, `preToolUse`, `postToolUse`, `postToolUseFailure`, `subagentStart`, `subagentStop`, `stop`, `sessionEnd` |
| `~/.cursor/skills/<name>/` | skill installs | the skill, with agent-mux's manifest |

The hook handlers record only sessions agent-mux started (they find the launch in `AGENT_MUX_SESSION_ID`, which a hook inherits). In any other Cursor session, the editor included, they print `{}` and store nothing. `~/.cursor/skills-cursor/` is Cursor's own and is never touched.

## Command line

| | Cursor CLI |
| --- | --- |
| Opening message | positional |
| Continue the last session | `--continue` |
| Resume one | `--resume <chat id>` (bare `--resume` is a picker) |
| Fork | none: `f` on a session hands the transcript over as a Markdown file |
| Model | `--model <id>` (ids from `cursor-agent models`) |
| Effort | none: it is part of the model id (`gpt-5.3-codex-high`) |
| Skip approvals | `--force` |
| One shot | `--print --output-format stream-json <prompt>` |
| Added by agent-mux | `--approve-mcps --trust` on every launch that has the MCP server (the server loads unprompted, and a worker in a new worktree does not stop at the trust prompt) |

## Tracing

Cursor keeps a chat in two places: the transcript `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`, and `~/.cursor/chats/<hash>/<id>/meta.json` with the folder it ran in. A launch is matched to the newest transcript whose chat ran in the launch's folder; `--resume <id>` names it outright.

The transcript has the prompts, the answers and the tool calls with their arguments. It has no timestamps, tool results, token counts or model. What you get therefore depends on the hooks:

| | Transcript only | With `trace hooks install cursor` |
| --- | --- | --- |
| Turns, prompts, answers, tool calls | yes | yes |
| Times | when agent-mux read the line | when the hook fired |
| Tokens per turn | no | yes, from the `stop` hook |
| Tool results and errors | no | in `hook_events` |
| Model | no | yes, unless it is Auto (Cursor reports `default`) |
| Cost | no | no: Cursor reports none, and Auto names no priced model |
| Budget and write guards, loop guard, bus leases | no | yes |
| Urgent bus messages | no | after the next tool call (Cursor's `preToolUse` cannot add context; `postToolUse` can) |

`agent-mux trace doctor` shows whether `cursor-agent` and its folders are found and whether the hooks are installed and current. `agent-mux trace import --discover --provider cursor` imports past chats. The store admits the provider since schema v29.

Not joined: a tool call in the transcript and its hook rows. The transcript has no tool ids, so tool rows keep the transcript's order and the hook's timing stays in `hook_events`.

## History

The History list shows Cursor chats (badge `U`) from the folders above, filtered by folder like the others, and `Enter` resumes one with `--resume <id>`.

## Skills

Skills install to `~/.cursor/skills/<name>/SKILL.md` and, at project level, `.cursor/skills/<name>/`. The invocation is `/<name>`, also as the opening message. Both locations answered a probe in print mode.

## Personas and agents

Cursor has no flag for instructions, and its CLI reads custom agents only as sub-agents from a project's `.cursor/agents/`. A persona's instructions therefore open the session's first message. An agent without `edit` runs with `--mode ask` in place of `--force`: the probe read a file and was refused a write and a `touch`. The rest of a tool list is advisory, and `workflow check` says so.

## Flows

A step runs as `cursor-agent --print --output-format stream-json --trust --force <prompt>`. The answer is read from the `assistant` messages of the stream (its final `result` event glues every message together with no separator), the tokens from the `result` event. There is no cost, budget flag or timeout flag: the flow's own session timeout applies. Live shows the messages, each tool call and its output.

The path guard for a writing step works when the hooks are installed.

## Loops

Cursor runs loops when its hooks are installed and current; without them a loop stays at level L1, as on any harness with no guard. The installed handler cannot carry `--loop`, so a loop run is recognised by `AGENT_MUX_LOOP_ID` in the environment the hook inherits, and then fails closed for write tools. The scaffolder installs the loop skills under `.cursor/skills/` and the loop agents under `.cursor/agents/<name>.md`.

## Swarms

Cursor is a coordinator and worker harness. Workers start with the whole task as the opening message, reach the bus through the MCP server, and are woken by a paste into their terminal like the others. File leases are enforced only with the hooks installed.

## Memory

ai-memory 2.6.7 wires Cursor itself (`install-mcp --client cursor`, `install-hooks --agent cursor`, the same two files). The Memory screen and `agent-mux memory install` include it. agent-mux does not relay Cursor sessions from its trace store: ai-memory's own hooks capture them.

## Not yet

- A known id for a new chat. `cursor-agent create-chat` prints one that `--resume` then uses, which would make the match deterministic; today a new chat is matched by folder and time.
- Tool results in the trace rows (they are in Cursor's `store.db`, partly as protobuf, and in the hook rows).
- An unattended factory item: it still requires Claude Code, Codex or Muse.
- MCP servers of a persona's tool list: `mcp:<server>` must be configured in `~/.cursor/mcp.json` or `.cursor/mcp.json`.
