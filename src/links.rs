//! Links in a session's pane: what is under the pointer, and opening it.
//!
//! agent-mux captures the mouse, so the outer terminal never sees a click
//! on a harness's link; the pane opens it itself. A cell written inside an
//! OSC 8 hyperlink (Claude Code and agy write them; `FORCE_HYPERLINK=1` is
//! set for every child, see `Session::spawn`) links to its target, whatever
//! its text. Anything else is read as plain text: a URL in the logical line
//! under the pointer, joined across the rows the child soft-wrapped (Codex
//! prints plain URLs).

use crate::selection::{self, Pos};

/// Schemes a click opens. Anything else (`javascript:`, a custom handler)
/// stays inert: output a child prints must not launch arbitrary handlers.
const SCHEMES: &[&str] = &["http://", "https://", "file://", "mailto:"];

/// Plain-text URLs start with one of these.
const TEXT_PREFIXES: &[&str] = &["https://", "http://", "file://"];

/// The link at a grid-absolute position (see `selection` for the
/// coordinates), if any.
pub fn link_at<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    pos: Pos,
) -> Option<String> {
    if let Some(uri) = with_row(parser, scrollback_len, pos.row, |screen, visual| {
        screen.hyperlink(visual, pos.col).map(str::to_string)
    })
    .flatten()
    {
        return openable(&uri).then_some(uri);
    }
    text_link_at(parser, scrollback_len, pos)
}

/// Whether a click may open `uri`.
pub fn openable(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    SCHEMES
        .iter()
        .any(|s| lower.len() > s.len() && lower.starts_with(s))
}

/// Opens `uri` with the platform's handler, detached.
pub fn open(uri: &str) -> std::io::Result<()> {
    if !openable(uri) {
        return Err(std::io::Error::other(format!(
            "not a link agent-mux opens: {uri}"
        )));
    }
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Runs `f` with the screen scrolled so grid-absolute `row` is visible,
/// and its visual row; restores the scroll. None below the live screen.
fn with_row<CB: vt100::Callbacks, T>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    row: usize,
    f: impl FnOnce(&vt100::Screen, u16) -> T,
) -> Option<T> {
    let (screen_rows, _) = parser.screen().size();
    let (offset, visual) = if row >= scrollback_len {
        let v = row - scrollback_len;
        if v >= usize::from(screen_rows) {
            return None;
        }
        (0, v as u16)
    } else {
        (scrollback_len - row, 0u16)
    };
    let saved = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(offset);
    let out = f(parser.screen(), visual);
    parser.screen_mut().set_scrollback(saved);
    Some(out)
}

fn row_wrapped<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    row: usize,
) -> bool {
    with_row(parser, scrollback_len, row, |screen, visual| {
        screen.row_wrapped(visual)
    })
    .unwrap_or(false)
}

/// A plain-text URL covering `pos` in its logical (soft-wrapped) line.
fn text_link_at<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    pos: Pos,
) -> Option<String> {
    // a logical line never reaches further than this either way
    const MAX_ROWS: usize = 16;
    let mut first = pos.row;
    while first > 0 && pos.row - first < MAX_ROWS && row_wrapped(parser, scrollback_len, first - 1)
    {
        first -= 1;
    }
    let mut last = pos.row;
    while last - pos.row < MAX_ROWS && row_wrapped(parser, scrollback_len, last) {
        last += 1;
    }
    // the line's text, and the char index each cell starts at
    let mut text: Vec<char> = Vec::new();
    let mut at = None;
    for row in first..=last {
        for (col, contents) in selection::row_cells(parser, scrollback_len, row) {
            if row == pos.row && col == pos.col {
                at = Some(text.len());
            }
            if contents.is_empty() {
                text.push(' ');
            } else {
                text.extend(contents.chars());
            }
        }
        // the pointer on a wide glyph's second half
        if row == pos.row && at.is_none() && pos.col > 0 {
            at = text.len().checked_sub(1);
        }
    }
    let at = at?;
    let (start, end) = url_span(&text, at)?;
    Some(text[start..end].iter().collect())
}

