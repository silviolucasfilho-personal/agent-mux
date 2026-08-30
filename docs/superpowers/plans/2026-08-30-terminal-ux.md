# agent-mux Terminal UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ghostty/iTerm2-grade terminal UX for the embedded pane: scrollable history with emulator-correct wheel routing, mouse forwarding to agents, click-drag selection with clipboard copy/paste, and incremental search over scrollback.

**Architecture:** Everything layers on the existing vt100/tui-term core. vt100 already stores 1000 scrollback lines per session and self-clamps `set_scrollback`; scroll state lives in the parser, with a cached `scrollback_len` on `Session` so the immutable render path can map grid-absolute coordinates. Selection and search are pure modules over the parser; their highlights are post-passes over the ratatui buffer after `PseudoTerminal` renders. Mouse events enter through the existing channel as a new `AppEvent::Mouse`.

**Tech Stack:** existing ratatui 0.30 / crossterm 0.29 / tui-term 0.3 / vt100 0.16 / portable-pty 0.9; new: `arboard` 3 (clipboard).

**Spec:** `docs/superpowers/specs/2026-08-30-terminal-ux-design.md` (v1 spec: `docs/superpowers/specs/2026-08-29-agent-mux-design.md`)

## Global Constraints

- Scrolling works in both Attached mode and the Control-mode preview of the selected session.
- Wheel routing rule (normative, in order): 1. Shift held → local scrollback (3 lines/tick); 2. child's `mouse_protocol_mode() != None` → encode + forward to PTY; 3. child in `alternate_screen()` → 3× arrow up/down; 4. otherwise local scrollback (3 lines/tick). Rules 2–3 apply only in Attached mode; Control preview always scrolls locally.
- While scrolled, new output must not move the view (content-anchored); indicator `[SCROLL ↑ N]` in the pane title; any forwarded key while scrolled snaps to live bottom first, then forwards.
- Copy-on-select (release copies to system clipboard), Ctrl+Shift+C explicit copy, Ctrl+Shift+V paste into the attached agent — wrapped in `ESC[200~ … ESC[201~` iff `screen.bracketed_paste()`, CR/LF normalized to CR when not bracketed.
- Search: Ctrl+Shift+F (Attached + Control), plain Ctrl+F additionally in Control only. Incremental, case-insensitive substring over screen + full scrollback; Enter → next, Shift+Enter → previous, Esc closes and snaps to live. While the bar is open, keys go to it, not the agent.
- Plain keys and plain Ctrl+letters that v1 forwards keep reaching the agent unchanged. Only `Ctrl+Shift+C/V/F`, `Shift`+navigation keys, and mouse events are intercepted in Attached mode.
- All v1 keybindings and behavior unchanged; clipboard failures → status-bar error, never a panic; mouse capture always released via the existing guard + panic hook.
- Windows 11 ConPTY primary target; all code cross-platform; integration tests spawn `cmd.exe`/`sh`, never real agents.
- Test evidence must be pristine: zero warnings, `cargo clippy --all-targets -- -D warnings` clean.
- Key-routing order in `App::handle_key` (established Task 3, extended 5/7): 1) clear one-shot error; 2) search bar open → search handling consumes the key; 3) UX chords (Shift+nav, Ctrl+Shift+C/V/F, Ctrl+F in Control) when mode is Control or Attached; 4) existing v1 `dispatch`.

## File Structure

- Create: `src/selection.rs` (pure selection model + grid-absolute row helpers), `src/search.rs` (search state machine), `src/mouse.rs` (wheel routing decision + mouse-event encoder), `tests/scroll_ux.rs` (integration: scrolling, snap, selection, search against real shells)
- Modify: `src/session.rs` (scroll helpers, cached `scrollback_len`, anchoring), `src/events.rs` (`Mouse` variant), `src/app.rs` (mouse handling, UX chords, selection/search state, clipboard), `src/ui.rs` (highlight post-passes, scroll indicator, search bar, pane-rect helper), `src/main.rs` (mouse capture + restore), `src/lib.rs` (new modules), `Cargo.toml` (arboard)

Notes for implementers on vt100 0.16.2 facts (verified against vendored source):
- `Screen::set_scrollback(rows)` self-clamps to the scrollback length (`grid.rs:198-200`), so callers never need to clamp.
- `Screen::scrollback()` returns the current offset (rows scrolled back; 0 = live).
- vt100 does NOT expose scrollback length; probe it in O(1): save offset, `set_scrollback(usize::MAX)`, read `scrollback()` (= length), restore offset.
- `Screen::cell(row, col)` returns the VISIBLE cell — it respects the scrollback offset. Reading an off-screen row means temporarily moving the offset and restoring it.
- `Screen::alternate_screen()`, `Screen::bracketed_paste()`, `Screen::application_cursor()`, `Screen::mouse_protocol_mode()` (`MouseProtocolMode::{None, Press, PressRelease, ButtonMotion, AnyMotion}`), `Screen::mouse_protocol_encoding()` (`MouseProtocolEncoding::{Default, Utf8, Sgr}`) all exist.

---

### Task 1: Session scroll state + content anchoring (`session.rs`)

**Files:**
- Modify: `src/session.rs`
- Test: inline unit tests + `tests/scroll_ux.rs` (created here)

**Interfaces:**
- Consumes: existing `Session` internals (`parser`, `process_output`, `resize`).
- Produces (Tasks 3-7 rely on these exact signatures):
  - `Session::scrolled(&self) -> usize` — rows scrolled back, 0 = live
  - `Session::scroll_view(&self) -> (usize, usize)` — `(scrollback_len, offset)` without mutation (len is the cached field)
  - `Session::scroll_by(&mut self, delta: i32)` — positive scrolls back into history, clamped both ends
  - `Session::set_scroll(&mut self, offset: usize)` / `scroll_to_top(&mut self)` / `scroll_to_bottom(&mut self)`
  - `pub(crate) fn anchored_offset(offset: usize, len_before: usize, len_after: usize) -> usize` — pure anchoring math
  - `Session::probe_scrollback_len(&mut self)` is private; the cached `scrollback_len` field is refreshed at the end of `process_output` and `resize`.

- [ ] **Step 1: Write failing tests**

Inline in `src/session.rs` (module `scroll_tests`) — these validate the vt100 assumptions and the pure math without a PTY:

```rust
#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn anchored_offset_grows_with_new_scrollback() {
        assert_eq!(anchored_offset(5, 10, 13), 8); // 3 new lines while scrolled 5
        assert_eq!(anchored_offset(5, 10, 10), 5); // no growth
        assert_eq!(anchored_offset(0, 10, 13), 3); // caller only anchors when offset > 0; math still total
    }

    #[test]
    fn vt100_set_scrollback_self_clamps_and_probe_reads_len() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..30 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        // 30 lines on a 5-row screen -> scrollback holds the rest
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        assert!(len >= 20, "expected >=20 scrollback rows, got {len}");
        parser.screen_mut().set_scrollback(0);
        assert_eq!(parser.screen().scrollback(), 0);
        // offset beyond len clamps to len
        parser.screen_mut().set_scrollback(len + 50);
        assert_eq!(parser.screen().scrollback(), len);
    }

    #[test]
    fn scrolled_view_shows_history() {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..30 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        assert!(!parser.screen().contents().contains("line-0"));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert!(parser.screen().contents().contains("line-0"));
    }
}
```

Create `tests/scroll_ux.rs` with the shared pump helper and the first two integration tests (real shells; the file grows in later tasks):

```rust
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::session::Session;
use agent_mux::status::Status;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub fn shell_profile() -> Profile {
    #[cfg(windows)]
    let command = "cmd.exe";
    #[cfg(not(windows))]
    let command = "sh";
    Profile {
        name: "shell".into(),
        command: command.into(),
        args: vec![],
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
    }
}

/// Command that prints numbered lines 1..=n, then exits.
pub fn print_lines_args(n: u32) -> Vec<String> {
    #[cfg(windows)]
    return vec!["/c".into(), format!("for /L %i in (1,1,{n}) do @echo line-%i")];
    #[cfg(not(windows))]
    return vec!["-c".into(), format!("i=1; while [ $i -le {n} ]; do echo line-$i; i=$((i+1)); done")];
}

pub async fn pump_session(
    rx: &mut mpsc::Receiver<AppEvent>,
    session: &mut Session,
    timeout: Duration,
    mut pred: impl FnMut(&Session) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(session) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) if id == session.id => {
                session.process_output(&bytes, Instant::now(), false);
            }
            Ok(Some(AppEvent::PtyExit { id })) if id == session.id => session.mark_exited(),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(session)
}

#[tokio::test]
async fn scrolling_reveals_history_and_snaps_back() {
    let (tx, mut rx) = mpsc::channel(256);
    let profile = Profile { args: print_lines_args(100), ..shell_profile() };
    let mut s = Session::spawn(1, profile, std::env::temp_dir(), 10, 80, tx).unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
            && s.parser.screen().contents().contains("line-100")
    })
    .await;
    assert!(ok, "screen: {:?}", s.parser.screen().contents());
    assert_eq!(s.scrolled(), 0);
    // line-1 scrolled out of the 10-row screen; scroll to top reveals it
    // (exact-line match: "line-1" is a substring of the visible "line-100")
    assert!(!s.parser.screen().contents().lines().any(|l| l.trim() == "line-1"));
    s.scroll_to_top();
    assert!(s.scrolled() > 0);
    assert!(s.parser.screen().contents().lines().any(|l| l.trim() == "line-1"));
    // scroll_by moves relative; scroll_to_bottom restores live view
    let at_top = s.scrolled();
    s.scroll_by(-3);
    assert_eq!(s.scrolled(), at_top - 3);
    s.scroll_to_bottom();
    assert_eq!(s.scrolled(), 0);
    assert!(s.parser.screen().contents().contains("line-100"));
    let (len, offset) = s.scroll_view();
    assert_eq!(offset, 0);
    assert!(len >= 90, "cached scrollback_len should be large, got {len}");
}

#[tokio::test]
async fn view_stays_anchored_while_new_output_arrives() {
    let (tx, mut rx) = mpsc::channel(256);
    let mut s = Session::spawn(1, shell_profile(), std::env::temp_dir(), 10, 80, tx).unwrap();
    // interactive shell: print 50 lines, scroll up, then print 20 more
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        !s.parser.screen().contents().trim().is_empty()
    })
    .await;
    assert!(ok);
    #[cfg(windows)]
    s.write_bytes(b"for /L %i in (1,1,50) do @echo first-%i\r").unwrap();
    #[cfg(not(windows))]
    s.write_bytes(b"i=1; while [ $i -le 50 ]; do echo first-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        s.parser.screen().contents().contains("first-50")
    })
    .await;
    assert!(ok);
    s.scroll_by(20);
    let top_line_before = s.parser.screen().contents().lines().next().unwrap_or_default().to_string();
    let offset_before = s.scrolled();
    #[cfg(windows)]
    s.write_bytes(b"for /L %i in (1,1,20) do @echo second-%i\r").unwrap();
    #[cfg(not(windows))]
    s.write_bytes(b"i=1; while [ $i -le 20 ]; do echo second-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        s.scrolled() > offset_before // anchoring grew the offset
    })
    .await;
    assert!(ok, "offset never grew: still {}", s.scrolled());
    let top_line_after = s.parser.screen().contents().lines().next().unwrap_or_default().to_string();
    assert_eq!(top_line_before, top_line_after, "view moved while scrolled");
    s.kill();
    let _ = pump_session(&mut rx, &mut s, Duration::from_secs(10), |s| {
        matches!(s.status(Instant::now()), Status::Exited(_))
    })
    .await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib scroll_tests` then `cargo test --test scroll_ux`
