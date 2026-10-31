# Shared memory (ai-memory)

agent-mux does not keep a memory itself. It installs and wires
[ai-memory](https://github.com/akitaonrails/ai-memory), so Claude Code, Codex,
Antigravity and OpenCode share one memory per project.

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
agent-mux memory install [--yes] [--harness claude|codex|agy|opencode]...
agent-mux memory uninstall [--yes] [--purge-data]
agent-mux memory backfill [--yes] [--force] [--max-sessions N] [<folder>...]
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

## agent-mux feeds ai-memory

agent-mux already records every session it launches in its trace store. For
those sessions agent-mux sends ai-memory the events from that store, so
ai-memory's hooks don't record them a second time. Prompts and tool output
arrive masked and truncated the way the trace store keeps them.

**What it changes**

- **At launch.** A session gets `AI_MEMORY_CAPTURE_OWNER=agent-mux`.
  ai-memory's hooks in that session then stop recording. They still hand
  the session its handoff, and the MCP server still answers. A file under
  `~/.agent-mux/ai-memory-feed/launches/` records that agent-mux owns the
  session's capture.
- **The relay.** A thread of the TUI reads the trace store's hook events past
  a cursor (`~/.agent-mux/ai-memory-feed/cursor`). It sends the owned
  sessions' events to ai-memory's `/hook/batch`, in order, in the shape
  ai-memory's own hooks use:
  - the harness's name and session id;
  - `extension=agent-mux`;
  - an `ingest_key` derived from the stored row, so a retry is recognised
    instead of stored twice.

  Before an event goes, ai-memory's own check (`ai-memory hook
  --check-capture`) applies the repository's capture policy and gives the
  project routing its hooks would use. The cursor moves past what the server
  acknowledged. If the server is down, events wait in the trace store until
  it's back.

**Which sessions**

| Session | Who records it |
|---|---|
| Claude Code, with agent-mux's per-launch hooks at the `standard` or `strict` profile and `full` content mode | agent-mux |
| Codex, when agent-mux's Codex trace hooks are installed | agent-mux |
| Antigravity | ai-memory's own hooks (agent-mux records only an Antigravity session's first model call) |
| OpenCode | ai-memory's own plugin (agent-mux does not trace OpenCode sessions yet) |
| Loop and workflow runs | Nobody: agent-mux owns their capture and sends nothing, so headless runs stay out of the memory |
| Sessions started outside agent-mux | ai-memory's own hooks |

The assistant's last message is not sent. ai-memory keeps it only when you
turn on its `capture_assistant` option.

When the relay first starts it begins at the newest stored event, so earlier
history is not sent. `AGENT_MUX_MEMORY_FEED=off` turns the feed off, and
ai-memory's hooks record every session again. The Memory screen and
`agent-mux memory status` show a `feed` row.

## Importing older sessions

ai-memory's hooks capture only the sessions that start after they are
installed. `agent-mux memory backfill` imports the ones from before.

```
agent-mux memory backfill
```

1. agent-mux lists every folder its trace store has seen a session in and
   that still exists. Loop worktrees and agent-mux's own folders are left
   out. To import only some folders, name them.
2. For each folder it asks ai-memory what an import would bring
   (`ai-memory backfill --dry-run`). It prints the result one project at a
   time, then asks before importing anything.
3. Each folder is imported with ai-memory's own `ai-memory backfill`. It
   reads the Claude Code, Codex and Antigravity transcripts recorded in
   that exact folder, keeps their original times, and sends them through
   the same path as live capture. The sessions then become searchable
   pages.

It stays safe to run again:

- **A project that already has captured sessions is left alone.** With
  `--force` it is imported anyway. ai-memory does not notice that a session
  was already captured, so those sessions are stored twice.
- **A folder is imported only once.** Every import stores every session
  again, so agent-mux lists the folders it has imported in
  `~/.agent-mux/ai-memory-feed/backfilled` and skips them from then on,
  even with `--force`. To import a folder again, delete its line.

The import covers every transcript in a folder, including headless runs
such as workflows, which the live feed leaves out. `--max-sessions N` keeps
only the newest N of each folder. ai-memory reads at most 200 sessions per
harness and 50,000 events per run.

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
| OpenCode | `~/.config/opencode/opencode.json` (`mcp.servers.ai-memory`) | `~/.config/opencode/plugins/ai-memory-opencode2.ts`, a plugin: OpenCode has no hooks (ai-memory's agent name is `open-code`; it needs `opencode` on `PATH` to pick the plugin shape) |

`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `OPENCODE_CONFIG_DIR` move the Claude, Codex and OpenCode files.

## After installing

- **Codex** shows "Hooks need review" at its next start. Choose
  "Trust all and continue".
- **Antigravity** has no session-end event. To summarise its last session,
  run `ai-memory finalize-session --agent antigravity-cli`.
- **OpenCode** loads plugins at startup: restart it after the plugin is written.
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
| `src/memory/feed.rs` | Taking over a launch's capture and the relay to `/hook/batch` |
| `src/memory/backfill.rs` | Importing older sessions with `ai-memory backfill`, folder by folder |
| `src/memory/query.rs` | Search, reading a page, and the question's opening message |
| `src/app/memory_view.rs` | The Memory screen; its steps run on a thread |
| `src/ui/memory.rs` | Drawing the Memory screen |

The facts this guide relies on (asset names, flags, file locations, the
launchd agent) were read from ai-memory v2.6.7's source and checked with its
real binary in an isolated home.

`agent-mux memory hook …`, the hook an older agent-mux installed, does nothing
and exits successfully.
