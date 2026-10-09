//! Drawing the Markdown viewer and editor (`crate::app::markdown_view`):
//! a title row, the rendered document or the source (with its live preview
//! beside it on a wide screen), and a footer of keys. Used full-screen by
//! the App and by `agent-mux md`.

use super::flow::{dim, head, hints};
use crate::app::markdown_view::{MarkdownView, Pane, render_width};
use crate::markdown;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Side by side from this many columns; the editor alone below it.
const SPLIT_MIN_WIDTH: u16 = 100;

pub fn draw(f: &mut Frame, area: Rect, v: &MarkdownView) {
    f.render_widget(Clear, area);
    let [top, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let split = v.pane == Pane::Edit && v.preview && body.width >= SPLIT_MIN_WIDTH;
    let position = match v.pane {
        Pane::View => draw_document(f, body, v, v.scroll),
        Pane::Edit if split => {
            let [left, sep, right] = Layout::horizontal([
                Constraint::Percentage(50),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(body);
            let (position, cursor_line, cursor_row) = draw_editor(f, left, v);
            let sep_lines = vec![Line::styled("│", dim()); sep.height as usize];
            f.render_widget(Paragraph::new(sep_lines), sep);
            // the preview keeps the cursor's block level with the cursor
            let doc = v.doc(render_width(document_area(right)));
            let target = doc.line_for_source(cursor_line);
            let max = doc.lines.len().saturating_sub(right.height as usize);
            draw_document(f, right, v, target.saturating_sub(cursor_row).min(max));
            position
        }
        Pane::Edit => draw_editor(f, body, v).0,
    };
    draw_title(f, top, v, &position);
    draw_footer(f, foot, v);
}

/// The text area of a document drawn in `area`: a column of margin on
/// the left, the scrollbar on the right.
fn document_area(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    )
}

/// Draws the rendered document from line `top`; returns where the view
/// is, for the title.
fn draw_document(f: &mut Frame, area: Rect, v: &MarkdownView, top: usize) -> String {
    let text_area = document_area(area);
    v.view_area.set(text_area);
    let doc = v.doc(render_width(text_area));
    let h = text_area.height as usize;
    let top = top.min(doc.lines.len().saturating_sub(1));
    if v.pane == Pane::Edit {
        v.preview_top.set(top);
    }
    let end = (top + h).min(doc.lines.len());
    let cols = text_area.width as usize;
    let hscroll = v.hscroll.min(doc.max_hscroll(cols));
    let lines: Vec<Line> = (top..end)
        .map(|i| match doc.wide_at(i) {
            Some(w) => slide(&doc.lines[i], w.prefix, hscroll, cols),
            None => doc.lines[i].clone(),
        })
        .collect();
    let lines = if lines.is_empty() && v.text.text.trim().is_empty() {
        vec![Line::styled("(empty: e to write)", dim())]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(lines), text_area);
    if doc.lines.len() > h {
        let mut state = ScrollbarState::new(doc.lines.len().saturating_sub(h)).position(top);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(Color::Gray))
                .track_style(dim()),
            area,
            &mut state,
        );
    }
    if doc.lines.len() <= h {
        "all".to_string()
    } else {
        format!("{}%", (end * 100 / doc.lines.len().max(1)).min(100))
    }
}