Expected: `scroll_tests` may pass (they test vt100 + a not-yet-written pure fn — `anchored_offset` missing = compile error); `scroll_ux` fails to compile (`scrolled`/`scroll_view`/`scroll_by`/`scroll_to_*` not defined).

- [ ] **Step 3: Implement**

In `src/session.rs` — add the field, the pure fn, and the methods; hook anchoring + cache refresh into `process_output` and `resize`:

```rust
/// Pure anchoring math: while the user is scrolled back, new lines pushed
/// into scrollback must grow the offset so the visible content stays put.
pub(crate) fn anchored_offset(offset: usize, len_before: usize, len_after: usize) -> usize {
    offset + len_after.saturating_sub(len_before)
}
```

Add `scrollback_len: usize` to the `Session` struct (initialize `0` in `spawn`), and:

```rust
    /// Rows currently scrolled back (0 = live bottom).
    pub fn scrolled(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// (total scrollback rows, current offset) without touching the parser.
    /// The length is cached by process_output/resize because probing vt100
    /// for it requires mutation, and the render path is immutable.
    pub fn scroll_view(&self) -> (usize, usize) {
        (self.scrollback_len, self.parser.screen().scrollback())
    }

    /// vt100 doesn't expose scrollback length; set_scrollback self-clamps,
    /// so probing with usize::MAX and restoring reads it in O(1).
    fn probe_scrollback_len(&mut self) -> usize {
        let cur = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let len = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(cur);
        len
    }

    /// Positive delta scrolls back into history; negative toward live.
    /// Clamped at both ends (vt100 clamps the top, we clamp at 0).
    pub fn scroll_by(&mut self, delta: i32) {
        let cur = self.parser.screen().scrollback() as i64;
        let next = (cur + i64::from(delta)).max(0) as usize;
        self.parser.screen_mut().set_scrollback(next);
    }

    pub fn set_scroll(&mut self, offset: usize) {
        self.parser.screen_mut().set_scrollback(offset);
    }

    pub fn scroll_to_top(&mut self) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }
```

In `process_output`, wrap the existing `self.parser.process(bytes)` call:

```rust
        let offset = self.parser.screen().scrollback();
        let len_before = self.scrollback_len;
        self.parser.process(bytes);
        self.scrollback_len = self.probe_scrollback_len();
        if offset > 0 {
            // content-anchored: don't let new output move the scrolled view
            let anchored = anchored_offset(offset, len_before, self.scrollback_len);
            self.parser.screen_mut().set_scrollback(anchored);
        }
```

At the end of `resize` (reflow can change scrollback): `self.scrollback_len = self.probe_scrollback_len();`

Known limitation (document as a comment where anchoring happens): once scrollback hits the 1000-line cap, vt100 drops oldest rows and `len` stops growing, so a view pinned at the very top can drift — same trade-off real emulators make at their buffer cap.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib scroll_tests && cargo test --test scroll_ux`
Expected: 3 unit + 2 integration passing. Then full `cargo test` — no regressions (the process_output change touches every existing integration test's path).

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: session scrollback view with content anchoring"
```

---

### Task 2: Wheel routing + mouse encoding (`mouse.rs`)

**Files:**
- Create: `src/mouse.rs`
- Modify: `src/lib.rs` (add `pub mod mouse;`)

**Interfaces:**
- Consumes: `crossterm::event::{MouseButton, MouseEventKind}`, `vt100::{MouseProtocolMode, MouseProtocolEncoding}`.
- Produces (Task 3/5 rely on these exact signatures):
  - `mouse::WheelRoute` — `Local | Forward | Arrows`
  - `mouse::route_wheel(shift: bool, attached: bool, mode: MouseProtocolMode, alt_screen: bool) -> WheelRoute` — pure, implements the spec's 4-rule order
  - `mouse::encode_mouse(kind: MouseEventKind, col: u16, row: u16, mode: MouseProtocolMode, encoding: MouseProtocolEncoding) -> Option<Vec<u8>>` — col/row are PANE-LOCAL 0-based; `None` = don't forward (child didn't ask, or event class not reported under `mode`)

- [ ] **Step 1: Write failing tests**

In `src/mouse.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEventKind};
    use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};

    #[test]
    fn wheel_routing_rule_order() {
        // 1. Shift always local, even when the child wants mouse / alt screen
        assert!(matches!(route_wheel(true, true, Mode::PressRelease, true), WheelRoute::Local));
        // 2. child wants mouse -> forward
        assert!(matches!(route_wheel(false, true, Mode::Press, false), WheelRoute::Forward));
        assert!(matches!(route_wheel(false, true, Mode::AnyMotion, true), WheelRoute::Forward));
        // 3. alt screen without mouse interest -> arrows
        assert!(matches!(route_wheel(false, true, Mode::None, true), WheelRoute::Arrows));
        // 4. plain -> local
        assert!(matches!(route_wheel(false, true, Mode::None, false), WheelRoute::Local));
        // Control-mode preview: always local regardless of child state
        assert!(matches!(route_wheel(false, false, Mode::AnyMotion, true), WheelRoute::Local));
    }

    #[test]
    fn sgr_encoding_press_release_wheel() {
        let e = Enc::Sgr;
        assert_eq!(
            encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 9, Mode::PressRelease, e),
            Some(b"\x1b[<0;5;10M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseEventKind::Up(MouseButton::Left), 4, 9, Mode::PressRelease, e),
            Some(b"\x1b[<0;5;10m".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseEventKind::Down(MouseButton::Right), 0, 0, Mode::PressRelease, e),
            Some(b"\x1b[<2;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseEventKind::ScrollUp, 4, 9, Mode::PressRelease, e),
            Some(b"\x1b[<64;5;10M".to_vec())
        );
        assert_eq!(
            encode_mouse(MouseEventKind::ScrollDown, 4, 9, Mode::PressRelease, e),
            Some(b"\x1b[<65;5;10M".to_vec())
        );
    }

    #[test]
    fn drag_only_reported_in_motion_modes() {
        let e = Enc::Sgr;
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(encode_mouse(drag, 2, 2, Mode::Press, e), None);
        assert_eq!(encode_mouse(drag, 2, 2, Mode::PressRelease, e), None);
        // drag = button + 32 motion flag
        assert_eq!(
            encode_mouse(drag, 2, 2, Mode::ButtonMotion, e),
            Some(b"\x1b[<32;3;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(drag, 2, 2, Mode::AnyMotion, e),
            Some(b"\x1b[<32;3;3M".to_vec())
        );
    }

    #[test]
    fn x10_mode_reports_press_only() {
        let e = Enc::Sgr;
        assert!(encode_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, Mode::Press, e).is_some());
        assert_eq!(encode_mouse(MouseEventKind::Up(MouseButton::Left), 0, 0, Mode::Press, e), None);
    }

    #[test]
    fn default_encoding_uses_byte_offsets() {
        // legacy: ESC [ M, then button+32, col+33, row+33 (1-based + 32)
        assert_eq!(
            encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 9, Mode::PressRelease, Enc::Default),
            Some(vec![0x1b, b'[', b'M', 32, 4 + 33, 9 + 33])
        );
        // release is button 3 in legacy encoding
        assert_eq!(
            encode_mouse(MouseEventKind::Up(MouseButton::Left), 4, 9, Mode::PressRelease, Enc::Default),
            Some(vec![0x1b, b'[', b'M', 3 + 32, 4 + 33, 9 + 33])
        );
        // coordinates that don't fit a byte are dropped, not corrupted
        assert_eq!(
            encode_mouse(MouseEventKind::Down(MouseButton::Left), 250, 9, Mode::PressRelease, Enc::Default),
            None
        );
    }

    #[test]
    fn disabled_mode_and_moved_are_none() {
        assert_eq!(
            encode_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, Mode::None, Enc::Sgr),
            None
        );
        assert_eq!(
            encode_mouse(MouseEventKind::Moved, 0, 0, Mode::PressRelease, Enc::Sgr),
            None
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mouse`
Expected: compile error (module/types not defined).

