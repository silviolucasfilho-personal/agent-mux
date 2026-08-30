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
