use crate::app::{App, DialogField, DialogState, Mode};
use crate::status::Status;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use std::time::Instant;
use tui_term::widget::PseudoTerminal;

pub const SIDEBAR_WIDTH: u16 = 30;

pub fn status_label_style(status: Status) -> (String, Style) {
    match status {
        Status::Working => ("working".into(), Style::default().fg(Color::Green)),
        Status::Idle => ("idle".into(), Style::default().fg(Color::DarkGray)),
        Status::NeedsAttention => (
            "attention".into(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Status::Exited(Some(code)) => (format!("exit {code}"), Style::default().fg(Color::Red)),
        Status::Exited(None) => ("exited".into(), Style::default().fg(Color::Red)),
    }
}

/// (rows, cols) inside the main pane's borders, given the full terminal
/// rect. Keep in sync with the Layout in draw(): 1 status-bar row at the
/// bottom, SIDEBAR_WIDTH columns on the left, 1-cell border all around
/// the main pane.
pub fn main_pane_inner(total: Rect) -> (u16, u16) {
    let rows = total.height.saturating_sub(1).saturating_sub(2).max(1);
    let cols = total
        .width
        .saturating_sub(SIDEBAR_WIDTH)
        .saturating_sub(2)
        .max(1);
    (rows, cols)
}

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

/// Like `pane_local`, but clamps out-of-range terminal coordinates into the
/// pane interior instead of returning `None`. Used to finalize a drag whose
/// terminating event (e.g. the mouse-up) landed outside the pane -- the
/// drag still needs a definite final position rather than being stranded
/// with `dragging: true` forever.
pub fn pane_clamped(col: u16, row: u16, pane: (u16, u16)) -> (u16, u16) {
    let (rows, cols) = pane;
    let (x0, y0) = PANE_ORIGIN;
    let max_x = x0 + cols.saturating_sub(1);
    let max_y = y0 + rows.saturating_sub(1);
    let cx = col.clamp(x0, max_x);
    let cy = row.clamp(y0, max_y);
    (cx - x0, cy - y0)
}

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

pub fn draw(f: &mut Frame, app: &App, now: Instant) {
    let [body, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    let [side, main] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);

    draw_sidebar(f, side, app, now);
    draw_main(f, main, app, now);
    draw_status_bar(f, bar, app);

    match &app.mode {
        Mode::NewSession(dialog) => draw_new_session_dialog(f, dialog, app),
        Mode::ConfirmKill => draw_confirm(f, "Kill this session? [y/n]"),
        Mode::ConfirmQuit => draw_confirm(f, "Sessions are still working. Quit anyway? [y/n]"),
        _ => {}
    }
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    let block = Block::default().borders(Borders::ALL).title("sessions");
    if app.sessions.is_empty() {
        let hint = Paragraph::new("no sessions\n\n[n] new session").block(block);
        f.render_widget(hint, area);
        return;
    }
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (label, style) = status_label_style(s.status(now));
            let marker = if i == app.selected { "> " } else { "  " };
            let line = Line::from(vec![
                Span::raw(format!("{marker}{} ", s.profile.name)),
                Span::styled(format!("[{label}]"), style),
            ]);
            let item = ListItem::new(line);
            if i == app.selected {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_main(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    let Some(session) = app.sessions.get(app.selected) else {
        let block = Block::default().borders(Borders::ALL).title("agent-mux");
        f.render_widget(
            Paragraph::new("no sessions — press [n] to create one").block(block),
            area,
        );
        return;
    };
    let (label, _) = status_label_style(session.status(now));
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
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let cursor = {
        let screen = session.parser.screen();
        f.render_widget(PseudoTerminal::new(screen), inner);
        // real cursor while attached AND live: a scrolled view is history,
        // the cursor belongs to the bottom of the buffer
        (matches!(app.mode, Mode::Attached) && !screen.hide_cursor() && scroll_offset == 0)
            .then(|| screen.cursor_position())
    };
    if let Some((row, col)) = cursor {
        f.set_cursor_position((inner.x + col, inner.y + row));
    }
    if let Some(sel) = app.displayed_selection() {
        let (len, offset) = session.scroll_view();
        apply_selection_highlight(f.buffer_mut(), inner, sel, len, offset);
    }
    if let Some(st) = &app.search {
        let (len, offset) = session.scroll_view();
        apply_search_highlight(f.buffer_mut(), inner, &st.matches, st.current, len, offset);
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
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
        Line::styled(err.clone(), Style::default().fg(Color::Red))
    } else {
        match app.mode {
            Mode::Attached => Line::raw("ATTACHED — Ctrl+Q detach, Ctrl+Q Ctrl+Q send literal"),
            _ => {
                Line::raw("[j/k] select  [Enter] attach  [n] new  [x] kill  [r] respawn  [q] quit")
            }
        }
    };
    f.render_widget(Paragraph::new(text), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_new_session_dialog(f: &mut Frame, dialog: &DialogState, app: &App) {
    let area = centered(
        f.area(),
        60,
        (app.profiles.len() as u16 + 8).min(f.area().height),
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New session ");
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("Profile:"));
    for (i, p) in app.profiles.iter().enumerate() {
        let marker = if i == dialog.profile_idx { "> " } else { "  " };
        let style = if i == dialog.profile_idx && matches!(dialog.field, DialogField::Profile) {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(format!("{marker}{}", p.name), style));
    }
    lines.push(Line::raw(""));
    let dir_style = if matches!(dialog.field, DialogField::Dir) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    lines.push(Line::from(vec![
        Span::raw("Directory: "),
        Span::styled(dialog.dir.clone(), dir_style),
    ]));
    if let Some(err) = &dialog.error {
        lines.push(Line::styled(err.clone(), Style::default().fg(Color::Red)));
    }
    lines.push(Line::raw("[Tab] switch  [Enter] start  [Esc] cancel"));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm(f: &mut Frame, message: &str) {
    let area = centered(f.area(), (message.len() as u16 + 4).min(f.area().width), 3);
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(Paragraph::new(message).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::Config;
    use crate::status::Status;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn status_labels() {
        assert_eq!(status_label_style(Status::Working).0, "working");
        assert_eq!(status_label_style(Status::Idle).0, "idle");
        assert_eq!(status_label_style(Status::NeedsAttention).0, "attention");
        assert_eq!(status_label_style(Status::Exited(Some(2))).0, "exit 2");
        assert_eq!(status_label_style(Status::Exited(None)).0, "exited");
    }

    #[test]
    fn main_pane_inner_accounts_for_sidebar_borders_and_status_bar() {
        // 100 cols x 40 rows total:
        // cols: 100 - 30 sidebar - 2 border = 68
        // rows: 40 - 1 status bar - 2 border = 37
        let total = ratatui::layout::Rect::new(0, 0, 100, 40);
        assert_eq!(main_pane_inner(total), (37, 68));
    }

    #[test]
    fn main_pane_inner_never_returns_zero() {
        let tiny = ratatui::layout::Rect::new(0, 0, 5, 2);
        let (rows, cols) = main_pane_inner(tiny);
        assert!(rows >= 1 && cols >= 1);
    }

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

    #[test]
    fn pane_clamped_clamps_outside_coordinates() {
        let pane = (37u16, 68u16); // (rows, cols) matching pane_local_matches_layout_math
        // inside: same result as pane_local (passthrough)
        assert_eq!(pane_clamped(31, 1, pane), (0, 0));
        assert_eq!(pane_clamped(31 + 67, 1 + 36, pane), (67, 36));
        // left of the pane -> clamps to the left edge column
        assert_eq!(pane_clamped(0, 5, pane), (0, 4));
        // right of the pane -> clamps to the last column
        assert_eq!(pane_clamped(200, 5, pane), (67, 4));
        // above the pane -> clamps to the top row
        assert_eq!(pane_clamped(35, 0, pane), (4, 0));
        // below the pane -> clamps to the last row
        assert_eq!(pane_clamped(35, 100, pane), (4, 36));
    }

    #[test]
    fn empty_app_renders_hint() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = App::new(Config::default_profiles(), tx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("no sessions"), "got: {text}");
        assert!(text.contains("[n]"), "keybinding hint missing: {text}");
    }

    #[test]
    fn pseudo_terminal_widget_renders_parser_screen() {
        // Wiring check for tui-term without needing a real Session.
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"hello-widget");
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|f| {
                let widget = tui_term::widget::PseudoTerminal::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();
        assert!(buffer_text(&terminal).contains("hello-widget"));
    }

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
        // outside any match: untouched (ratatui's default Cell bg is
        // Color::Reset, not None -- style() always reports Some(_))
        assert_eq!(buf[(2 + 5, 1 + 1)].style().bg, Some(Color::Reset));
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

    #[test]
    fn dialog_mode_renders_profile_and_dir() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), tx);
        app.mode = crate::app::Mode::NewSession(crate::app::DialogState::new(&app.profiles));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("New session"), "got: {text}");
        assert!(text.contains("Claude Code"), "got: {text}");
    }
}
