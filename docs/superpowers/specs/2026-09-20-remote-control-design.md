# Remote control: mirror and drive every session from another device

Status: Implemented on 2026-09-20.
Date: 2026-09-20
Baseline: `81bcf6c` on `master`.

## 1. Problem and outcome

agent-mux runs AI coding harnesses as PTYs inside one TUI. Everything it
knows how to show — a session working, a session waiting for an answer, a
loop escalating — is visible only at that terminal. Stepping away from the
desk means losing the thread of work that is still running, and a harness
that stops to ask a question waits until someone comes back.

Outcome: the running TUI serves a web app. Any device that can reach the
port sees **every session mirrored live**, can **type into any of them**,
**switch** between them, and **start** new sessions, skills, loop runs and
workflow runs, or **kill** a session. Off by default; a per-run bearer
token; loopback unless told otherwise.

Out of scope: TLS (section 7 says what to use instead), a second desktop
UI, editing files, and anything that changes the desktop's own view — a
remote client never scrolls, resizes or attaches the desktop's pane.

## 2. Vocabulary

| Term | Meaning |
| --- | --- |
| Client | One browser tab, one WebSocket, one `ClientId`. |
| Focus | The session a client is watching. Per client, independent of the desktop's selection. |
| Resync | The whole screen as escape sequences (`vt100::Screen::state_formatted`), including the input modes. The client resets its emulator and writes it. |
| Hub | `RemoteHub`: the fan-out the server tasks hold, one bounded queue per client. |
| Token | The bearer secret for a run. In memory unless `[remote] token` pins it. |
| Hole | A gap in a client's byte stream after its queue overflowed. Healed only by a resync, never by resuming output. |

## 3. Design decision and alternatives

Chosen: **a hand-rolled HTTP/WebSocket server on `tokio::net`, bridged to
the `App` through the existing `AppEvent` channel, serving a vendored
xterm.js page.**

| Approach | Benefit | Limitation |
| --- | --- | --- |
| Hand-rolled on tokio net (chosen) | No new crates at all — `httparse`, `base64` and `sha1_smol` were already transitive; the protocol surface is three routes and one frame codec, the size the crate already hand-writes for MCP's JSON-RPC; nothing to audit but our own code | We own RFC 6455 correctness, which is why `ws.rs` carries a conformance test per rule |
| `axum` + `tokio-tungstenite` | Standard, less code to own | ~40 new crates (hyper, tower, http-body…) in a binary that has stayed at 18 direct dependencies |
| A `ttyd` sidecar process | No code at all | Mirrors one PTY, not agent-mux's session model; no session list, no launching, a second auth story, a second process to supervise |
| Reuse the MCP server over TCP | One server | MCP is read-only analysis over the trace store, request/response, no push; the remote needs a live byte stream |

The load-bearing constraint is the second one below, and it is what makes
the rest simple.

## 4. Two rules

**1. A server task never touches `App`.** `App` is one owned value on the
main loop's stack (`src/main.rs`), holding `Vec<Session>` with non-`Sync`
PTY handles. Making it shareable would put a lock on the terminal hot path
for a feature that is off by default. Instead commands enter as
`AppEvent::Remote(RemoteCommand)` on the channel every other actor already
uses — the input thread, the tick timer, the trace runtime — and are applied
on the main loop like a keypress. Frames leave through the hub. The remote
adds no locking to any existing path, and `App::handle_event` (moved out of
`main.rs`) means the TUI, the three headless drivers and the integration
test all dispatch identically.

**2. The remote never resizes a PTY.** All sessions share `App.pane_size`,
owned by the desktop. A phone that resized would reflow the desktop's panes
under the user. So there is no resize message in the protocol at all: the
page creates its terminal at the server's cols × rows and scales the
element with CSS. That also removes the whole class of "who owns the
geometry" races.

## 5. The bridge and backpressure

Output is the only thing pushed synchronously, from `handle_pty_output`
*after* `process_output` — so a `key` command arriving next reads the same
DECCKM state the client has been shown. Exits publish immediately. The
session list is published from `on_tick` when a cheap fingerprint
(`[(id, status)]`, selection, pane size, attached) changes, which catches
launches, kills, `X` and respawns without instrumenting each call site.

Each client has a 256-frame queue (~1 MiB, since PTY chunks are ≤ 4 KiB).
`push_output` uses `try_send`. When it fills:

- the client is marked `needs_resync` and receives **no further output**,
  because a terminal stream with a hole renders worse than a blank screen;
- once its queue drains, `take_resync_requests` reports it and the App
  sends one `state_formatted()` snapshot, which the client applies after a
  `reset()`;
- control frames (`sessions`, `exit`, `result`) are idempotent snapshots,
  so a dropped one costs at most a second of staleness.

## 6. Protocol