- [ ] **Step 3: Implement `src/mouse.rs`**

```rust
use crossterm::event::{MouseButton, MouseEventKind};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Where a wheel tick over the main pane goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRoute {
    /// Scroll our own scrollback view.
    Local,
    /// Encode and forward to the child pty (it asked for mouse events).
    Forward,
    /// Send arrow-key sequences (full-screen app without mouse interest).
    Arrows,
}

/// Spec's normative wheel rule, in order. `attached` is false for the
/// Control-mode preview, which always scrolls locally.
pub fn route_wheel(
    shift: bool,
    attached: bool,
    mode: MouseProtocolMode,
    alt_screen: bool,
) -> WheelRoute {
    if shift || !attached {
        return WheelRoute::Local;
    }
    if mode != MouseProtocolMode::None {
        return WheelRoute::Forward;
    }
    if alt_screen {
        return WheelRoute::Arrows;
    }
    WheelRoute::Local
}

fn button_code(kind: MouseEventKind) -> Option<u16> {
    Some(match kind {
        MouseEventKind::Down(b) | MouseEventKind::Up(b) => match b {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        },
        // motion flag (32) + button
        MouseEventKind::Drag(b) => {
            32 + match b {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
            }
        }
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        _ => return None, // Moved / ScrollLeft / ScrollRight: not forwarded
    })
}

/// True if `mode` reports this event class at all.
fn mode_reports(kind: MouseEventKind, mode: MouseProtocolMode) -> bool {
    match kind {
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            mode != MouseProtocolMode::None
        }
        MouseEventKind::Up(_) => matches!(
            mode,
            MouseProtocolMode::PressRelease
                | MouseProtocolMode::ButtonMotion
                | MouseProtocolMode::AnyMotion
        ),
        MouseEventKind::Drag(_) => matches!(
            mode,
            MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
        ),
        _ => false,
    }
}

/// Encode a mouse event for the child. `col`/`row` are pane-local 0-based.
/// `None` = the child didn't ask for this event (or it can't be encoded).
/// The `Utf8` encoding is emitted as the legacy byte form: coordinates that
/// fit in a byte are identical in both, and every modern TUI negotiates SGR
/// anyway -- SGR is where unlimited coordinates actually work.
pub fn encode_mouse(
    kind: MouseEventKind,
    col: u16,
    row: u16,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    if !mode_reports(kind, mode) {
        return None;
    }
    let btn = button_code(kind)?;
    let (x, y) = (col + 1, row + 1); // 1-based
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let terminator = if matches!(kind, MouseEventKind::Up(_)) { 'm' } else { 'M' };
            Some(format!("\x1b[<{btn};{x};{y}{terminator}").into_bytes())
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            // legacy: byte-sized fields only
            let btn = if matches!(kind, MouseEventKind::Up(_)) { 3 } else { btn };
            let bx = u8::try_from(x + 32).ok()?;
            let by = u8::try_from(y + 32).ok()?;
            let bb = u8::try_from(btn + 32).ok()?;
            Some(vec![0x1b, b'[', b'M', bb, bx, by])
        }
    }
}
```

Add `pub mod mouse;` to `src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mouse`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: wheel routing rule and mouse event encoding"
```

---

### Task 3: Mouse capture, wheel + keyboard scrolling, snap-to-live (`events.rs`, `main.rs`, `app.rs`, `ui.rs`)

**Files:**
- Modify: `src/events.rs`, `src/main.rs`, `src/app.rs`, `src/ui.rs`
- Test: inline ui tests + `tests/scroll_ux.rs` (extend)

**Interfaces:**
- Consumes: Task 1 (`scroll_by`, `scroll_to_top/bottom`, `scrolled`, `scroll_view`), Task 2 (`route_wheel`, `encode_mouse`, `WheelRoute`).
- Produces:
  - `events::AppEvent::Mouse(crossterm::event::MouseEvent)` (new variant)
  - `App::handle_mouse(&mut self, ev: MouseEvent, now: Instant)` — main.rs calls this
  - `App::handle_ux_key(&mut self, key: &KeyEvent) -> bool` (private) — the chord interceptor; Tasks 5 and 7 ADD ARMS to this same function
  - `ui::PANE_ORIGIN: (u16, u16)` = `(SIDEBAR_WIDTH + 1, 1)` — (x, y) of the pane interior's top-left
  - `ui::pane_local(col: u16, row: u16, pane: (u16, u16)) -> Option<(u16, u16)>` — terminal coords → pane-local `(col, row)`, `None` outside; `pane` is App's `(rows, cols)`
  - `tests/scroll_ux.rs::pump_app` helper (Tasks 5/7 reuse): routes channel events into an `App`

- [ ] **Step 1: Write failing tests**

Add to `src/ui.rs` tests:

```rust
    #[test]
    fn pane_local_matches_layout_math() {
        // pane interior starts right of the sidebar border column
        assert_eq!(PANE_ORIGIN, (SIDEBAR_WIDTH + 1, 1));
        let pane = (37u16, 68u16); // (rows, cols) for a 100x40 terminal
        assert_eq!(pane_local(31, 1, pane), Some((0, 0)));
        assert_eq!(pane_local(31 + 67, 1 + 36, pane), Some((67, 36)));
        // one past either edge is outside
        assert_eq!(pane_local(30, 1, pane), None); // sidebar border
        assert_eq!(pane_local(31 + 68, 1, pane), None);
        assert_eq!(pane_local(31, 0, pane), None); // top border
        assert_eq!(pane_local(31, 1 + 37, pane), None);
    }
```

Add to `tests/scroll_ux.rs` (uses the Task 1 helpers; note the new imports at the top of the file: `use agent_mux::app::{App, Mode};` `use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};`):

```rust
pub async fn pump_app(
    rx: &mut mpsc::Receiver<AppEvent>,
    app: &mut App,
    timeout: Duration,
    mut pred: impl FnMut(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(app) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(app)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn wheel(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
    // (32, 2): inside the pane for any pane at least 2x2
    MouseEvent { kind, column: 32, row: 2, modifiers }
}

/// App with one spawned shell session that has printed `lines` lines and
/// exited, sized 10 rows x 80 cols.
async fn app_with_history(
    lines: u32,
) -> (App, mpsc::Receiver<AppEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(vec![shell_profile()], tx.clone());
    app.set_pane_size(10, 80);
    let profile = Profile { args: print_lines_args(lines), ..shell_profile() };
    let session = Session::spawn(900, profile, std::env::temp_dir(), 10, 80, tx).unwrap();
    app.sessions.push(session);
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        a.sessions[0]
            .parser
            .screen()
            .contents()
            .contains(&format!("line-{lines}"))
            && matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(ok, "history session never finished");
    (app, rx)
}

#[tokio::test]
async fn wheel_scrolls_control_preview_locally() {
    let (mut app, _rx) = app_with_history(100).await;
    for _ in 0..4 {
        app.handle_mouse(wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE), Instant::now());
    }
    assert_eq!(app.sessions[0].scrolled(), 12); // 4 ticks x 3 lines
    app.handle_mouse(wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 9);
}

#[tokio::test]
async fn wheel_outside_pane_is_ignored() {
    let (mut app, _rx) = app_with_history(100).await;
    let ev = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 5, // inside the sidebar
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(ev, Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
}

#[tokio::test]
async fn shift_paging_and_home_end() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_key(&shift_key(KeyCode::PageUp), Instant::now());
    let after_page = app.sessions[0].scrolled();
    assert!(after_page > 0);
    app.handle_key(&shift_key(KeyCode::Home), Instant::now());
    let (len, offset) = app.sessions[0].scroll_view();
    assert_eq!(offset, len);
    app.handle_key(&shift_key(KeyCode::End), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
    app.handle_key(&shift_key(KeyCode::PageUp), Instant::now());
    app.handle_key(&shift_key(KeyCode::PageDown), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0);
}

#[tokio::test]
async fn forwarded_key_snaps_to_live_when_attached() {
    // live interactive shell, filled with enough output to have scrollback
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(vec![shell_profile()], tx.clone());
    app.set_pane_size(10, 80);
    let session = Session::spawn(901, shell_profile(), std::env::temp_dir(), 10, 80, tx).unwrap();
    app.sessions.push(session);
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        !a.sessions[0].parser.screen().contents().trim().is_empty()
    })
    .await;
    assert!(ok, "shell never painted its prompt");
    #[cfg(windows)]
    app.sessions[0].write_bytes(b"for /L %i in (1,1,50) do @echo fill-%i\r").unwrap();
    #[cfg(not(windows))]
    app.sessions[0].write_bytes(b"i=1; while [ $i -le 50 ]; do echo fill-$i; i=$((i+1)); done\n").unwrap();
    let ok = pump_app(&mut rx, &mut app, Duration::from_secs(10), |a| {
        a.sessions[0].parser.screen().contents().contains("fill-50")
    })
    .await;
    assert!(ok);
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // attach
    assert!(matches!(app.mode, Mode::Attached));
    app.sessions[0].scroll_by(5);
    assert!(app.sessions[0].scrolled() > 0, "test setup: expected scrollback");
    app.handle_key(&key(KeyCode::Char('a')), Instant::now());
    assert_eq!(app.sessions[0].scrolled(), 0, "forwarded key must snap to live");
    app.kill_all();
}

