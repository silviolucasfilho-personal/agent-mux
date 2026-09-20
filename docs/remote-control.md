# Remote control

The running TUI can serve a small web app so a phone, a tablet or another
computer can watch every session live, type into any of them, and start new
work. It is off by default.

Design: [`docs/superpowers/specs/2026-09-20-remote-control-design.md`](superpowers/specs/2026-09-20-remote-control-design.md).
Code: `src/remote/` (server), `src/app/remote.rs` (the App side), `web/` (the page).

---

## 1. Turning it on

Either put it in `profiles.toml`:

```toml
[remote]
enabled = true
# listen = "127.0.0.1:7681"   # default; use "0.0.0.0:7681" to reach it from the LAN
# token = "…"                 # a fixed token; omit for a fresh one each run
# allow_kill = true           # remote may kill sessions
# allow_launch = true         # remote may start sessions, skills, loops, workflows
```

or pass the flag:

```sh
agent-mux --remote                  # loopback, port 7681
agent-mux --remote 0.0.0.0:7681     # every interface
agent-mux --remote 9000             # shorthand for 127.0.0.1:9000
```

or set the environment:

```sh
AGENT_MUX_REMOTE=1 agent-mux
```

On startup the status bar prints the URL, token included:

```
remote: http://127.0.0.1:7681/?token=k3Jq…  (v shows it again)
```

Press `v` to see it again — the About overlay carries it under **Remote
control**, with the bind address, the number of connected clients, and what
the permissions are set to. **`y` there copies the URL to the clipboard**,
token and all, which is the practical way to get it onto a phone: paste it
into a message to yourself rather than retyping 43 random characters. The
status bar confirms the copy and reminds you the token is in what you just
copied.

If the port is taken, the TUI says so in one line and keeps running without
the remote.

## 2. Reaching it from another device

The default bind is loopback, so only the machine itself can connect. Two
safe ways to go further:

**Tailscale** (easiest, encrypted, no port forwarding):

```sh
agent-mux --remote          # stays on loopback
tailscale serve --bg 7681   # publishes it over your tailnet with HTTPS
```

**SSH tunnel** (nothing new installed):

```sh
ssh -L 7681:127.0.0.1:7681 you@desktop
# then open http://127.0.0.1:7681/?token=… on the laptop
```

Binding to `0.0.0.0` works too, on a network you trust. agent-mux colours
that banner as a warning, and the page shows its own, because **there is no
TLS**: the token and every keystroke cross the network in the clear.

## 3. Using it from a phone

Open the URL. The token moves from the address bar into an `HttpOnly`
cookie on the first load, so it is not in your history or in anything you
might share; the page itself never sees it. If you land on the token form,
paste the token from the desktop.

- **Chips** across the top are the sessions, in the desktop's order. A dot
  shows state (green working, yellow waiting for you, red exited), the
  filled chip is the one you are watching, and a thin underline marks the
  one the desktop has selected. A dot after the name means output arrived
  while you were elsewhere.
- **The badge** counts sessions waiting for you, matching the yellow rows
  in the desktop's sidebar.
- **Tap a chip** to switch. The screen redraws from a fresh snapshot.
- **⌨** opens the keyboard; tapping the terminal does too.
- **The key bar** sends what a phone keyboard cannot: `Esc`, `Tab`, `^C`,
  arrows, `Enter`. `…` opens a second row with sticky `Ctrl`/`Alt` (tap
  once, then a letter), `^D`, `^L`, `^Z`, `Paste`, `Home`/`End`,
  `PgUp`/`PgDn`. Holding an arrow repeats it. **Paste** reads the device
  clipboard and sends the text to be wrapped server-side, because a phone
  frequently cannot aim a normal paste at the terminal; the browser asks
  for clipboard permission the first time.