One WebSocket per client. Text frames are JSON tagged with `t`; binary
frames are `[u8 kind][u32 BE session][payload]` with `0x01` output, `0x02`
resync, `0x10` input.

Client: `hello`, `select`, `resync`, `key`, `paste`, `catalog`, `launch`,
`kill`, `launch_skill`, `start_loop`, `start_workflow`, `ping`, plus binary
input. `paste` carries clipboard text the page read itself, because a phone
often cannot aim a normal paste at xterm's hidden textarea; the server
wraps it, since it is the side that knows whether bracketed paste is on.
`resync` is what a tab asks for when it returns from being throttled in the
background with its socket still open. Server: `hello`, `sessions`, `catalog`, `exit`, `result`, `notice`,
`pong`, plus binary output and resync. `protocol::CLIENT_FRAMES` and
`SERVER_FRAMES` are checked against `web/app.js` by a unit test, which is
the only thing keeping a JS string and a Rust enum in step.

Session ids on the wire are always the stable `Session.id`, never a vector
index — a kill arriving a tick late must not hit whatever slid into that
slot.

**Key encoding.** Typing travels as the raw bytes xterm.js produces: it
tracks DECCKM from the same stream vt100 does (the resync carries
`input_mode_formatted`, and later changes ride the output), so its bytes are
exactly what a local terminal would send. Helper buttons have no keypress
to encode, so they send a named key and the server runs it through
`keys::encode_key_with_mode` with `application_cursor()` read off vt100.
One key table, no second implementation to drift.

## 7. Security

- **Token**: 32 random bytes (two v4 UUIDs) as base64url, compared in
  constant time (`ct_eq`), `Debug` prints `Token(****)`, never logged,
  never in an `AppEvent`. A configured token under 16 characters is dropped
  with a warning rather than honoured.
- **Cookie, not query**: `GET /?token=…` sets `amx_token` as
  `HttpOnly; SameSite=Strict`, and the page strips the query from its URL.
  The token never reaches page script, browser history or a shared link.
- **Origin check** on the upgrade; CSP, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer` on the page.
- **Assets from a fixed table**, never a filesystem path join.
- **Gates in the App**: `allow_kill` and `allow_launch` are enforced in
  `handle_remote`, not only in the page and not only at the parse boundary,
  so a hand-written client gains nothing.
- **Limits**: 8 clients, 1 MiB per message, a 64 KiB/s token bucket on
  typed input, 45 s idle timeout, 5 s to send a request head, 8 KiB of
  headers.
- **No TLS**, stated plainly in the guide, with Tailscale and SSH tunnel
  recipes. An off-loopback bind warns in the status bar and in the page.
- **PTY bytes are not masked.** `mask_secrets` guards stored trace content
  and never applied to the live stream. Whatever is on the screen reaches
  the browser. The guide says so in its own section rather than in a
  footnote.

## 8. Tests

`cargo test` covers the whole feature without a browser:

- `ws.rs`: the RFC 6455 §1.3 accept vector, every length class, masked
  round trip, unmasked rejected 1002, fragmented reassembly, oversized
  control frame, bad UTF-8 1007, partial buffers.
- `http.rs`, `token.rs`, `protocol.rs`, `bridge.rs`, `config.rs`,
  `mod.rs`: parsing, constant-time compare, message round-trips, the
  backpressure contract (fill → withheld → resync → resumed), settings
  precedence, the CLI flag.
- `web.rs`: every file under `web/` is served and every entry exists, the
  page references only assets that resolve, no CDN and no inline script,
  content types match, the vendored bundles are the ones recorded in
  `VERSIONS`, and `app.js` speaks exactly the frames Rust knows.
- `tests/remote_control.rs`: a headless `App` plus the real server on an
  ephemeral port, driven by a hand-written WebSocket client sharing no code
  with `ws.rs` — 401/cookie/asset routing, the greeting, a `sh -c cat`
  session mirroring and echoing typed input, a named `enter` key reaching
  the session, `allow_kill = false` refused with the session still alive,
  kill working when allowed, launch creating a session and reporting an
  unknown profile, bad token 401, foreign origin 403, ninth client 503, a
  2 MiB frame closing 1009, and shutdown closing 1001.

The browser half is a manual checklist in the guide; §7 of that document
says what to walk through and why.

## 9. What was deliberately left out

- **Scrollback on switch.** `state_formatted()` is the visible grid; a
  switch redraws from it and history fills in as new output arrives. A
  `history` frame is the obvious next addition.
- **Touch selection under scaling.** xterm hit-tests post-transform and
  measures cells pre-transform, so selection is off by the scale factor.
  Documented ("pinch to zoom before selecting") rather than fixed, which
  would mean forking xterm.
- **`addon-fit`**, which resizes to fit its container — exactly what rule 2
  forbids. Not vendored, so it cannot be reached for by accident.
