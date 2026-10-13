# Shared memory (ai-memory)

agent-mux does not keep a memory itself. It installs and wires
[ai-memory](https://github.com/akitaonrails/ai-memory), so Claude Code, Codex
and Antigravity share one memory per project.

ai-memory works in three parts:

- **A local server**: `ai-memory serve` on `127.0.0.1:49374`. It keeps a
  Markdown wiki and a SQLite index under
  `~/Library/Application Support/ai-memory`.
- **Hooks in each harness**: they capture every session, whether or not
  agent-mux launched it, and hand the next session what the last one did.
- **An MCP server**: each session can search and read the memory through it.

## Setting it up

Open the Memory screen with **F1 → "Open memory (ai-memory)"**. It has one row
for each part: the binary, the server, and each harness's MCP entry and hooks.

1. Press **`i`**. agent-mux lists the steps that are missing, each with the
   command it will run.
2. Press **`y`** to run them. Their output appears below the rows, and the
   rows are read again when the steps finish.

The same flow works from a shell:

```
agent-mux memory status [--json]
agent-mux memory install [--yes] [--harness claude|codex|agy]...
agent-mux memory uninstall [--yes] [--purge-data]
```

## Reading the memory and asking it

The Memory screen works on one project folder: the folder of the selected
session, else the default folder. The folder is shown under the rows.
ai-memory names the project after the folder's nearest `.ai-memory.toml`,
else its repository's remote, else the folder's name.

- **`/` searches** the project's memory with ai-memory's full-text search
  (`ai-memory search --json`). Pick a result with `↑`/`↓`. **`Enter`**
  fetches the page (`ai-memory read-page --json`) and opens it in the
  Markdown viewer, with its kind, tier and origin as front matter. `Esc`
  goes back.
- **`a` asks a question.** Type it, use `Tab` to choose among the wired
  harnesses whose CLI is installed, and press `Enter`.
  - A new session starts in the project folder on that harness's first
    profile.
  - Its first message asks it to search the memory with ai-memory's
    `memory_query` tool and to name the pages its answer comes from.
  - The conversation goes on there like any other session. The sidebar
    marks it "a memory question".
- **`w` opens ai-memory's web view** (`http://127.0.0.1:49374/web`) in the
  browser. It is read-only: projects, each project's page tree, rendered
  pages and search.

Inside any session you can also just ask: "have we discussed X?", "catch me
up", "where did we leave off?". The MCP server answers.

Typing in the search or question line is plain text: the screen's letter
keys do nothing there. `Ctrl+R` reads the state again.

## What install does

Each step runs only when it is needed:

1. **Download.** When no `ai-memory` is installed, agent-mux fetches the
   latest macOS release (`ai-memory-macos-<arch>.tar.gz` from GitHub),
   checks it against its `.sha256`, and unpacks it into
   `~/Applications/ai-memory` (the binary, its `hooks/` and `packaging/`).
   - agent-mux looks for an existing binary in this order: `$AI_MEMORY_BIN`,
     your `PATH`, `~/Applications/ai-memory`, then the menu bar app.
   - On other systems, install ai-memory by hand first.
2. **Init.** Runs `ai-memory init` when the data folder has no `config.toml`.
3. **Server.**
   - agent-mux writes ai-memory's own launchd agent,
     `~/Library/LaunchAgents/com.github.akitaonrails.ai-memory.plist`, which
     runs `ai-memory serve --transport http --enable-web` at login and
     restarts it if it stops. It then bootstraps the agent.
   - If that agent already exists, agent-mux restarts it instead.
   - It then waits up to 45 s for the port: a freshly downloaded binary
     starts slowly the first time on macOS.
   - Logs go to `~/Library/Logs/ai-memory/`.
4. **Wiring.** For each harness whose CLI is on `PATH`, or each one named
   with `--harness`, agent-mux adds what its files lack:
   - the MCP entry, with `ai-memory install-mcp --client <agent> --apply`;
   - the hooks, with `ai-memory install-hooks --agent <agent> --apply`.

   Both are ai-memory's own commands. Running them twice changes nothing,
   and they back up each file before changing it.

The wiring goes into these files:

| Harness | MCP entry | Hooks |
|---|---|---|
| Claude Code | `~/.claude.json` (`mcpServers.ai-memory`) | `~/.claude/settings.json` |
| Codex | `~/.codex/config.toml` (`[mcp_servers.ai-memory]`) | `~/.codex/hooks.json` |
| Antigravity | `~/.gemini/config/mcp_config.json` | `~/.gemini/config/hooks.json` (group `ai-memory`) |

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` move the Claude and Codex files.

## After installing

- **Codex** shows "Hooks need review" at its next start. Choose
  "Trust all and continue".
- **Antigravity** has no session-end event. To summarise its last session,
  run `ai-memory finalize-session --agent antigravity-cli`.
- **Running sessions** must be restarted to load the MCP server.
- **The web view** is at `http://127.0.0.1:49374/web`.

## Uninstall

`u` on the Memory screen, or `agent-mux memory uninstall`, does two things:

1. Runs `ai-memory uninstall --apply --yes`. This removes ai-memory's hooks,
   MCP entries, instructions and skills from every harness.
2. Stops the server and removes its launchd agent.

The memory itself stays unless you press `p` on the screen or pass
`--purge-data`. The binary always stays.

## Code

| Path | What it holds |
|---|---|
| `src/memory/mod.rs` | Reading the state: binary, server, launchd agent, each harness's files |
| `src/memory/install.rs` | The steps and how each one runs |
| `src/memory/cli.rs` | `agent-mux memory …` |
| `src/memory/query.rs` | Search, reading a page, and the question's opening message |
| `src/app/memory_view.rs` | The Memory screen; its steps run on a thread |
| `src/ui/memory.rs` | Drawing the Memory screen |

The facts this guide relies on (asset names, flags, file locations, the
launchd agent) were read from ai-memory v2.6.7's source and checked with its
real binary in an isolated home.

`agent-mux memory hook …`, the hook an older agent-mux installed, does nothing
and exits successfully.