- **+** opens a sheet with four tabs: a new session (profile + directory), a
  skill (with its own directory, defaulting to the desktop's), a loop run,
  or a workflow run.
- **Long-press a chip** for its actions, including **Kill**, which asks a
  second time before it does anything.

Two things to know:

- **The remote cannot resize anything.** Every session shares one pane size
  owned by the desktop TUI, so the page renders that geometry and scales it
  to fit your screen. Pinch to zoom in; that is also the way to select text
  accurately, because the scaling throws off touch selection.
- **There is no scrollback right after switching.** A switch redraws from
  the visible screen only. Scrollback fills in again as new output arrives.
  A tab that was throttled in the background asks for the screen again when
  you return to it, so a phone that slept does not show a half-drawn frame.

`Ctrl+Q` is deliberately absent from the key bar: on the desktop it is
agent-mux's own detach key, not something the harness ever sees, so it
would do nothing useful here.

## 4. What it costs you

Be clear-eyed about what turning this on means.

- **The token is the whole fence.** Anyone who has it can type into every
  session, and those sessions are AI coding harnesses with your repository
  and, usually, your approvals bypassed. Treat the URL like an SSH key.
- **Terminal output is mirrored verbatim.** agent-mux masks secrets in what
  it *stores* (`mask_secrets`, `src/tracing/map.rs`), but that never touched
  the live PTY stream and does not here either. Whatever is on the screen —
  an API key a tool printed, a `.env` a session catted — goes to the
  browser as it is.
- **No TLS.** See section 2.
- **`allow_kill = false` / `allow_launch = false`** narrow the remote to a
  read-and-type surface. They are enforced in the App, not just hidden in
  the page, so a hand-written client cannot get past them.
- A per-run token exists only in memory. Restarting agent-mux invalidates
  every old link, which is usually what you want; set `token` if you would
  rather bookmark one URL.

Limits, if you want the exact numbers: 8 clients, 1 MiB per message, 64 KiB/s
of typed input per client (128 KiB burst for a paste), a 45-second idle
timeout, and a 15-second ping.

## 5. Troubleshooting

| What you see | What it means |
| --- | --- |
| `401` and a token form | The token is wrong or the cookie expired. Re-open the URL from the desktop (`v`). |
| `403 origin mismatch` | The page was served from a different host than the socket is connecting to. Use the URL as printed. |
| `503 too many remote clients` | Eight clients are already connected; close an old tab. |
| The page says *Reconnecting…* | The desktop quit, the network dropped, or the laptop slept. It retries with backoff and redraws on its own. |
| `remote: cannot listen on …` | The port is in use. Pick another with `--remote 9001`. |
| Typing does nothing | The session may have exited — its chip turns red. The banner shows the write error. |
| Columns look shifted | A font without the box-drawing glyphs. The page asks for Menlo / DejaVu Sans Mono / Noto Sans Mono first. |
| The page looks like an older version | It should not: assets revalidate with an ETag, so restarting agent-mux and reloading the tab is enough. If it persists, the tab is holding an old WebSocket — close and reopen it. |

## 6. Configuration reference

| Key | Env | Default | Meaning |
| --- | --- | --- | --- |
| `[remote] enabled` | `AGENT_MUX_REMOTE` (`1`/`true`/`yes`) | `false` | Serve the remote at all. |
| `[remote] listen` | `AGENT_MUX_REMOTE_LISTEN` | `127.0.0.1:7681` | Bind address. |
| `[remote] token` | `AGENT_MUX_REMOTE_TOKEN` | generated | Fixed token; must be at least 16 characters or it is ignored. |
| `[remote] allow_kill` | — | `true` | Remote may kill sessions. |
| `[remote] allow_launch` | — | `true` | Remote may start sessions, skills, loops and workflows. |

Configuration wins over the environment, as everywhere else in agent-mux;
`--remote` wins over both.

## 7. Checking it by hand

`cargo test --test remote_control` covers the protocol end to end without a
browser. What a browser adds, and what to walk through after a change to
`web/`:

**iOS Safari, portrait and landscape.** The token leaves the address bar
within a second. The insecure banner appears on a LAN address and not on
`localhost`. Chips scroll; the desktop's selection is underlined. The
terminal fits the width; pinch-zoom works; scrolling the terminal does not
scroll the page. `⌨` opens the keyboard and the key bar stays above it,
including after a rotation. Arrows move through a Claude Code menu (that is
DECCKM working), `Esc` backs out, `^C` interrupts, sticky `Ctrl` then `c`
interrupts too. A paste arrives as one bracketed paste. Lock the phone for
two minutes: it reconnects and redraws with no leftover garbage. Kill asks
twice. With `allow_kill = false`, the menu offers no Kill.

**Android Chrome.** The same, plus: the back button closes the sheet rather
than leaving the page, and the bell vibrates.

**A desktop browser.** 1:1 at a large window, scaled down at a small one,
never upscaled. A hardware keyboard reaches the session directly. Text
selection lands on the right columns (that is the Unicode 11 addon).

In all of them, the desktop's own pane size must never change while a phone
is connected. There is no resize message in the protocol at all, so this is
true by construction — but it is the property worth re-checking if the
geometry code is ever touched.