#[tokio::test]
async fn scroll_indicator_renders_in_title() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (mut app, _rx) = app_with_history(100).await;
    app.sessions[0].scroll_by(7);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, &app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains("SCROLL"), "no scroll indicator: {text}");
    assert!(text.contains('7'), "offset missing from indicator");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test scroll_ux`
Expected: compile error (`handle_mouse`, `AppEvent::Mouse`, `pane_local` not defined).

- [ ] **Step 3: Implement**

`src/events.rs` — extend the enum (keep all existing doc comments):

```rust
use crossterm::event::{KeyEvent, MouseEvent};
// ... existing variants ...
    /// A captured terminal mouse event (wheel, click, drag). Routed by
    /// App::handle_mouse; events outside the main pane are dropped there.
    Mouse(MouseEvent),
```

`src/main.rs`:
- Imports: `use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};`
- `restore_terminal`: `let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);` (replacing the LeaveAlternateScreen-only line; disable-before-leave, both idempotent).
- Entry: change to `crossterm::execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;`
- Input thread: add arm `Ok(Event::Mouse(m)) => { if tx.blocking_send(AppEvent::Mouse(m)).is_err() { break; } }`
- `handle_event`: add arm `AppEvent::Mouse(m) => app.handle_mouse(m, Instant::now()),`

`src/ui.rs`:

```rust
/// (x, y) of the main pane's interior top-left cell. Keep in sync with
/// draw()'s layout: sidebar occupies columns 0..SIDEBAR_WIDTH, then the
/// pane's own left border, then the interior; row 0 is the pane's top
/// border. pane_local_matches_layout_math pins this against
/// main_pane_inner's numbers.
pub const PANE_ORIGIN: (u16, u16) = (SIDEBAR_WIDTH + 1, 1);

/// Translate absolute terminal coordinates into pane-local (col, row).
/// `pane` is App's pane_size, i.e. (rows, cols). None = outside the pane
/// interior (border cells count as outside).
pub fn pane_local(col: u16, row: u16, pane: (u16, u16)) -> Option<(u16, u16)> {
    let (rows, cols) = pane;
    let (x0, y0) = PANE_ORIGIN;
    if col >= x0 && col < x0 + cols && row >= y0 && row < y0 + rows {
        Some((col - x0, row - y0))
    } else {
        None
    }
}
```

In `draw_main`, derive the indicator and suppress the live cursor while scrolled (replace the `title` construction and the cursor `if`):

```rust
    let (_, scroll_offset) = session.scroll_view();
    let scroll_tag = if scroll_offset > 0 {
        format!("[SCROLL ↑ {scroll_offset}] ")
    } else {
        String::new()
    };
    let title = format!(
        " {} — {} [{label}] {scroll_tag}",
        session.profile.name,
        session.dir.display()
    );
    // ... unchanged block/inner/render_widget lines ...
    // real cursor while attached AND live: a scrolled view is history, the
    // cursor belongs to the bottom of the buffer
    if matches!(app.mode, Mode::Attached) && !screen.hide_cursor() && scroll_offset == 0 {
```

`src/app.rs` — imports gain `use crate::mouse::{WheelRoute, encode_mouse, route_wheel};`, `use crate::ui;`, and `MouseEvent, MouseEventKind` in the crossterm import. Three changes:

1. In `handle_key`, after `self.error = None;` insert:

```rust
        if self.handle_ux_key(key) {
            self.just_detached = false;
            return;
        }
```

2. New methods:

```rust
    /// Terminal-emulator chords intercepted before v1 dispatch (Ghostty /
    /// Windows Terminal convention: the app reserves Ctrl+Shift and
    /// Shift+navigation for itself; everything else still reaches the
    /// agent). Returns true if the key was consumed. Tasks: selection
    /// (Ctrl+Shift+C/V) and search (Ctrl+Shift+F) add arms here.
    fn handle_ux_key(&mut self, key: &KeyEvent) -> bool {
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return false;
        };
        match (key.code, shift) {
            (KeyCode::PageUp, true) => {
                s.scroll_by(page);
                true
            }
            (KeyCode::PageDown, true) => {
                s.scroll_by(-page);
                true
            }
            (KeyCode::Home, true) => {
                s.scroll_to_top();
                true
            }
            (KeyCode::End, true) => {
                s.scroll_to_bottom();
                true
            }
            _ => false,
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, _now: Instant) {
        let Some((lcol, lrow)) = ui::pane_local(ev.column, ev.row, self.pane_size) else {
            return; // outside the main pane: sidebar stays keyboard-driven in this iteration
        };
        let attached = matches!(self.mode, Mode::Attached);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        // read child terminal state up front so no borrow is held across
        // the mutating calls below
        let Some((mouse_mode, enc, alt, app_cursor)) = self.sessions.get(self.selected).map(|s| {
            let sc = s.parser.screen();
            (
                sc.mouse_protocol_mode(),
                sc.mouse_protocol_encoding(),
                sc.alternate_screen(),
                sc.application_cursor(),
            )
        }) else {
            return;
        };
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                match route_wheel(shift, attached, mouse_mode, alt) {
                    WheelRoute::Local => {
                        if let Some(s) = self.sessions.get_mut(self.selected) {
                            s.scroll_by(if up { 3 } else { -3 });
                        }
                    }
                    WheelRoute::Forward => {
                        if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                            self.forward_bytes(&bytes);
                        }
                    }
                    WheelRoute::Arrows => {
                        let seq: &[u8] = match (up, app_cursor) {
                            (true, false) => b"\x1b[A",
                            (true, true) => b"\x1bOA",
                            (false, false) => b"\x1b[B",
                            (false, true) => b"\x1bOB",
                        };
                        self.forward_bytes(&seq.repeat(3));
                    }
                }
            }
            _ => {
                // Press/drag/release: when attached and the agent asked for
                // mouse events (and Shift isn't overriding), the agent owns
                // the mouse. Local selection arrives in a later task.
                if attached && !shift
                    && let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc)
                {
                    self.forward_bytes(&bytes);
                }
            }
        }
    }
```

3. Snap-to-live in `apply` — the `ForwardBytes` and `SendLiteralDetachKey` arms both gain, before their forward:

```rust
            Action::ForwardBytes(bytes) => {
                self.snap_selected_to_live();
                self.forward_bytes(&bytes);
            }