/// A row of a wide block slid `hscroll` columns left past its first
/// `prefix` columns, cut to `cols`. A `‹` or `›` at an edge says there is
/// more of the row that way.
fn slide(line: &Line<'static>, prefix: usize, hscroll: usize, cols: usize) -> Line<'static> {
    // one cell per column; the second column of a wide character is None
    let mut cells: Vec<Option<(char, Style)>> = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if w == 0 {
                continue;
            }
            cells.push(Some((c, style)));
            cells.extend(std::iter::repeat_n(None, w - 1));
        }
    }
    let prefix = prefix.min(cells.len());
    let from = (prefix + hscroll).min(cells.len());
    let blank = |c: &Option<(char, Style)>| c.is_none_or(|(c, _)| c == ' ');
    let left_cut = cells[prefix..from].iter().any(|c| !blank(c));
    let mut out: Vec<Option<(char, Style)>> = cells[..prefix].to_vec();
    out.extend_from_slice(&cells[from..]);
    let right_cut = out.len() > cols && out[cols..].iter().any(|c| !blank(c));
    out.truncate(cols);
    // half a wide character at either edge of the slid part is a space
    if out.len() > prefix && from < cells.len() && out[prefix].is_none() {
        out[prefix] = Some((' ', Style::default()));
    }
    if let Some(last) = out.len().checked_sub(1)
        && last >= prefix
        && out[last].is_some_and(|(c, _)| c.width().unwrap_or(1) > 1)
    {
        out[last] = Some((' ', Style::default()));
    }
    let marker = Style::default().fg(Color::Yellow);
    if left_cut && out.len() > prefix {
        out[prefix] = Some(('‹', marker));
    }
    if right_cut && let Some(last) = out.len().checked_sub(1) {
        if out[last].is_none() && last > prefix {
            // over the second half of a wide character: drop its first
            out[last - 1] = Some((' ', Style::default()));
        }
        out[last] = Some(('›', marker));
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut covered = 0; // columns the last wide character still covers
    for cell in out {
        let (c, style) = match cell {
            None if covered > 0 => {
                covered -= 1;
                continue;
            }
            None => (' ', Style::default()),
            Some((c, style)) => {
                covered = c.width().unwrap_or(1).saturating_sub(1);
                (c, style)
            }
        };
        match spans.last_mut() {
            Some(s) if s.style == style => s.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    Line::from(spans)
}

/// What a source line is, for colouring the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Plain,
    Heading(u8),
    Fence,
    Code,
    Quote,
    FrontMatter,
}

fn source_kinds(text: &str) -> Vec<SourceKind> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    let mut front = text.starts_with("---\n") || text == "---";
    for (i, line) in text.split('\n').enumerate() {
        let t = line.trim_start();
        let kind = if front {
            if i > 0 && (line == "---" || line == "...") {
                front = false;
            }
            SourceKind::FrontMatter
        } else if let Some(f) = &fence {
            if t.starts_with(f.as_str())
                && t.trim_start_matches(f.chars().next().unwrap_or('`'))
                    .trim()
                    .is_empty()
            {
                fence = None;
                SourceKind::Fence
            } else {
                SourceKind::Code
            }
        } else if t.starts_with("```") || t.starts_with("~~~") {
            let ch = t.chars().next().unwrap_or('`');
            let n = t.chars().take_while(|c| *c == ch).count();
            fence = Some(ch.to_string().repeat(n));
            SourceKind::Fence
        } else if t.starts_with('#') {
            let n = t.chars().take_while(|c| *c == '#').count();
            if n <= 6 && (t[n..].starts_with([' ', '\t']) || t.len() == n) {
                SourceKind::Heading(n.min(6) as u8)
            } else {
                SourceKind::Plain
            }
        } else if t.starts_with('>') {
            SourceKind::Quote
        } else {
            SourceKind::Plain
        };
        out.push(kind);
    }
    out
}

fn source_style(kind: SourceKind) -> Style {
    match kind {
        SourceKind::Plain => Style::default(),
        SourceKind::Heading(l) => markdown::heading_style(l),
        SourceKind::Fence => dim(),
        SourceKind::Code => Style::default().fg(Color::Yellow),
        SourceKind::Quote => Style::default().fg(Color::Green),
        SourceKind::FrontMatter => Style::default().fg(Color::Cyan),
    }
}