/// The URL in `text` covering index `at`: [start, end).
fn url_span(text: &[char], at: usize) -> Option<(usize, usize)> {
    let stop = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '│');
    if text.get(at).is_none_or(|&c| stop(c)) {
        return None;
    }
    let mut start = at;
    while start > 0 && !stop(text[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < text.len() && !stop(text[end]) {
        end += 1;
    }
    // the token may carry text before the URL: `(see https://…`, `x=https://…`
    let token: String = text[start..end].iter().collect();
    let lower = token.to_ascii_lowercase();
    let offset = TEXT_PREFIXES.iter().filter_map(|p| lower.find(p)).min()?;
    let start = start + token[..offset].chars().count();
    if at < start {
        return None;
    }
    let end = trim_end(&text[start..end]) + start;
    (at < end && end - start > 8).then_some((start, end))
}

/// Length of `url` without the punctuation that ends a sentence or closes
/// a bracket the URL did not open.
fn trim_end(url: &[char]) -> usize {
    let mut end = url.len();
    while end > 0 {
        let c = url[end - 1];
        let unbalanced = |open: char, close: char| {
            c == close
                && url[..end].iter().filter(|&&x| x == close).count()
                    > url[..end].iter().filter(|&&x| x == open).count()
        };
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(bytes: &[u8]) -> vt100::Parser {
        let mut p = vt100::Parser::new(5, 20, 100);
        p.process(bytes);
        p
    }

    fn scrollback_len(p: &mut vt100::Parser) -> usize {
        let cur = p.screen().scrollback();
        p.screen_mut().set_scrollback(usize::MAX);
        let len = p.screen().scrollback();
        p.screen_mut().set_scrollback(cur);
        len
    }

    fn at(p: &mut vt100::Parser, row: usize, col: u16) -> Option<String> {
        let len = scrollback_len(p);
        link_at(
            p,
            len,
            Pos {
                row: len + row,
                col,
            },
        )
    }

    #[test]
    fn an_osc8_link_opens_its_target_whatever_its_text() {
        let mut p = parser(b"see \x1b]8;;https://example.com/a;b\x1b\\docs\x1b]8;;\x1b\\ here");
        assert_eq!(at(&mut p, 0, 4).as_deref(), Some("https://example.com/a;b"));
        assert_eq!(at(&mut p, 0, 7).as_deref(), Some("https://example.com/a;b"));
        assert_eq!(at(&mut p, 0, 8), None, "the space after the link");
        assert_eq!(at(&mut p, 0, 1), None);
    }

    #[test]
    fn an_osc8_link_survives_sgr_resets_and_scrolling_into_scrollback() {
        let mut p = parser(b"\x1b]8;id=1;https://a.io/x\x07\x1b[1mab\x1b[0mcd\x1b]8;;\x07\r\n");
        p.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
        let len = scrollback_len(&mut p);
        assert!(len > 0);
        let first = len - 2; // "abcd" has scrolled off the top
        assert_eq!(
            link_at(&mut p, len, Pos { row: first, col: 3 }).as_deref(),
            Some("https://a.io/x")
        );
    }

    #[test]
    fn a_link_to_another_scheme_stays_inert() {
        let mut p = parser(b"\x1b]8;;javascript:alert(1)\x1b\\click\x1b]8;;\x1b\\");
        assert_eq!(at(&mut p, 0, 1), None);
    }

    #[test]
    fn a_plain_url_is_found_under_the_pointer_and_trimmed() {
        let mut p = parser(b"go (https://x.io/a).");
        assert_eq!(at(&mut p, 0, 10).as_deref(), Some("https://x.io/a"));
        assert_eq!(at(&mut p, 0, 4).as_deref(), Some("https://x.io/a"));
        assert_eq!(at(&mut p, 0, 3), None, "the bracket before it");
        assert_eq!(at(&mut p, 0, 0), None);
    }

    #[test]
    fn a_plain_url_soft_wrapped_over_rows_is_joined() {
        // 20 columns: the URL runs on into the next row
        let mut p = parser(b"> https://example.com/a/very/long/path ok");
        assert_eq!(
            at(&mut p, 1, 2).as_deref(),
            Some("https://example.com/a/very/long/path")
        );
        assert_eq!(
            at(&mut p, 0, 5).as_deref(),
            Some("https://example.com/a/very/long/path")
        );
    }

    #[test]
    fn balanced_brackets_stay_in_the_url() {
        let text: Vec<char> = "https://en.wikipedia.org/wiki/Rust_(lang)"
            .chars()
            .collect();
        assert_eq!(url_span(&text, 3), Some((0, text.len())));
    }

    #[test]
    fn only_web_file_and_mail_links_open() {
        assert!(openable("https://example.com"));
        assert!(openable("HTTP://example.com"));
        assert!(openable("file:///tmp/x.md"));
        assert!(openable("mailto:a@b.c"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("vscode://open"));
        assert!(!openable("https://"));
        assert!(open("javascript:alert(1)").is_err());
    }
}