```

and in the `SendLiteralDetachKey` arm insert `self.snap_selected_to_live();` immediately before the `if self.forward_bytes(&[0x11])` line, plus the helper:

```rust
    /// Spec: any keystroke forwarded while scrolled first snaps the view
    /// back to the live bottom, like every terminal emulator.
    fn snap_selected_to_live(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.selected)
            && s.scrolled() > 0
        {
            s.scroll_to_bottom();
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test scroll_ux && cargo test`
Expected: all scroll_ux tests green (7 total in the file now), full suite green, zero warnings.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: mouse capture with emulator wheel routing and scroll keys"
```

---

### Task 4: Selection model (`selection.rs`)

**Files:**
- Create: `src/selection.rs`
- Modify: `src/lib.rs` (add `pub mod selection;`)

**Interfaces:**
- Consumes: `vt100::Parser<CB>` / `vt100::Callbacks` (generic — tests use the plain default parser, the app uses `Parser<BellCounter>`).
- Produces (Tasks 5-7 rely on these exact signatures):
  - `selection::Pos { pub row: usize, pub col: u16 }` — grid-absolute position (`Copy, PartialEq, Eq, Debug`)
  - `selection::Selection { pub anchor: Pos, pub head: Pos }` (`Copy, Debug`) with `new(p: Pos)`, `is_empty(&self) -> bool`, `normalized(&self) -> (Pos, Pos)`, `contains(&self, row: usize, col: u16) -> bool`
  - `selection::abs_row(scrollback_len: usize, offset: usize, visual_row: u16) -> usize` — the one coordinate formula everyone shares
  - `selection::row_cells<CB: vt100::Callbacks>(parser: &mut vt100::Parser<CB>, scrollback_len: usize, row: usize) -> Vec<(u16, String)>` — (col, contents) of a grid-absolute row, wide-continuation cells skipped, offset restored afterward (search reuses this in Task 6)
  - `selection::extract_text<CB: vt100::Callbacks>(parser: &mut vt100::Parser<CB>, scrollback_len: usize, sel: &Selection) -> String`
- Coordinate system (normative for Tasks 5-7): abs row 0 = oldest scrollback row; rows `[scrollback_len, scrollback_len + screen_rows)` are the live screen; with scroll offset `o`, visible visual row `v` shows abs row `scrollback_len - o + v`.

- [ ] **Step 1: Write failing tests**

In `src/selection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 5-row x 20-col parser fed 12 numbered lines: rows row-00..row-11,
    /// scrollback holds the first 7, screen shows row-07..row-11.
    fn parser_with_lines() -> (vt100::Parser, usize) {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..12 {
            parser.process(format!("row-{i:02}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(0);
        (parser, len)
    }

    #[test]
    fn abs_row_formula() {
        // live view (offset 0): visual 0 is the first on-screen row
        assert_eq!(abs_row(7, 0, 0), 7);
        // scrolled all the way back: visual 0 is the oldest row
        assert_eq!(abs_row(7, 7, 0), 0);
        assert_eq!(abs_row(7, 3, 2), 6);
    }

    #[test]
    fn normalization_swaps_reverse_drags() {
        let sel = Selection {
            anchor: Pos { row: 5, col: 10 },
            head: Pos { row: 3, col: 2 },
        };
        let (start, end) = sel.normalized();
        assert_eq!(start, Pos { row: 3, col: 2 });
        assert_eq!(end, Pos { row: 5, col: 10 });
        // same row, reversed cols
        let sel = Selection {
            anchor: Pos { row: 4, col: 9 },
            head: Pos { row: 4, col: 1 },
        };
        let (start, end) = sel.normalized();
        assert_eq!((start.col, end.col), (1, 9));
    }

    #[test]
    fn contains_linear_selection_semantics() {
        let sel = Selection {
            anchor: Pos { row: 2, col: 5 },
            head: Pos { row: 4, col: 3 },
        };
        assert!(!sel.contains(2, 4)); // before start col on first row
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 19)); // rest of first row
        assert!(sel.contains(3, 0)); // full middle row
        assert!(sel.contains(3, 19));
        assert!(sel.contains(4, 0));
        assert!(sel.contains(4, 3)); // inclusive end
        assert!(!sel.contains(4, 4));
        assert!(!sel.contains(1, 10));
        assert!(!sel.contains(5, 0));
    }

    #[test]
    fn extract_within_live_screen() {
        let (mut parser, len) = parser_with_lines();
        // rows 7 and 8 are "row-07" and "row-08" on the live screen
        let sel = Selection {
            anchor: Pos { row: 7, col: 0 },
            head: Pos { row: 8, col: 5 },
        };
        assert_eq!(extract_text(&mut parser, len, &sel), "row-07\nrow-08");
    }

    #[test]
    fn extract_spans_scrollback_and_restores_offset() {
        let (mut parser, len) = parser_with_lines();
        parser.screen_mut().set_scrollback(2); // some arbitrary view
        let sel = Selection {
            anchor: Pos { row: 5, col: 0 }, // in scrollback
            head: Pos { row: 7, col: 5 },   // first live row
        };
        assert_eq!(extract_text(&mut parser, len, &sel), "row-05\nrow-06\nrow-07");
        assert_eq!(parser.screen().scrollback(), 2, "offset must be restored");
    }

    #[test]
    fn extract_reverse_drag_same_row_column_range() {
        let (mut parser, len) = parser_with_lines();
        let sel = Selection {
            anchor: Pos { row: 7, col: 4 },
            head: Pos { row: 7, col: 0 },
        };
        assert_eq!(extract_text(&mut parser, len, &sel), "row-0");
    }

    #[test]
    fn extract_handles_wide_chars() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process("你好ab".as_bytes());
        let sel = Selection {
            anchor: Pos { row: 0, col: 0 },
            head: Pos { row: 0, col: 5 },
        };
        assert_eq!(extract_text(&mut parser, 0, &sel), "你好ab");
    }

    #[test]
    fn row_cells_skips_wide_continuations() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process("你a".as_bytes());
        let cells = row_cells(&mut parser, 0, 0);
        // col 0 = 你 (wide), col 1 is its continuation (skipped), col 2 = a
        assert_eq!(cells[0], (0, "你".to_string()));
        assert!(!cells.iter().any(|(c, _)| *c == 1));
        assert!(cells.contains(&(2, "a".to_string())));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib selection`
Expected: compile error (module not defined).

- [ ] **Step 3: Implement `src/selection.rs`**

```rust
//! Pure selection model over the vt100 grid.
//!
//! Coordinates are grid-absolute so a selection survives scrolling: row 0
//! is the oldest scrollback row, rows [scrollback_len, scrollback_len +
//! screen_rows) are the live screen. With scroll offset `o`, visible
//! visual row `v` shows abs row `scrollback_len - o + v`.
//!
//! Known limit (same trade-off as any emulator at its buffer cap): once
//! vt100 starts dropping rows at SCROLLBACK_LINES, absolute rows shift by
//! the number of dropped rows and a held selection drifts. Selections are
//! short-lived (a drag), so this is acceptable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub row: usize,
    pub col: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

/// Grid-absolute row shown at visible row `visual_row` of the current view.
pub fn abs_row(scrollback_len: usize, offset: usize, visual_row: u16) -> usize {
    scrollback_len - offset + usize::from(visual_row)
}

impl Selection {
    pub fn new(p: Pos) -> Self {
        Selection { anchor: p, head: p }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// (start, end) in reading order, both inclusive.
    pub fn normalized(&self) -> (Pos, Pos) {
        let a = self.anchor;
        let h = self.head;
        if (h.row, h.col) < (a.row, a.col) {
            (h, a)
        } else {
            (a, h)
        }
    }

    /// Linear (stream) selection semantics, like every terminal: first row
    /// from start col to end-of-row, middle rows fully, last row up to end
    /// col, all bounds inclusive.
    pub fn contains(&self, row: usize, col: u16) -> bool {
        let (s, e) = self.normalized();
        if row < s.row || row > e.row {
            return false;
        }
        if s.row == e.row {
            return col >= s.col && col <= e.col;
        }
        if row == s.row {
            return col >= s.col;
        }
        if row == e.row {
            return col <= e.col;
        }
        true
    }
}

/// (col, contents) for every non-empty, non-continuation cell of a
/// grid-absolute row. Temporarily moves the scrollback offset to bring the
/// row into view and restores it before returning.
pub fn row_cells<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    row: usize,
) -> Vec<(u16, String)> {
    let (screen_rows, cols) = parser.screen().size();
    let saved = parser.screen().scrollback();
    let (offset, visual) = if row >= scrollback_len {
        let v = row - scrollback_len;
        if v >= usize::from(screen_rows) {
            return Vec::new(); // below the live screen: nothing there
        }
        (0, v as u16)
    } else {
        (scrollback_len - row, 0u16)
    };
    parser.screen_mut().set_scrollback(offset);
    let mut out = Vec::new();
    {
        let screen = parser.screen();
        for c in 0..cols {
            if let Some(cell) = screen.cell(visual, c) {
                if cell.is_wide_continuation() {
                    continue;
                }
                out.push((c, cell.contents()));
            }
        }
    }
    parser.screen_mut().set_scrollback(saved);
    out
}

/// Text of the selection: rows joined with `\n`, trailing whitespace
/// trimmed per row, empty cells inside the range read as spaces.
pub fn extract_text<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    sel: &Selection,
) -> String {
    let (start, end) = sel.normalized();
    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let cells = row_cells(parser, scrollback_len, row);
        let (from, to) = (
            if row == start.row { start.col } else { 0 },
            if row == end.row { end.col } else { u16::MAX },
        );
        let mut line = String::new();
        for (col, contents) in &cells {
            if *col < from || *col > to {
                continue;
            }
            if contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(contents);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}
```

Add `pub mod selection;` to `src/lib.rs`.

API-drift note: `Cell::is_wide_continuation()` and `Cell::contents()` are the vt100 0.16 names — verify against the vendored source if the compile disagrees, and adapt minimally (recording it in your report).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib selection`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: grid-absolute selection model with vt100 text extraction"
```

---

### Task 5: Selection wiring, copy-on-select, paste (`app.rs`, `ui.rs`, `Cargo.toml`)

**Files:**
- Modify: `Cargo.toml` (add arboard), `src/app.rs`, `src/ui.rs`
- Test: inline ui tests + `tests/scroll_ux.rs` (extend)

**Interfaces:**
- Consumes: Task 4's `Selection`/`Pos`/`abs_row`/`extract_text`, Task 3's `handle_ux_key` + `handle_mouse` + `pane_local`, Task 1's `scroll_view`.
- Produces:
  - `app::ActiveSelection { pub session_id: usize, pub sel: selection::Selection, pub dragging: bool }`
  - `App.selection: Option<ActiveSelection>` (pub field)
  - `App::displayed_selection(&self) -> Option<&selection::Selection>` — `Some` only when the selection belongs to the currently displayed (selected) session
  - `ui::apply_selection_highlight(buf: &mut ratatui::buffer::Buffer, inner: Rect, sel: &selection::Selection, scrollback_len: usize, offset: usize)` — pure buffer post-pass (Task 7's search highlight mirrors it)
- Behavior contract: mouse release with a non-empty selection copies to the system clipboard (copy-on-select); a plain click (down+up, no movement) clears the selection; Shift forces local selection even when the agent owns the mouse; Ctrl+Shift+C copies; Ctrl+Shift+V pastes into the attached agent (bracketed iff the child enabled it, otherwise newlines normalized to `\r`); clipboard errors land in `app.error`, never panic.

- [ ] **Step 1: Add the dependency**

```powershell
cargo add arboard
```

Expected major: arboard 3. Run `cargo build` once — it must compile before the code steps.

- [ ] **Step 2: Write failing tests**

In `src/ui.rs` tests:

```rust
    #[test]
    fn selection_highlight_marks_expected_cells() {
        use crate::selection::{Pos, Selection};
        use ratatui::buffer::Buffer;
        let area = Rect::new(2, 1, 10, 4); // inner pane at (2,1), 10 cols x 4 rows
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        // len=6, offset=2 -> visual row v shows abs row 4 + v
        let sel = Selection {
            anchor: Pos { row: 5, col: 3 },
            head: Pos { row: 6, col: 1 },
        };
        apply_selection_highlight(&mut buf, area, &sel, 6, 2);
        // abs 5 = visual 1, abs 6 = visual 2
        assert!(buf[(2 + 3, 1 + 1)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(2 + 9, 1 + 1)].modifier.contains(Modifier::REVERSED)); // rest of first row
        assert!(buf[(2, 1 + 2)].modifier.contains(Modifier::REVERSED)); // start of last row
        assert!(buf[(2 + 1, 1 + 2)].modifier.contains(Modifier::REVERSED)); // inclusive end
        assert!(!buf[(2 + 2, 1 + 2)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(2 + 2, 1)].modifier.contains(Modifier::REVERSED)); // row above
        assert!(!buf[(2 + 3, 1 + 3)].modifier.contains(Modifier::REVERSED)); // row below
    }
```

Add to `tests/scroll_ux.rs`:

```rust
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
}

#[tokio::test]
async fn drag_selects_and_extracts_screen_text() {
    let (mut app, _rx) = app_with_history(100).await;
    // pane interior starts at (31, 1); drag across two rows
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31 + 7, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 31 + 7, 2), Instant::now());
    let active = app.selection.as_ref().expect("selection should exist");
    assert!(!active.sel.is_empty());
    assert!(!active.dragging, "release must end the drag");
    let sel = active.sel;
    let (len, _) = app.sessions[0].scroll_view();
    let text = agent_mux::selection::extract_text(&mut app.sessions[0].parser, len, &sel);
    // the visible screen at rows 0..=1 holds two of the trailing lines
    // (line-93.., depending on prompt rows); assert shape, not exact rows
    assert!(text.contains('\n'), "two-row drag extracts two rows: {text:?}");
    assert!(text.contains("line-9"), "extracted from visible tail: {text:?}");
}

#[tokio::test]
async fn plain_click_clears_selection() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 40, 2), Instant::now());
    assert!(app.selection.is_some());
    // click without drag clears
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 1), Instant::now());
    assert!(app.selection.is_none());
}

#[tokio::test]
async fn selection_does_not_leak_across_sessions() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 2), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 40, 2), Instant::now());
    assert!(app.displayed_selection().is_some());
    // respawn replaces the session (fresh id) -> selection no longer displayed
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(app.displayed_selection().is_none());
    app.kill_all();
}

#[tokio::test]
#[ignore = "mutates the real system clipboard"]
async fn copy_on_select_reaches_clipboard() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31 + 7, 1), Instant::now());
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 31 + 7, 1), Instant::now());
    let text = arboard::Clipboard::new().unwrap().get_text().unwrap();
    assert!(text.contains("line-9"), "clipboard got: {text:?}");
}
```

(Add `arboard` usage in tests via the crate's normal dependency — it is a main dependency, usable from integration tests.)

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test scroll_ux drag_selects` (and the ui test)
Expected: compile error (`App.selection`, `ActiveSelection`, `apply_selection_highlight` not defined).

- [ ] **Step 4: Implement**

`src/app.rs` — imports gain `use crate::selection::{self, Pos, Selection};` and `MouseButton` in the crossterm import. Add:

```rust
#[derive(Debug)]
pub struct ActiveSelection {
    pub session_id: usize,
    pub sel: Selection,
    pub dragging: bool,
}
```

New `App` field `pub selection: Option<ActiveSelection>` (init `None` in `new`). New methods:

```rust
    /// The selection, but only if it belongs to the session currently shown
    /// in the main pane -- selections on removed/replaced/other sessions
    /// are treated as gone.
    pub fn displayed_selection(&self) -> Option<&Selection> {
        let shown = self.sessions.get(self.selected)?.id;
        self.selection
            .as_ref()
            .filter(|a| a.session_id == shown)
            .map(|a| &a.sel)
    }

    fn copy_to_clipboard(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            self.error = Some(format!("clipboard: {e}"));
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.displayed_selection().copied() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        let text = selection::extract_text(&mut s.parser, len, &sel);
        self.copy_to_clipboard(text);
    }

    fn paste_into_attached(&mut self) {
        if !matches!(self.mode, Mode::Attached) {
            return;
        }
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(format!("clipboard: {e}"));
                return;
            }
        };
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        let bytes = paste_bytes(&text, s.parser.screen().bracketed_paste());
        self.snap_selected_to_live();
        self.forward_bytes(&bytes);
    }
```

plus the free function (file-level, near `is_ctrl_q`) with its unit tests appended to one of app.rs's existing test modules:

```rust
/// Bytes to write for a paste: wrapped in bracketed-paste markers verbatim
/// when the child enabled bracketed paste, otherwise newlines normalized
/// to CR so a multi-line paste presses Enter instead of inserting raw LFs.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(text.as_bytes());
        b.extend_from_slice(b"\x1b[201~");
        b
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod paste_tests {
    use super::paste_bytes;

    #[test]
    fn bracketed_paste_wraps_verbatim() {
        assert_eq!(
            paste_bytes("a\r\nb\nc", true),
            b"\x1b[200~a\r\nb\nc\x1b[201~".to_vec()
        );
    }

    #[test]
    fn unbracketed_paste_normalizes_newlines_to_cr() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(paste_bytes("plain", false), b"plain".to_vec());
    }
}
```

`handle_ux_key` gains two arms in its match (before the `_ => false` arm; `ctrl` computed as `key.modifiers.contains(KeyModifiers::CONTROL)` at the top). Note crossterm may report shifted letters as upper- OR lowercase depending on platform — match both:

```rust
            (KeyCode::Char('c') | KeyCode::Char('C'), true) if ctrl => {
                self.copy_selection();
                true
            }
            (KeyCode::Char('v') | KeyCode::Char('V'), true) if ctrl => {
                self.paste_into_attached();
                true
            }