/// Draws the source with line numbers and places the cursor; returns the
/// title's position, the cursor's source line and its row on screen.
fn draw_editor(f: &mut Frame, area: Rect, v: &MarkdownView) -> (String, usize, usize) {
    let text = &v.text.text;
    let line_count = text.split('\n').count();
    let gutter = (line_count.to_string().len() + 2) as u16;
    let text_area = Rect::new(
        area.x + gutter.min(area.width),
        area.y,
        area.width.saturating_sub(gutter + 1),
        area.height,
    );
    v.edit_area.set(text_area);
    v.text.width.set(text_area.width.max(1) as usize);
    let rows = v.text.rows();
    let (cr, _) = v.text.cursor_at(&rows);
    let h = (text_area.height as usize).max(1);
    let mut top = v.edit_top.get();
    if top == usize::MAX {
        top = cr.saturating_sub(h / 3);
    }
    if cr < top {
        top = cr;
    } else if cr >= top + h {
        top = cr + 1 - h;
    }
    v.edit_top.set(top);

    let kinds = source_kinds(text);
    let bytes = text.as_bytes();
    let mut line = 0usize;
    let mut cursor_line = 0usize;
    let mut out: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let first_of_line = i == 0 || bytes.get(row.start.wrapping_sub(1)) == Some(&b'\n');
        if i > 0 && first_of_line {
            line += 1;
        }
        if i == cr {
            cursor_line = line;
        }
        if i < top {
            continue;
        }
        if i >= top + h {
            if i > cr {
                break;
            }
            continue;
        }
        let number = if first_of_line {
            format!("{:>w$} ", line + 1, w = gutter as usize - 2)
        } else {
            " ".repeat(gutter as usize - 1)
        };
        let num_style = if i == cr {
            Style::default().fg(Color::Gray)
        } else {
            dim()
        };
        out.push(Line::from(vec![
            Span::styled(number, num_style),
            Span::raw(" "),
            Span::styled(
                text[row.start..row.end].trim_end_matches('\n').to_string(),
                source_style(kinds.get(line).copied().unwrap_or(SourceKind::Plain)),
            ),
        ]));
    }
    f.render_widget(
        Paragraph::new(out),
        Rect::new(area.x, area.y, area.width, area.height),
    );
    let (_, col_chars) = v.text.cursor_at(&rows);
    if let Some(row) = rows.get(cr)
        && !v.confirm_close
    {
        let before = &text[row.start..v.text.cursor().clamp(row.start, row.end)];
        let x = text_area.x + (before.width() as u16).min(text_area.width);
        let y = text_area.y + (cr - top) as u16;
        f.set_cursor_position((x, y));
    }
    let col = col_chars + 1;
    let column = text[..v.text.cursor()]
        .rsplit('\n')
        .next()
        .map(|l| l.chars().count() + 1)
        .unwrap_or(col);
    (
        format!("Ln {}, Col {column}", cursor_line + 1),
        cursor_line,
        cr - top,
    )
}

