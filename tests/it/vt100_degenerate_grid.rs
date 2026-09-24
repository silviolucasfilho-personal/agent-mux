//! A degenerate terminal must not take the process down.
//!
//! agent-mux computes its pane size from the terminal it is given. Started
//! without one -- in CI, under `script(1)`, or during a resize to zero --
//! that is a 1x1 grid, and the first line that wrapped used to panic inside
//! the vendored vt100: `cols - width` and `prev_pos.row -= scrolled` both
//! underflowed, and the row and cell lookups that follow had nothing to
//! unwrap. See the local patches in `vendor/vt100/src/{grid,screen}.rs`.
//!
//! `contents()` joins a wrapped row with the one it wrapped into, so these
//! assert on the text a reader would see, not on the row layout.

#[test]
fn a_one_by_one_grid_survives_wrapping_output() {
    let mut parser = vt100::Parser::new(1, 1, 100);
    parser.process(b"hello-world-that-wraps\r\n");
    assert_eq!(parser.screen().size(), (1, 1));
    // Everything scrolled away except the cell the cursor landed on.
    let _ = parser.screen().contents();
}

#[test]
fn a_one_column_grid_keeps_every_character() {
    let mut parser = vt100::Parser::new(4, 1, 100);
    parser.process(b"abcd");
    // Four rows of one column, all wrapped, read back as one logical line.
    assert_eq!(parser.screen().contents(), "abcd");
}

#[test]
fn a_zero_column_grid_does_not_panic() {
    let mut parser = vt100::Parser::new(1, 0, 10);
    parser.process(b"xyz\r\n");
    let _ = parser.screen().contents();
}

#[test]
fn a_glyph_wider_than_the_grid_does_not_panic() {
    // A double-width character in a one-column grid is the case where
    // `cols - width` underflowed.
    let mut parser = vt100::Parser::new(2, 1, 10);
    parser.process("\u{4e16}\u{754c}".as_bytes());
    let _ = parser.screen().contents();
}

#[test]
fn an_ordinary_grid_still_wraps_and_still_breaks_lines() {
    // The guards must not change behaviour where there is room to wrap.
    let mut parser = vt100::Parser::new(3, 4, 100);
    parser.process(b"abcdef");
    assert_eq!(parser.screen().contents(), "abcdef");

    let mut parser = vt100::Parser::new(3, 4, 100);
    parser.process(b"ab\r\ncd");
    // An explicit newline is a real line break, not a wrap.
    assert_eq!(parser.screen().contents(), "ab\ncd");
}

#[test]
fn the_agent_mux_pane_that_used_to_crash_the_tui() {
    // The exact shape the TUI produced with no terminal size.
    let mut parser = vt100::Parser::new(1, 1, 1000);
    for chunk in [&b"$ "[..], b"echo hello", b"\r\n", b"hello\r\n"] {
        parser.process(chunk);
    }
    let _ = parser.screen().state_formatted();
}