```

CAREFUL: these two arms and the scroll arms borrow-conflict with the `let Some(s) = self.sessions.get_mut(...)` taken earlier in Task 3's version of `handle_ux_key`. Restructure the function: match FIRST on the chord (pure), then act — e.g. compute a small local enum `enum Chord { PageUp, PageDown, Top, Bottom, Copy, Paste }` from `(key.code, shift, ctrl)`, `return false` when none matched, then perform the action in a second match where each arm does its own session lookup. The resulting shape:

```rust
    fn handle_ux_key(&mut self, key: &KeyEvent) -> bool {
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        enum Chord { PageUp, PageDown, Top, Bottom, Copy, Paste }
        let chord = match (key.code, shift, ctrl) {
            (KeyCode::PageUp, true, _) => Chord::PageUp,
            (KeyCode::PageDown, true, _) => Chord::PageDown,
            (KeyCode::Home, true, _) => Chord::Top,
            (KeyCode::End, true, _) => Chord::Bottom,
            (KeyCode::Char('c') | KeyCode::Char('C'), true, true) => Chord::Copy,
            (KeyCode::Char('v') | KeyCode::Char('V'), true, true) => Chord::Paste,
            _ => return false,
        };
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        match chord {
            Chord::PageUp => self.scroll_selected(page),
            Chord::PageDown => self.scroll_selected(-page),
            Chord::Top => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_top();
                }
            }
            Chord::Bottom => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            Chord::Copy => self.copy_selection(),
            Chord::Paste => self.paste_into_attached(),
        }
        true
    }

    fn scroll_selected(&mut self, delta: i32) {
        if let Some(s) = self.sessions.get_mut(self.selected) {
            s.scroll_by(delta);
        }
    }
```

(This REPLACES Task 3's simpler `handle_ux_key` body — if you are implementing this task, Task 3's version is already in the file; refactor it to this shape.)

`handle_mouse`'s non-wheel arm becomes the selection lifecycle. Replace the `_ => { ... }` arm with:

```rust
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left) => {
                // the agent owns the mouse when attached + it asked for
                // events, unless Shift forces local selection (iTerm2 rule)
                let agent_owns =
                    attached && !shift && mouse_mode != vt100::MouseProtocolMode::None;
                if agent_owns {
                    if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                        self.forward_bytes(&bytes);
                    }
                    return;
                }
                let Some(s) = self.sessions.get(self.selected) else {
                    return;
                };
                let (len, offset) = s.scroll_view();
                let session_id = s.id;
                let pos = Pos {
                    row: selection::abs_row(len, offset, lrow),
                    col: lcol,
                };
                match ev.kind {
                    MouseEventKind::Down(_) => {
                        self.selection = Some(ActiveSelection {
                            session_id,
                            sel: Selection::new(pos),
                            dragging: true,
                        });
                    }
                    MouseEventKind::Drag(_) => {
                        if let Some(a) = self.selection.as_mut().filter(|a| a.dragging) {
                            a.sel.head = pos;
                        }
                    }
                    _ => {
                        // release: finish the drag; copy-on-select or clear
                        let finished = self.selection.take_if(|a| a.dragging);
                        if let Some(mut a) = finished {
                            a.dragging = false;
                            if a.sel.is_empty() {
                                // plain click: selection stays cleared
                            } else {
                                self.selection = Some(a);
                                self.copy_selection();
                            }
                        }
                    }
                }
            }
            _ => {
                // other buttons: forward when the agent owns the mouse
                if attached && !shift
                    && let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc)
                {
                    self.forward_bytes(&bytes);
                }
            }
```

(`Option::take_if` is stable; if the toolchain disagrees, `self.selection.take()` + re-insert achieves the same.) Also add `self.selection = None;` to the `MoveUp`, `MoveDown`, `RemoveSelected`, and `RespawnSelected` arms of `apply` — switching or replacing the displayed session drops the selection (and `displayed_selection`'s id check covers any path that misses).

`src/ui.rs` — the post-pass + wiring in `draw_main`:

```rust
/// Applies REVERSED to every cell of `inner` whose grid-absolute position
/// falls inside the selection. Pure over the buffer; testable headlessly.
pub fn apply_selection_highlight(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    sel: &crate::selection::Selection,
    scrollback_len: usize,
    offset: usize,
) {
    for v in 0..inner.height {
        let row = crate::selection::abs_row(scrollback_len, offset, v);
        for c in 0..inner.width {
            if sel.contains(row, c) {
                let cell = &mut buf[(inner.x + c, inner.y + v)];
                let style = cell.style().add_modifier(Modifier::REVERSED);
                cell.set_style(style);
            }
        }
    }
}
```

In `draw_main`, after the `f.render_widget(PseudoTerminal::new(screen), inner);` line:

```rust
    if let Some(sel) = app.displayed_selection() {
        let (len, offset) = session.scroll_view();
        apply_selection_highlight(f.buffer_mut(), inner, sel, len, offset);
    }