fn shown_path(path: &std::path::Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = path.strip_prefix(&cwd)
    {
        return rel.display().to_string();
    }
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

fn draw_title(f: &mut Frame, area: Rect, v: &MarkdownView, position: &str) {
    let mut after = Vec::new();
    if v.dirty() {
        after.push(Span::styled(
            " ● modified",
            Style::default().fg(Color::Yellow),
        ));
    }
    after.push(Span::styled(
        match v.pane {
            Pane::View => "  · view",
            Pane::Edit => "  · edit",
        },
        dim(),
    ));
    let right = format!("  {position} ");
    let lead = " Markdown  ";
    let fixed = lead.width() + right.width() + after.iter().map(|s| s.width()).sum::<usize>();
    // the path gives way first, from the left: its file name matters most
    let path = tail(
        &shown_path(&v.path),
        (area.width as usize).saturating_sub(fixed),
    );
    let mut spans = vec![
        Span::styled(lead, head()),
        Span::styled(path, Style::default().add_modifier(Modifier::BOLD)),
    ];
    spans.extend(after);
    if v.pane == Pane::View
        && let Some(h) = v.current_heading()
    {
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let room = (area.width as usize).saturating_sub(used + right.width());
        if room > 6 {
            spans.push(Span::styled(tail(&format!("  § {h}"), room), dim()));
        }
    }
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let room = (area.width as usize).saturating_sub(used + right.width());
    spans.push(Span::raw(" ".repeat(room)));
    spans.push(Span::styled(right, dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The end of `s` that fits in `width` columns, with `…` when cut.
fn tail(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 1;
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        w += cw;
        out.push(c);
    }
    if width == 0 {
        return String::new();
    }
    std::iter::once('…').chain(out.into_iter().rev()).collect()
}

fn draw_footer(f: &mut Frame, area: Rect, v: &MarkdownView) {
    let line = if v.confirm_close {
        let mut l = hints(&[
            ("s", "save and close"),
            ("d", "discard"),
            ("Esc", "keep editing"),
        ]);
        l.spans.insert(
            1,
            Span::styled(
                "Unsaved changes.  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        );
        l
    } else if let Some(n) = &v.notice {
        Line::styled(format!(" {n}"), Style::default().fg(Color::Yellow))
    } else {
        match v.pane {
            Pane::View => hints(&[
                ("j/k", "scroll"),
                ("[ ]", "heading"),
                ("←→", "sideways"),
                ("e", "edit"),
                ("s", "save"),
                ("⌃O", "$EDITOR"),
                ("q", "back"),
            ]),
            Pane::Edit => hints(&[
                ("Esc", "view"),
                ("⌃S", "save"),
                ("⌃Z/⌃Y", "undo/redo"),
                ("⌃P", "preview"),
                ("Tab/⇧Tab", "indent"),
                ("⌃O", "$EDITOR"),
            ]),
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(v: &MarkdownView, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, f.area(), v)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut s = String::new();
                for x in 0..w {
                    s.push_str(buf[(x, y)].symbol());
                }
                s.trim_end().to_string()
            })
            .collect()
    }

    fn open(text: &str) -> (tempfile::TempDir, MarkdownView) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, text).unwrap();
        let v = MarkdownView::open(&path).unwrap();
        (dir, v)
    }

    #[test]
    fn the_view_shows_the_rendered_document_with_its_keys() {
        let (_d, v) = open("# Title\n\nSome *text* here.\n");
        let s = screen(&v, 60, 8);
        assert!(s[0].starts_with(" Markdown  "), "{s:?}");
        assert!(s[0].contains("doc.md") && s[0].ends_with("all"), "{s:?}");
        assert_eq!(s[1], " Title");
        assert_eq!(s[2], " ━━━━━");
        assert_eq!(s[4], " Some text here.");
        assert!(s[7].contains("edit"), "{s:?}");
    }

    #[test]
    fn only_a_block_wider_than_the_view_slides_sideways() {
        let digits = "0123456789".repeat(8);
        let (_d, mut v) = open(&format!("Some prose.\n\n```\n{digits}\nshort\n```\n"));
        let s = screen(&v, 40, 10);
        assert!(
            s[3].starts_with(" ╭─ code · ←/→ scroll ─"),
            "the frame says so: {s:?}"
        );
        assert!(s[4].starts_with(" │ 0123") && s[4].ends_with('›'), "{s:?}");
        v.hscroll = 10;
        let s = screen(&v, 40, 10);
        assert_eq!(s[1], " Some prose.", "prose stays put: {s:?}");
        assert!(s[4].starts_with(" │ ‹1234567890"), "{s:?}");
        assert!(s[4].ends_with('›'), "more to the right: {s:?}");
        assert_eq!(s[5], " │", "a short row slides out of sight: {s:?}");
        v.hscroll = 1000;
        let s = screen(&v, 40, 10);
        assert!(s[4].ends_with("6789"), "clamped at the right end: {s:?}");
    }

    #[test]
    fn sliding_never_splits_a_wide_character() {
        let line = Line::from(vec![Span::raw("│ "), Span::raw("界界界界界界")]);
        let out: String = slide(&line, 2, 1, 7)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(out.width(), 7, "{out:?}");
        assert!(out.starts_with("│ ‹"), "{out:?}");
        assert!(out.ends_with('›'), "{out:?}");
    }

    #[test]
    fn the_editor_numbers_lines_and_previews_beside_it_when_wide() {
        let (_d, mut v) = open("# A\n\ntext\n");
        v.pane = Pane::Edit;
        let s = screen(&v, 120, 8);
        assert!(s[1].starts_with("1  # A"), "{s:?}");
        assert!(s[1].contains("│ A"), "the preview is beside it: {s:?}");
        assert!(s[0].contains("Ln 1, Col 1"), "{s:?}");
        let s = screen(&v, 80, 8);
        assert!(!s[1].contains('│'), "narrow: the editor alone: {s:?}");
    }

    #[test]
    fn the_editor_scrolls_to_keep_the_cursor_in_sight() {
        let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let (_d, mut v) = open(&text);
        v.pane = Pane::Edit;
        v.text.set_cursor(text.len());
        let s = screen(&v, 80, 10);
        assert!(s[8].trim_start().starts_with("51"), "{s:?}");
    }

    #[test]
    fn a_long_path_gives_way_to_the_position() {
        assert_eq!(tail("/a/very/long/path/doc.md", 10), "…th/doc.md");
        assert_eq!(tail("doc.md", 10), "doc.md");
        assert_eq!(tail("doc.md", 0), "");
    }

    #[test]
    fn source_lines_are_classified_for_colour() {
        let k = source_kinds("---\na: b\n---\n# H\n```\n# not\n```\n> q\nplain");
        assert_eq!(
            k,
            vec![
                SourceKind::FrontMatter,
                SourceKind::FrontMatter,
                SourceKind::FrontMatter,
                SourceKind::Heading(1),
                SourceKind::Fence,
                SourceKind::Code,
                SourceKind::Fence,
                SourceKind::Quote,
                SourceKind::Plain
            ]
        );
    }
}