```

(Borrow note: `screen` is borrowed from `session.parser`; end that borrow before `f.buffer_mut()` by scoping — take `cursor_position`/`hide_cursor` values out before the highlight block, or restructure so `screen` isn't referenced after. The compiler will tell you; keep behavior identical.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test scroll_ux && cargo test`
Expected: new tests green (the `#[ignore]` clipboard test stays ignored; run it once manually with `cargo test --test scroll_ux -- --ignored` and report the result). Full suite green, zero warnings.

- [ ] **Step 6: Commit**

```powershell
git add -A
git commit -m "feat: click-drag selection with copy-on-select and bracketed paste"
```

---

### Task 6: Search engine (`search.rs`)

**Files:**
- Create: `src/search.rs`
- Modify: `src/lib.rs` (add `pub mod search;`)

**Interfaces:**
- Consumes: `selection::row_cells` (Task 4).
- Produces (Task 7 relies on these exact signatures):
  - `search::Match { pub row: usize, pub col_start: u16, pub col_end: u16 }` — grid-absolute row, inclusive column range (`Copy, Debug, PartialEq, Eq`)
  - `search::SearchState { pub query: String, pub matches: Vec<Match>, pub current: usize }` with `new()`, `run<CB: vt100::Callbacks>(&mut self, parser: &mut vt100::Parser<CB>, scrollback_len: usize)`, `next(&mut self)`, `prev(&mut self)`, `current_match(&self) -> Option<&Match>`
- Semantics (normative): case-insensitive substring, all rows `0..scrollback_len + screen_rows`; matches sorted ascending by (row, col); after `run`, `current` = last index (nearest the live bottom); `next()` walks UP through history (older; wraps from oldest to newest), `prev()` walks back down — terminal search convention: you search for something that just scrolled away.

- [ ] **Step 1: Write failing tests**

In `src/search.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 5x20 parser fed rows row-00..row-11 (scrollback_len = 7).
    fn parser_with_lines() -> (vt100::Parser, usize) {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..12 {
            parser.process(format!("row-{i:02}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(0);
        (parser, len)
    }

    #[test]
    fn finds_matches_across_scrollback_and_screen() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "row-0".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 10); // row-00 .. row-09
        assert_eq!(st.matches[0].row, 0);
        assert_eq!(st.matches[9].row, 9);
        // current starts nearest the bottom
        assert_eq!(st.current, 9);
    }

    #[test]
    fn match_columns_are_cell_positions() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "w-03".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 1);
        let m = &st.matches[0];
        assert_eq!((m.row, m.col_start, m.col_end), (3, 2, 5));
    }

    #[test]
    fn case_insensitive() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "ROW-05".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 1);
        assert_eq!(st.matches[0].row, 5);
    }

    #[test]
    fn navigation_walks_history_and_wraps() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "row-0".into();
        st.run(&mut parser, len);
        assert_eq!(st.current_match().unwrap().row, 9);
        st.next(); // older
        assert_eq!(st.current_match().unwrap().row, 8);
        st.prev(); // newer
        assert_eq!(st.current_match().unwrap().row, 9);
        st.prev(); // newest wraps to oldest? no: prev from last wraps to first
        assert_eq!(st.current_match().unwrap().row, 0);
        st.next(); // older than oldest wraps to newest
        assert_eq!(st.current_match().unwrap().row, 9);
    }

    #[test]
    fn empty_query_and_no_match_are_harmless() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.run(&mut parser, len);
        assert!(st.matches.is_empty());
        assert!(st.current_match().is_none());
        st.next();
        st.prev(); // no panic on empty
        st.query = "zebra".into();
        st.run(&mut parser, len);
        assert!(st.matches.is_empty());
    }

    #[test]
    fn run_restores_scroll_offset() {
        let (mut parser, len) = parser_with_lines();
        parser.screen_mut().set_scrollback(4);
        let mut st = SearchState::new();
        st.query = "row".into();
        st.run(&mut parser, len);
        assert_eq!(parser.screen().scrollback(), 4);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib search`
Expected: compile error (module not defined).

- [ ] **Step 3: Implement `src/search.rs`**

```rust
//! Incremental, case-insensitive substring search over screen + scrollback.

use crate::selection::row_cells;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Grid-absolute row (same coordinate system as selection.rs).
    pub row: usize,
    /// Inclusive cell-column range of the match on that row.
    pub col_start: u16,
    pub col_end: u16,
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String,
    /// Ascending by (row, col_start).
    pub matches: Vec<Match>,
    /// Index into `matches`; starts at the last (nearest live) match.
    pub current: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute matches for the current query. Restores the parser's
    /// scroll offset (row_cells does per row). Resets `current` to the
    /// match nearest the live bottom.
    pub fn run<CB: vt100::Callbacks>(
        &mut self,
        parser: &mut vt100::Parser<CB>,
        scrollback_len: usize,
    ) {
        self.matches.clear();
        let needle = self.query.to_lowercase();
        if needle.is_empty() {
            self.current = 0;
            return;
        }
        let screen_rows = usize::from(parser.screen().size().0);
        for row in 0..scrollback_len + screen_rows {
            let cells = row_cells(parser, scrollback_len, row);
            // build the row text alongside a char-index -> cell-col map
            let mut text = String::new();
            let mut col_of_char: Vec<u16> = Vec::new();
            for (col, contents) in &cells {
                let piece = if contents.is_empty() { " " } else { contents };
                for _ in piece.chars() {
                    col_of_char.push(*col);
                }
                text.push_str(piece);
            }
            let hay = text.to_lowercase();
            let mut from = 0;
            while let Some(found) = hay[from..].find(&needle) {
                let start = from + found;
                let end = start + needle.len(); // byte len of lowercase needle
                // map byte offsets to char indices for the col map
                let start_char = hay[..start].chars().count();
                let end_char = start_char + needle.chars().count();
                if let (Some(&cs), Some(&ce)) =
                    (col_of_char.get(start_char), col_of_char.get(end_char.saturating_sub(1)))
                {
                    self.matches.push(Match { row, col_start: cs, col_end: ce });
                }
                from = end.max(start + 1);
            }
        }
        self.current = self.matches.len().saturating_sub(1);
    }

    /// Step to the next OLDER match (upward through history), wrapping.
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
    }

    /// Step back toward newer matches, wrapping.
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.matches.len();
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.matches.get(self.current)
    }
}
```

Add `pub mod search;` to `src/lib.rs`.

Unicode note for the implementer: the byte-offset → char-index mapping above is correct for the test corpus and normal terminal text; lowercasing can change char counts for exotic scripts (e.g. 'İ'). That edge is out of scope — matching stays approximate there, and must simply not panic (`get(...)` guards it).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib search`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: incremental case-insensitive search over scrollback"
```

---

### Task 7: Search bar wiring + highlights (`app.rs`, `ui.rs`)

**Files:**
- Modify: `src/app.rs`, `src/ui.rs`
- Test: inline ui tests + `tests/scroll_ux.rs` (extend)

**Interfaces:**
- Consumes: Task 6's `SearchState`/`Match`, Task 4's `abs_row`, Task 3's `handle_ux_key` (add the open-chord arms), Task 1's `set_scroll`/`scroll_view`/`scroll_to_bottom`.
- Produces:
  - `App.search: Option<SearchState>` (pub field)
  - `ui::apply_search_highlight(buf: &mut Buffer, inner: Rect, matches: &[Match], current: usize, scrollback_len: usize, offset: usize)` — all matches `bg Yellow, fg Black`; the current match `bg White, fg Black, BOLD`
- Behavior contract: while the bar is open, ALL keys go to it (no v1 dispatch, no forwarding); Ctrl+Shift+F opens in Control and Attached; plain Ctrl+F opens in Control only (in Attached it still forwards `0x06`); typing/Backspace rerun the search and jump the view to the current match; Enter → older match, Shift+Enter → newer, view follows; Esc closes and snaps to live; new output on the displayed session reruns the search while open.

- [ ] **Step 1: Write failing tests**

In `src/ui.rs` tests:

```rust
    #[test]
    fn search_highlight_styles_match_cells() {
        use crate::search::Match;
        use ratatui::buffer::Buffer;
        let area = Rect::new(2, 1, 10, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        let matches = vec![
            Match { row: 5, col_start: 1, col_end: 3 },
            Match { row: 6, col_start: 0, col_end: 2 },
        ];
        // len=6, offset=2: abs 5 -> visual 1, abs 6 -> visual 2
        apply_search_highlight(&mut buf, area, &matches, 1, 6, 2);
        // non-current match: yellow bg
        let cell = &buf[(2 + 1, 1 + 1)];
        assert_eq!(cell.style().bg, Some(Color::Yellow));
        // current match (index 1): white bg + bold
        let cell = &buf[(2, 1 + 2)];
        assert_eq!(cell.style().bg, Some(Color::White));
        assert!(cell.style().add_modifier.contains(Modifier::BOLD));
        // outside any match: untouched
        assert_eq!(buf[(2 + 5, 1 + 1)].style().bg, None);
    }

    #[test]
    fn search_bar_renders_query_and_count() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), tx);
        let mut st = crate::search::SearchState::new();
        st.query = "hello".into();
        app.search = Some(st);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Search: hello"), "got: {text}");
        assert!(text.contains("no matches"), "empty result indicator: {text}");
    }
```

Add to `tests/scroll_ux.rs`:

```rust
fn ctrl_shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[tokio::test]
async fn search_finds_and_navigates_history() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_key(&ctrl_shift('f'), Instant::now());
    assert!(app.search.is_some());
    for c in "line-3".chars() {
        app.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    let st = app.search.as_ref().unwrap();
    assert_eq!(st.query, "line-3");
    // line-3, line-30..line-39 = 11 matches
    assert_eq!(st.matches.len(), 11);
    // current = nearest bottom (line-39); the view jumped so it's visible
    assert!(
        app.sessions[0].parser.screen().contents().lines().any(|l| l.trim() == "line-39"),
        "view did not scroll to current match"
    );
    // Enter walks up through history
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        app.sessions[0].parser.screen().contents().lines().any(|l| l.trim() == "line-38")
    );
    // Shift+Enter walks back down
    app.handle_key(&shift_key(KeyCode::Enter), Instant::now());
    let st = app.search.as_ref().unwrap();
    assert_eq!(st.current, st.matches.len() - 1);
    // Esc closes and snaps to live
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(app.search.is_none());
    assert_eq!(app.sessions[0].scrolled(), 0);
}

#[tokio::test]
async fn open_search_bar_consumes_all_keys() {
    let (mut app, _rx) = app_with_history(100).await;
    app.handle_key(&ctrl_shift('f'), Instant::now());
    app.handle_key(&key(KeyCode::Char('q')), Instant::now());
    assert!(!app.should_quit, "q must edit the query, not quit");
    assert_eq!(app.search.as_ref().unwrap().query, "q");
    app.handle_key(&key(KeyCode::Backspace), Instant::now());
    assert_eq!(app.search.as_ref().unwrap().query, "");
}

#[tokio::test]
async fn plain_ctrl_f_opens_only_in_control_mode() {
    let (mut app, _rx) = app_with_history(100).await;
    // Control mode: plain Ctrl+F opens
    app.handle_key(&ctrl('f'), Instant::now());
    assert!(app.search.is_some());
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    // Attached: plain Ctrl+F is the agent's key (forwarded), bar stays shut
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // attach (session is Exited; Enter still attaches)
    if matches!(app.mode, Mode::Attached) {
        app.handle_key(&ctrl('f'), Instant::now());
        assert!(app.search.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test scroll_ux search && cargo test --lib ui`
Expected: compile error (`App.search`, `apply_search_highlight` not defined).

- [ ] **Step 3: Implement**

`src/app.rs` — imports gain `use crate::search::SearchState;`. New pub field `pub search: Option<SearchState>` (init `None`). Changes:

1. `handle_key` routing order becomes (replacing the Task 3 insertion):

```rust
        self.error = None;
        if self.search.is_some() {
            self.handle_search_key(key);
            return;
        }
        if self.handle_ux_key(key) {
            self.just_detached = false;
            return;
        }
```

2. `handle_ux_key`'s chord enum gains `OpenSearch`; the chord match gains:

```rust
            (KeyCode::Char('f') | KeyCode::Char('F'), true, true) => Chord::OpenSearch,
            // plain Ctrl+F only when nothing is forwarded (Control mode)
            (KeyCode::Char('f') | KeyCode::Char('F'), false, true)
                if matches!(self.mode, Mode::Control) =>
            {
                Chord::OpenSearch
            }
```

and the action match gains:

```rust
            Chord::OpenSearch => {
                self.search = Some(SearchState::new());
            }
```

3. New methods:

```rust
    fn handle_search_key(&mut self, key: &KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => {
                self.search = None;
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            KeyCode::Enter => {
                if let Some(st) = self.search.as_mut() {
                    if shift {
                        st.prev();
                    } else {
                        st.next();
                    }
                }
                self.scroll_to_current_match();
            }
            KeyCode::Backspace => {
                if let Some(st) = self.search.as_mut() {
                    st.query.pop();
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            KeyCode::Char(c) => {
                if let Some(st) = self.search.as_mut() {
                    st.query.push(c);
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            _ => {}
        }
    }

    fn rerun_search(&mut self) {
        let Some(st) = self.search.as_mut() else { return };
        let Some(s) = self.sessions.get_mut(self.selected) else { return };
        let (len, _) = s.scroll_view();
        st.run(&mut s.parser, len);
    }

    fn scroll_to_current_match(&mut self) {
        let Some(row) = self
            .search
            .as_ref()
            .and_then(|st| st.current_match().map(|m| m.row))
        else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else { return };
        let (len, _) = s.scroll_view();
        // live rows need no scrolling; scrollback rows go to the top of view
        let offset = if row >= len { 0 } else { len - row };
        s.set_scroll(offset);
    }
```

4. In `handle_pty_output`, after the existing `process_output` call: rerun while the bar is open on the displayed session:

```rust
        if self.search.is_some()
            && self.sessions.get(self.selected).map(|s| s.id) == Some(id)
        {
            self.rerun_search();
        }
```

`src/ui.rs`:

```rust
/// Highlight search matches: all matches yellow, the current one white +
/// bold. Same visual-coordinate mapping as the selection pass.
pub fn apply_search_highlight(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    matches: &[crate::search::Match],
    current: usize,
    scrollback_len: usize,
    offset: usize,
) {
    for (i, m) in matches.iter().enumerate() {
        // abs = len - offset + v  =>  v = row + offset - len
        let Some(v) = (m.row + offset).checked_sub(scrollback_len) else {
            continue; // above the current view
        };
        if v >= usize::from(inner.height) {
            continue; // below the current view
        }
        let style = if i == current {
            Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Yellow).fg(Color::Black)
        };
        for c in m.col_start..=m.col_end.min(inner.width.saturating_sub(1)) {
            buf[(inner.x + c, inner.y + v as u16)].set_style(style);
        }
    }
}
```

In `draw_main`, after the selection-highlight block:

```rust
    if let Some(st) = &app.search {
        let (len, offset) = session.scroll_view();
        apply_search_highlight(f.buffer_mut(), inner, &st.matches, st.current, len, offset);
    }
```

In `draw_status_bar`, the search bar takes precedence over everything else (insert as the first branch):

```rust
    let text = if let Some(st) = &app.search {
        let count = if st.matches.is_empty() {
            if st.query.is_empty() { String::new() } else { "no matches".into() }
        } else {
            format!("{}/{}", st.current + 1, st.matches.len())
        };
        Line::raw(format!(
            "Search: {}  {count}  [Enter] next  [Shift+Enter] prev  [Esc] close",
            st.query
        ))
    } else if let Some(err) = &app.error {
        // ... existing branches unchanged ...
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test scroll_ux && cargo test`
Expected: all green, zero warnings.

- [ ] **Step 5: Commit**

```powershell
git add -A
git commit -m "feat: incremental search bar with match highlighting and navigation"
```

---

### Task 8: Final verification + manual smoke checklist

**Files:**
- Modify: nothing new — verification, lint, and the smoke list only.

**Interfaces:** none (gate task).

- [ ] **Step 1: Full verification**

Run, in order, all must be clean:

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test scroll_ux -- --ignored   # clipboard round-trip, report result
cargo build --release
```

Fix anything they flag (mechanical only; behavior changes need a finding/ruling).

- [ ] **Step 2: Keybinding conformance sweep**

Re-read the spec's keybinding table (`docs/superpowers/specs/2026-08-30-terminal-ux-design.md`) row by row against `handle_ux_key`, `handle_search_key`, `handle_mouse`, and `dispatch`. Confirm: plain Ctrl+C/V/F still forward in Attached mode (encode_key path untouched); every new chord matches the table; v1 rows unchanged. Note each row's verdict in your report.

- [ ] **Step 3: Manual smoke checklist (deferred to the human — record as pending in your report)**

With real `claude`/`codex` profiles in `profiles.toml`:
- [ ] Wheel over the agent's full-screen TUI scrolls its content (arrows route) — and Shift+Wheel scrolls agent-mux's own scrollback instead.
- [ ] After `claude` prints a long transcript, Shift+PgUp pages back; the `[SCROLL ↑ N]` tag shows; typing snaps to live.
- [ ] While scrolled, new agent output does not move the view.
- [ ] Click-drag selects text, highlight tracks the drag, release lands it on the clipboard; paste into another app to verify.
- [ ] Ctrl+Shift+V pastes a multi-line snippet into claude's input box intact (bracketed paste).
- [ ] Ctrl+Shift+F finds a string from earlier in the session; Enter/Shift+Enter walk matches; Esc returns to live.
- [ ] Plain Ctrl+F while attached still does whatever the agent binds it to (not agent-mux search).
- [ ] Quit/restart: terminal restored, no mouse-capture residue (moving the mouse after exit must not print escape codes).

- [ ] **Step 4: Commit (if fmt/clippy changed anything)**

```powershell
git add -A
git commit -m "chore: fmt/clippy pass for terminal UX feature"
```

---

## Verification checklist (spec → task)

| Spec requirement | Covered by |
|---|---|
| Scroll history via wheel + keyboard, Attached + Control preview | Tasks 1, 3 |
| Wheel routing rule (4 rules, Shift override, Control always local) | Task 2 (`route_wheel`), Task 3 (wiring) |
| Content-anchored view under new output + indicator + snap on typing | Task 1 (anchoring), Task 3 (indicator, snap) |
| Mouse forwarding per child's protocol/encoding | Task 2 (`encode_mouse`), Tasks 3/5 (wiring) |
| Click-drag selection, highlight, copy-on-select, Ctrl+Shift+C | Tasks 4, 5 |
| Ctrl+Shift+V paste, bracketed iff enabled, CR normalization | Task 5 |
| Ctrl+Shift+F / Ctrl+F-in-Control incremental search, highlights, nav | Tasks 6, 7 |
| Plain Ctrl+letters still reach the agent | Global constraint; conformance sweep Task 8 |
| Clipboard errors → status bar, never panic | Task 5 (`copy_to_clipboard`) |
| Mouse capture released on exit (guard + hook) | Task 3 (`restore_terminal`) |
| Cross-platform, cmd.exe/sh tests only | every task's tests |
| Manual real-agent smoke | Task 8 Step 3 |




