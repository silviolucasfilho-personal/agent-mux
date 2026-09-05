use crate::app::{
    App, BrowserPane, DialogContentMode, DialogField, DialogState, HistoryPane, HistoryState, Mode,
    NoticeLevel, TraceBrowserState,
};
use crate::status::Status;
use crate::tracing::cli::{fmt_cost, fmt_ms, fmt_time, fmt_tokens};
use crate::tracing::store::query::LaunchStats;
use crate::tracing::view::{self as trace_view, Bar};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use std::time::Instant;
use tui_term::widget::PseudoTerminal;

pub const SIDEBAR_WIDTH: u16 = 30;

/// First visible index of the sidebar's session list: 0 until the selected
/// row would fall below the window, then scrolled just far enough to keep
/// it on the last row. Pure — the mouse handler uses the same math to map a
/// clicked row back to a session index.
pub fn sidebar_window(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        0
    } else if selected >= visible {
        (selected + 1 - visible).min(len - visible)
    } else {
        0
    }
}

/// Char-boundary-safe truncation with an ellipsis. Byte slicing here
/// panicked on non-ASCII titles (emoji, accents).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let keep: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{keep}...")
    } else {
        s.to_string()
    }
}

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
        Mode::SessionHistory(history) => draw_session_history(f, history, app),
        Mode::TraceBrowser(browser) => draw_trace_browser(f, browser),
        Mode::ConfirmKill => draw_confirm(f, "Kill this session? [y/n]"),
        Mode::ConfirmQuit => draw_confirm(f, "Sessions are still working. Quit anyway? [y/n]"),
        Mode::Help => draw_help(f),
        _ => {}
    }
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    let title = if app.sessions.is_empty() {
        "agent-mux".to_string()
    } else {
        format!("agent-mux [{}/{}]", app.selected + 1, app.sessions.len())
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    if app.sessions.is_empty() {
        let hint = Paragraph::new("no sessions\n\n[n] new session").block(block);
        f.render_widget(hint, area);
        return;
    }
    let visible = usize::from(area.height.saturating_sub(2));
    let start = sidebar_window(app.selected, app.sessions.len(), visible);
    let end = (start + visible.max(1)).min(app.sessions.len());
    let items: Vec<ListItem> = app.sessions[start..end]
        .iter()
        .enumerate()
        .map(|(offset, s)| {
            let i = start + offset;
            let (label, style) = status_label_style(s.status(now));
            let marker = if i == app.selected { "> " } else { "  " };
            // rows 1-9 are directly addressable from Control mode
            let num = if i < 9 {
                format!("{} ", i + 1)
            } else {
                "  ".into()
            };
            let mut spans = vec![
                Span::raw(marker.to_string()),
                Span::styled(num, Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ", s.profile.name)),
                Span::styled(format!("[{label}]"), style),
            ];
            if s.trace.is_some() {
                spans.push(Span::styled(
                    format!(
                        " {}",
                        trace_badge(s.trace_stats.as_ref(), false, session_backend(s))
                    ),
                    Style::default().fg(Color::Cyan),
                ));
            }
            let line = Line::from(spans);
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
    let (scrollback_len, scroll_offset) = session.scroll_view();
    let scroll_tag = if scroll_offset > 0 {
        format!("[SCROLL ↑ {scroll_offset}/{scrollback_len}] ")
    } else {
        String::new()
    };
    let trace_tag = if session.trace.is_some() {
        format!(
            "{} ",
            trace_badge(session.trace_stats.as_ref(), true, session_backend(session))
        )
    } else {
        String::new()
    };
    let title = format!(
        " {} — {} [{label}] {trace_tag}{scroll_tag}",
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
            if st.query.is_empty() {
                String::new()
            } else {
                "no matches".into()
            }
        } else {
            format!("{}/{}", st.current + 1, st.matches.len())
        };
        Line::raw(format!(
            "Search: {}  {count}  [Enter] next  [Shift+Enter] prev  [Esc] close",
            st.query
        ))
    } else if let Some(notice) = &app.notice {
        let color = match notice.level {
            NoticeLevel::Info => Color::Cyan,
            NoticeLevel::Warn => Color::Yellow,
            NoticeLevel::Error => Color::Red,
        };
        Line::styled(notice.text.clone(), Style::default().fg(color))
    } else {
        match app.mode {
            Mode::Attached => Line::raw(
                "ATTACHED — Ctrl+Q detach · Shift+PgUp/PgDn scroll · Ctrl+Shift+C/V copy/paste · Ctrl+Shift+F search",
            ),
            _ => Line::raw(
                "[j/k] select  [Enter] attach  [n] new  [l] logs  [t] trace  [T] traces  [x] kill  [?] help  [q] quit",
            ),
        }
    };
    f.render_widget(Paragraph::new(text), area);
}

/// Full keybinding reference — the one place every chord (including the
/// otherwise invisible Ctrl+Shift ones) is written down in the UI.
fn draw_help(f: &mut Frame) {
    let key_style = Style::default().fg(Color::Cyan);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let row = |key: &str, desc: &str| {
        Line::from(vec![
            Span::styled(format!("  {key:<18}"), key_style),
            Span::raw(desc.to_string()),
        ])
    };
    let lines: Vec<Line> = vec![
        Line::styled("Control mode", head),
        row("j/k, ↑/↓", "select session"),
        row("1-9", "jump to session N"),
        row("Enter", "attach to selected session"),
        row(
            "n",
            "new session (pick the trace backend: SQLite, Langfuse, both)",
        ),
        row("l", "browse past session logs"),
        row("t", "toggle tracing (local SQLite store)"),
        row("T", "browse local traces: turns, tools, tokens, cost"),
        row(
            "● ◆ ◈",
            "badge glyphs: traced locally, to Langfuse, to both",
        ),
        row("x", "kill session (remove if exited)"),
        row("r", "respawn exited session"),
        row("q", "quit"),
        Line::raw(""),
        Line::styled("Attached mode", head),
        row("Ctrl+Q", "detach back to control mode"),
        row("Ctrl+Q Ctrl+Q", "send a literal Ctrl+Q to the agent"),
        Line::raw(""),
        Line::styled("Scrollback, selection & search", head),
        row("Shift+PgUp/PgDn", "scroll one page"),
        row("Shift+Home/End", "jump to top / back to live"),
        row(
            "mouse wheel",
            "scroll (forwarded to the agent when it asks)",
        ),
        row("mouse drag", "select text — copied on release"),
        row("Ctrl+Shift+C/V", "copy selection / paste"),
        row("Ctrl+Shift+F", "search scrollback (Ctrl+F in control mode)"),
        Line::raw(""),
        Line::styled("Session logs", head),
        row("Tab, ←/→", "switch pane"),
        row("a", "toggle this project / all projects"),
        row("r or Enter", "resume the selected session"),
        Line::raw(""),
        Line::styled("Trace browser", head),
        row("Tab, ←/→", "sessions → turns → detail"),
        row("Enter", "drill in / expand an observation"),
        row("/", "full-text search (full mode content)"),
        row("a", "toggle this project / all projects"),
        row("r", "resume the selected session"),
        Line::raw(""),
        Line::styled("  [Esc] or [?] to close", dim),
    ];
    let height = (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2));
    let width = 64.min(f.area().width.saturating_sub(4)).max(40);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keyboard & mouse reference ");
    f.render_widget(Paragraph::new(lines).block(block), area);
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
    let width = 72.min(f.area().width.saturating_sub(4)).max(40);
    // the launch options add six rows when the profile runs a known CLI
    let options_rows = if dialog.harness.is_some() { 6 } else { 0 };
    let height = (app.profiles.len() as u16 + 20 + options_rows)
        .min(f.area().height.saturating_sub(2))
        .max(18);
    let area = centered(f.area(), width, height);
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
    let dir_style = if matches!(dialog.field, DialogField::Dir) && dialog.dir_selected_idx.is_none()
    {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    lines.push(Line::from(vec![
        Span::raw("Directory: "),
        Span::styled(dialog.dir.clone(), dir_style),
    ]));
    lines.push(Line::styled(
        "  Select subfolder: (↑/↓ choose, → open, Enter select)",
        Style::default().fg(Color::DarkGray),
    ));
    if dialog.dir_entries.is_empty() {
        lines.push(Line::styled(
            "    (no subdirectories)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        // the subfolder list is the flexible part: on a short terminal it
        // shrinks so the fields below it stay on screen
        let fixed_rows = app.profiles.len() + 12 + if dialog.harness.is_some() { 6 } else { 0 };
        let max_visible = usize::from(height).saturating_sub(fixed_rows).clamp(1, 4);
        let selected = dialog.dir_selected_idx.unwrap_or(0);
        let start = if selected >= max_visible {
            selected + 1 - max_visible
        } else {
            0
        };
        let end = (start + max_visible).min(dialog.dir_entries.len());
        for (idx, entry) in dialog.dir_entries[start..end].iter().enumerate() {
            let actual_idx = start + idx;
            let is_sel = dialog.dir_selected_idx == Some(actual_idx);
            let marker = if is_sel { "> " } else { "  " };
            let style = if is_sel && matches!(dialog.field, DialogField::Dir) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if entry == ".." {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Blue)
            };
            let display_name = if entry == ".." {
                ".. (parent directory)".to_string()
            } else {
                format!("{entry}/")
            };
            lines.push(Line::styled(format!("    {marker}{display_name}"), style));
        }
        if dialog.dir_entries.len() > max_visible {
            lines.push(Line::styled(
                format!("      ... ({} total directories)", dialog.dir_entries.len()),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    lines.push(Line::raw(""));

    // Tracing Option
    let tracing_style = if matches!(dialog.field, DialogField::Tracing) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let (trace_box, trace_color) = if dialog.tracing_enabled {
        ("[●] Enabled", Color::Cyan)
    } else {
        ("[○] Disabled", Color::DarkGray)
    };
    lines.push(Line::from(vec![
        Span::raw("Tracing:          "),
        Span::styled(trace_box, tracing_style.fg(trace_color)),
        Span::styled(" (Space to toggle)", Style::default().fg(Color::DarkGray)),
    ]));

    // Backend Option
    let backend_style = if matches!(dialog.field, DialogField::Backend) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let backend_label = match dialog.backend {
        crate::config::Backend::Local => "[Local SQLite]",
        crate::config::Backend::Langfuse => "[Langfuse]",
        crate::config::Backend::Both => "[Both] SQLite + Langfuse",
    };
    let backend_hint = if dialog.langfuse_available {
        " (Space to cycle)"
    } else {
        " (Langfuse: not configured — see `agent-mux trace doctor`)"
    };
    lines.push(Line::from(vec![
        Span::raw("Backend:          "),
        Span::styled(backend_label, backend_style.fg(Color::Yellow)),
        Span::styled(backend_hint, Style::default().fg(Color::DarkGray)),
    ]));

    // Content Mode Option
    let mode_style = if matches!(dialog.field, DialogField::ContentMode) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let mode_desc = match dialog.content_mode {
        DialogContentMode::Full => "[Full] Prompts, tool I/O, skills & subagents",
        DialogContentMode::Metadata => "[Metadata] Privacy-safe, token counts & timings only",
    };
    lines.push(Line::from(vec![
        Span::raw("Content Mode:     "),
        Span::styled(mode_desc, mode_style.fg(Color::Yellow)),
        Span::styled(" (Space to toggle)", Style::default().fg(Color::DarkGray)),
    ]));

    // Launch options, only for a command we know how to pass them to
    if let Some(harness) = dialog.harness {
        let focused = |f: DialogField| {
            if dialog.field == f {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }
        };
        let dim = Style::default().fg(Color::DarkGray);
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("── {} options ──", harness.as_str()),
            Style::default().fg(Color::DarkGray),
        ));

        let model = if dialog.model.is_empty() {
            "(the CLI's default)".to_string()
        } else {
            dialog.model.clone()
        };
        let model_style = if dialog.model.is_empty() {
            focused(DialogField::Model).fg(Color::DarkGray)
        } else {
            focused(DialogField::Model).fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::raw("Model:            "),
            Span::styled(model, model_style),
            Span::styled(" (--model, blank = unset)", dim),
        ]));

        let (approvals, approvals_color) = if dialog.bypass_approvals {
            ("[!] bypass", Color::Red)
        } else {
            ("[●] normal", Color::Green)
        };
        lines.push(Line::from(vec![
            Span::raw("Approvals:        "),
            Span::styled(
                approvals,
                focused(DialogField::Approvals).fg(approvals_color),
            ),
            Span::styled(
                if dialog.bypass_approvals {
                    match harness {
                        crate::harness::Harness::Codex => " (--yolo)",
                        _ => " (--dangerously-skip-permissions)",
                    }
                } else {
                    " (Space to toggle)"
                },
                dim,
            ),
        ]));

        lines.push(Line::from(vec![
            Span::raw("Resume:           "),
            Span::styled(
                if dialog.resume_last {
                    "[Last session]"
                } else {
                    "[Off]"
                },
                focused(DialogField::Resume).fg(Color::Yellow),
            ),
            Span::styled(
                if dialog.resume_last {
                    match harness {
                        crate::harness::Harness::Codex => " (resume --last)",
                        _ => " (--continue)",
                    }
                } else {
                    " (Space to toggle)"
                },
                dim,
            ),
        ]));

        let one_shot = if dialog.one_shot.is_empty() {
            "(interactive)".to_string()
        } else {
            truncate_chars(&dialog.one_shot, 44)
        };
        let one_shot_style = if dialog.one_shot.is_empty() {
            focused(DialogField::OneShot).fg(Color::DarkGray)
        } else {
            focused(DialogField::OneShot).fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::raw("One-shot prompt:  "),
            Span::styled(one_shot, one_shot_style),
            Span::styled(
                match harness {
                    crate::harness::Harness::Codex => " (codex exec)",
                    _ => " (-p, blank = unset)",
                },
                dim,
            ),
        ]));
    }

    if let Some(err) = &dialog.error {
        lines.push(Line::styled(err.clone(), Style::default().fg(Color::Red)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "[Tab] switch field  [Space] toggle  [Enter] launch  [Esc] cancel",
    ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_session_history(f: &mut Frame, history: &HistoryState, _app: &App) {
    let width = (f.area().width * 95 / 100).clamp(60, 140);
    let height = (f.area().height * 90 / 100).clamp(18, 45);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);

    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Min(0)]).areas(body);

    // Left pane: Session List
    let left_border_style = if matches!(history.focused_pane, HistoryPane::SessionsList) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let scope_label = if history.all_projects {
        "all projects"
    } else {
        "this project"
    };
    let left_title = format!(
        " Past Sessions ({}) [a: {scope_label}] ",
        history.sessions.len()
    );
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(left_border_style)
        .title(left_title);

    if history.sessions.is_empty() {
        let hint =
            Paragraph::new("\n  No past sessions found.\n\n  Press [a] to search all projects.")
                .style(Style::default().fg(Color::DarkGray))
                .block(left_block);
        f.render_widget(hint, left);
    } else {
        let items: Vec<ListItem> = history
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let is_sel = i == history.selected_session_idx;
                let marker = if is_sel { "> " } else { "  " };
                let badge = match s.provider {
                    crate::history::AgentProvider::Claude => {
                        Span::styled("[Claude] ", Style::default().fg(Color::LightMagenta))
                    }
                    crate::history::AgentProvider::Antigravity => {
                        Span::styled("[AGY]    ", Style::default().fg(Color::LightCyan))
                    }
                };
                let title_str = truncate_chars(&s.title, 18);
                let line = Line::from(vec![
                    Span::raw(marker),
                    badge,
                    Span::raw(format!("{:<16} ", s.timestamp_str)),
                    Span::styled(
                        title_str,
                        Style::default().fg(if is_sel { Color::Yellow } else { Color::White }),
                    ),
                ]);
                let item = ListItem::new(line);
                if is_sel && matches!(history.focused_pane, HistoryPane::SessionsList) {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Yellow))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // Right pane: Log Details
    let right_border_style = if matches!(history.focused_pane, HistoryPane::LogDetail) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let right_title = if let Some(s) = history.sessions.get(history.selected_session_idx) {
        let id_short: String = s.session_id.chars().take(8).collect();
        let scroll_tag = if history.scroll_offset > 0 {
            format!(
                " [↑ {}/{}] ",
                history.scroll_offset,
                history.log_lines.len()
            )
        } else {
            String::new()
        };
        let provider_tag = match s.provider {
            crate::history::AgentProvider::Claude => "Claude",
            crate::history::AgentProvider::Antigravity => "Antigravity",
        };
        format!(
            " Log [{provider_tag}]: {} ({id_short}){scroll_tag} ",
            s.title
        )
    } else {
        " Log ".to_string()
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_border_style)
        .title(right_title);

    let inner_right = right_block.inner(right);
    f.render_widget(right_block, right);
    // hand the real viewport height back so scrolling clamps to a full page
    history.viewport_rows.set(usize::from(inner_right.height));

    if let Some(err) = &history.error {
        let p = Paragraph::new(Line::styled(err.clone(), Style::default().fg(Color::Red)));
        f.render_widget(p, inner_right);
    } else if history.log_lines.is_empty() {
        let p = Paragraph::new(Line::styled(
            "  (No log output for this session)",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(p, inner_right);
    } else {
        let start = history
            .scroll_offset
            .min(history.log_lines.len().saturating_sub(1));
        let visible: Vec<Line> = history.log_lines[start..].to_vec();
        let p = Paragraph::new(visible);
        f.render_widget(p, inner_right);
    }

    // Footer line
    let footer_text = Line::styled(
        " [Tab] switch pane  [↑/↓/PgUp/PgDn] select/scroll  [a] toggle all projects  [r/Enter] resume  [Esc] close",
        Style::default().fg(Color::Black).bg(Color::Cyan),
    );
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// `[● TRACE]` until the first live rollup arrives, then turns + cost (or
/// tokens when nothing is priced); the verbose form adds the running tool.
/// The backend a live session's launch chose (local when untraced).
fn session_backend(session: &crate::session::Session) -> crate::config::Backend {
    session
        .trace
        .as_ref()
        .map(|t| t.backend)
        .unwrap_or_default()
}

pub fn trace_badge(
    stats: Option<&LaunchStats>,
    verbose: bool,
    backend: crate::config::Backend,
) -> String {
    let glyph = match backend {
        crate::config::Backend::Local => "●",
        crate::config::Backend::Langfuse => "◆",
        crate::config::Backend::Both => "◈",
    };
    let Some(stats) = stats else {
        return format!("[{glyph} TRACE]");
    };
    let money = match (stats.cost_usd, stats.total_tokens) {
        (Some(c), _) if c > 0.0 => fmt_cost(Some(c)),
        (_, Some(t)) if t > 0 => format!("{} tok", fmt_tokens(Some(t))),
        _ => String::new(),
    };
    let mut badge = format!("[{glyph} {}t", stats.turns);
    if !money.is_empty() {
        badge.push(' ');
        badge.push_str(&money);
    }
    if verbose && let Some(tool) = &stats.running_tool {
        badge.push_str(" ▸ ");
        badge.push_str(&truncate_chars(tool, 24));
    }
    badge.push(']');
    badge
}

fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn provider_badge(provider: &str) -> Span<'static> {
    match provider {
        "claude" => Span::styled("[C] ", Style::default().fg(Color::LightMagenta)),
        "codex" => Span::styled("[X] ", Style::default().fg(Color::LightGreen)),
        "antigravity" => Span::styled("[A] ", Style::default().fg(Color::LightCyan)),
        _ => Span::raw("[?] "),
    }
}

fn draw_trace_browser(f: &mut Frame, browser: &TraceBrowserState) {
    let width = (f.area().width * 96 / 100).clamp(60, 200);
    let height = (f.area().height * 92 / 100).clamp(18, 60);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, middle, right] = Layout::horizontal([
        Constraint::Percentage(26),
        Constraint::Percentage(40),
        Constraint::Min(0),
    ])
    .areas(body);

    // Sessions
    let scope = if browser.all_projects {
        "all projects"
    } else {
        "this project"
    };
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Sessions))
        .title(format!(
            " Sessions ({}) [a: {scope}] ",
            browser.sessions.len()
        ));
    if let Some(err) = &browser.error {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::styled(format!("  {err}"), Style::default().fg(Color::Red)),
            Line::raw(""),
            Line::styled(
                "  Traces are written while sessions run; see `agent-mux trace doctor`.",
                Style::default().fg(Color::DarkGray),
            ),
        ])
        .block(left_block);
        f.render_widget(p, left);
    } else if browser.sessions.is_empty() {
        let p = Paragraph::new("\n  No traced sessions yet.\n\n  Press [a] to show all projects, or\n  `agent-mux trace import --discover`\n  to backfill past transcripts.")
            .style(Style::default().fg(Color::DarkGray))
            .block(left_block);
        f.render_widget(p, left);
    } else {
        let visible = usize::from(left.height.saturating_sub(2));
        let start = sidebar_window(browser.selected_session, browser.sessions.len(), visible);
        let end = (start + visible.max(1)).min(browser.sessions.len());
        let items: Vec<ListItem> = browser.sessions[start..end]
            .iter()
            .enumerate()
            .map(|(offset, s)| {
                let i = start + offset;
                let is_sel = i == browser.selected_session;
                let title = s
                    .title
                    .clone()
                    .or_else(|| s.cwd.clone())
                    .unwrap_or_else(|| s.session_id.clone());
                let live = if s.open_turns > 0 { "● " } else { "" };
                let line = Line::from(vec![
                    Span::raw(if is_sel { "> " } else { "  " }),
                    provider_badge(&s.provider),
                    Span::styled(
                        format!("{} ", &fmt_time(s.last_seen_ns)[5..16]),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{}t {} ", s.turn_count, fmt_cost(s.total_cost_usd)),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(live.to_string(), Style::default().fg(Color::Green)),
                    Span::raw(truncate_chars(&title, 40)),
                ]);
                let item = ListItem::new(line);
                if is_sel && browser.focused == BrowserPane::Sessions {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Yellow))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // Turns
    let middle_title = match &browser.search_query {
        Some(q) => format!(
            " Search: {} ({}) ",
            truncate_chars(q, 24),
            browser.turns.len()
        ),
        None => match browser.sessions.get(browser.selected_session) {
            Some(s) => format!(
                " Turns ({})  {} tok  {} ",
                browser.turns.len(),
                fmt_tokens(s.total_tokens),
                fmt_cost(s.total_cost_usd)
            ),
            None => " Turns ".to_string(),
        },
    };
    let middle_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Turns))
        .title(middle_title);
    if browser.turns.is_empty() {
        let p = Paragraph::new(Line::styled(
            "  (no turns)",
            Style::default().fg(Color::DarkGray),
        ))
        .block(middle_block);
        f.render_widget(p, middle);
    } else {
        let visible = usize::from(middle.height.saturating_sub(2));
        let start = sidebar_window(browser.selected_turn, browser.turns.len(), visible);
        let end = (start + visible.max(1)).min(browser.turns.len());
        let items: Vec<ListItem> = browser.turns[start..end]
            .iter()
            .enumerate()
            .map(|(offset, t)| {
                let i = start + offset;
                let is_sel = i == browser.selected_turn;
                let status_style = match t.status.as_str() {
                    "open" => Style::default().fg(Color::Green),
                    "aborted" => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let errors = if t.error_count > 0 {
                    Span::styled(
                        format!(" {}!", t.error_count),
                        Style::default().fg(Color::Red),
                    )
                } else {
                    Span::raw("")
                };
                let name = t
                    .name
                    .split_once(": ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| t.name.clone());
                let line = Line::from(vec![
                    Span::raw(if is_sel { "> " } else { "  " }),
                    Span::styled(
                        format!("#{:<3}", t.ordinal),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:<7} ", t.status), status_style),
                    Span::styled(
                        format!(
                            "{:>6} {:>7} {:>2}🔧",
                            fmt_ms(t.latency_ms),
                            fmt_cost(t.total_cost_usd),
                            t.tool_count
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                    errors,
                    Span::raw(format!(" {}", truncate_chars(&name, 48))),
                ]);
                let item = ListItem::new(line);
                if is_sel && browser.focused == BrowserPane::Turns {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Yellow))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(middle_block), middle);
    }

    // Detail
    let right_title = match browser.turns.get(browser.selected_turn) {
        Some(t) if browser.expanded => format!(
            " Turn #{} · observation {}/{} [Esc] back ",
            t.ordinal,
            browser.selected_observation + 1,
            browser.observations.len()
        ),
        Some(t) => format!(
            " Turn #{} · {} · {} obs · {} · {} tok · {} ",
            t.ordinal,
            browser.detail_view.label(),
            browser.observations.len(),
            fmt_ms(t.latency_ms),
            fmt_tokens(t.total_tokens),
            fmt_cost(t.total_cost_usd)
        ),
        None => " Detail ".to_string(),
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Detail))
        .title(right_title);
    let inner_right = right_block.inner(right);
    f.render_widget(right_block, right);
    browser.viewport_rows.set(usize::from(inner_right.height));
    if browser.expanded {
        let start = browser
            .scroll_offset
            .min(browser.detail_lines.len().saturating_sub(1));
        let visible: Vec<Line> = browser.detail_lines[start..].to_vec();
        f.render_widget(Paragraph::new(visible), inner_right);
    } else if browser.observations.is_empty() {
        let hint = match browser.turns.get(browser.selected_turn) {
            Some(t) if t.input.is_some() => {
                let mut lines = vec![Line::styled(
                    "── input ──",
                    Style::default().fg(Color::Yellow),
                )];
                for l in t.input.as_deref().unwrap_or("").lines().take(50) {
                    lines.push(Line::raw(l.to_string()));
                }
                lines
            }
            _ => vec![Line::styled(
                "  (no observations)",
                Style::default().fg(Color::DarkGray),
            )],
        };
        f.render_widget(Paragraph::new(hint), inner_right);
    } else if browser.detail_view == crate::app::DetailView::Tree {
        draw_observation_tree(f, browser, inner_right);
    } else if browser.detail_view == crate::app::DetailView::Timeline {
        draw_observation_timeline(f, browser, inner_right);
    } else {
        let visible = usize::from(inner_right.height);
        let start = sidebar_window(
            browser.selected_observation,
            browser.observations.len(),
            visible,
        );
        let end = (start + visible.max(1)).min(browser.observations.len());
        let items: Vec<ListItem> = browser.observations[start..end]
            .iter()
            .enumerate()
            .map(|(offset, o)| {
                let i = start + offset;
                let is_sel = i == browser.selected_observation;
                let glyph = match (o.obs_type.as_str(), o.end_ns.is_some()) {
                    ("generation", _) => "💬",
                    ("agent", _) => "🤖",
                    (_, false) => "⏳",
                    _ => "🔧",
                };
                let duration = o
                    .end_ns
                    .map(|e| fmt_ms((e - o.start_ns) / 1_000_000))
                    .unwrap_or_else(|| "running".into());
                let level_style = match o.level.as_str() {
                    "ERROR" => Style::default().fg(Color::Red),
                    "WARNING" => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let line = Line::from(vec![
                    Span::raw(if is_sel { "> " } else { "  " }),
                    Span::styled(
                        format!("{} ", &fmt_time(o.start_ns)[11..]),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(format!("{glyph} ")),
                    Span::raw(truncate_chars(
                        &crate::tracing::cli::indent(o.depth, &o.name),
                        28,
                    )),
                    Span::styled(
                        format!(
                            " {:>7} {:>6} {:>7}",
                            duration,
                            fmt_tokens(o.total_tokens),
                            fmt_cost(o.total_cost_usd)
                        ),
                        level_style,
                    ),
                ]);
                let item = ListItem::new(line);
                if is_sel && browser.focused == BrowserPane::Detail {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items), inner_right);
    }

    let footer_text = match &browser.search_input {
        Some(input) => Line::styled(
            format!(" Search: {input}▏  [Enter] run  [Esc] cancel"),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
        None => Line::styled(
            " [Tab] pane  [↑/↓] select  [Enter] drill  [v] view  [space] fold  [/] search  [a] all  [r] resume  [Esc] close",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    };
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// Colour for an observation row, shared by both new views.
fn obs_style(o: &crate::tracing::store::query::ObservationView) -> Style {
    if o.is_error || o.level == "ERROR" {
        return Style::default().fg(Color::Red);
    }
    match o.level.as_str() {
        "WARNING" => Style::default().fg(Color::Yellow),
        _ => match o.obs_type.as_str() {
            "generation" => Style::default().fg(Color::Blue),
            "agent" => Style::default().fg(Color::Cyan),
            _ => Style::default(),
        },
    }
}

fn obs_glyph(o: &crate::tracing::store::query::ObservationView) -> &'static str {
    match (o.obs_type.as_str(), o.end_ns.is_some()) {
        ("generation", _) => "💬",
        ("agent", _) => "🤖",
        (_, false) => "⏳",
        _ => "🔧",
    }
}

/// The hierarchy: connectors from the depth sequence, folded subtrees
/// reporting what they hide.
fn draw_observation_tree(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let rows = trace_view::tree_rows(&browser.observations, &browser.collapsed);
    let selected = rows
        .iter()
        .position(|r| r.obs.id == browser.observations[browser.selected_observation].id)
        .unwrap_or(0);
    let height = usize::from(area.height);
    let start = sidebar_window(selected, rows.len(), height);
    let end = (start + height.max(1)).min(rows.len());
    let width = usize::from(area.width);
    let items: Vec<ListItem> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, r)| {
            let i = start + offset;
            let is_sel = i == selected;
            let fold = if !r.has_children {
                "  "
            } else if r.collapsed {
                "▸ "
            } else {
                "▾ "
            };
            let metrics = if r.collapsed {
                format!(
                    " +{} {:>6} {:>7}",
                    r.hidden,
                    fmt_tokens(Some(r.subtree.tokens).filter(|t| *t > 0)),
                    fmt_cost(Some(r.subtree.cost).filter(|c| *c > 0.0))
                )
            } else {
                format!(
                    " {:>7} {:>6} {:>7}",
                    r.obs
                        .end_ns
                        .map(|e| fmt_ms((e - r.obs.start_ns) / 1_000_000))
                        .unwrap_or_else(|| "running".into()),
                    fmt_tokens(r.obs.total_tokens),
                    fmt_cost(r.obs.total_cost_usd)
                )
            };
            // the name takes whatever the connectors and metrics leave,
            // padded so the metrics stay in one column down the pane
            let budget = width
                .saturating_sub(r.prefix.chars().count() + metrics.chars().count() + 6)
                .max(8);
            let name = truncate_chars(&r.obs.name, budget);
            let line = Line::from(vec![
                Span::raw(if is_sel { ">" } else { " " }),
                Span::styled(fold.to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled(r.prefix.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ", obs_glyph(r.obs))),
                Span::styled(format!("{name:<budget$}"), obs_style(r.obs)),
                Span::styled(metrics, Style::default().fg(Color::DarkGray)),
            ]);
            let item = ListItem::new(line);
            if is_sel && browser.focused == BrowserPane::Detail {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), area);
}

/// Time, proportional to the turn: an axis, then one bar per observation.
fn draw_observation_timeline(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let turn = browser.turns.get(browser.selected_turn);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let window = trace_view::window(
        turn.map(|t| t.start_ns).unwrap_or(0),
        turn.and_then(|t| t.end_ns),
        &browser.observations,
        now_ns,
    );
    // label column, then the track
    let label_cols = usize::from(area.width).saturating_sub(4).clamp(6, 22);
    let track_cols = usize::from(area.width).saturating_sub(label_cols + 1);
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::raw(" ".repeat(label_cols + 1)),
        Span::styled(
            trace_view::axis(&window, track_cols),
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    let height = usize::from(area.height).saturating_sub(1);
    let start = sidebar_window(
        browser.selected_observation,
        browser.observations.len(),
        height,
    );
    let end = (start + height.max(1)).min(browser.observations.len());
    for (i, o) in browser.observations[start..end].iter().enumerate() {
        let i = start + i;
        let is_sel = i == browser.selected_observation;
        let Bar {
            offset,
            width,
            running,
            instant,
        } = trace_view::bar(o.start_ns, o.end_ns, &window, track_cols);
        let indent = "  ".repeat(o.depth.min(3));
        let label = format!(
            "{}{}{}",
            if is_sel { ">" } else { " " },
            indent,
            truncate_chars(&o.name, label_cols.saturating_sub(indent.len() + 1))
        );
        let bar_body = if instant {
            "▏".to_string()
        } else if running {
            format!("{}▶", "█".repeat(width.saturating_sub(1)))
        } else {
            "█".repeat(width)
        };
        let mut spans = vec![
            Span::styled(format!("{label:<label_cols$} "), obs_style(o)),
            Span::styled("░".repeat(offset), Style::default().fg(Color::DarkGray)),
            Span::styled(bar_body, obs_style(o)),
        ];
        if is_sel && browser.focused == BrowserPane::Detail {
            spans = spans
                .into_iter()
                .map(|s| {
                    let style = s.style.add_modifier(Modifier::REVERSED);
                    s.style(style)
                })
                .collect();
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_confirm(f: &mut Frame, message: &str) {
    let width = (message.chars().count() as u16 + 4).min(f.area().width);
    let area = centered(f.area(), width, 3);
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
        let app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("no sessions"), "got: {text}");
        assert!(text.contains("[n]"), "keybinding hint missing: {text}");
    }

    #[test]
    fn sidebar_window_keeps_selection_visible() {
        // fits: never scrolls
        assert_eq!(sidebar_window(0, 3, 5), 0);
        assert_eq!(sidebar_window(2, 3, 5), 0);
        // overflow: 0 until the selection passes the window, then follows
        assert_eq!(sidebar_window(0, 10, 4), 0);
        assert_eq!(sidebar_window(3, 10, 4), 0);
        assert_eq!(sidebar_window(4, 10, 4), 1);
        assert_eq!(sidebar_window(9, 10, 4), 6);
        // degenerate viewport
        assert_eq!(sidebar_window(5, 10, 0), 0);
    }

    #[test]
    fn truncate_chars_is_multibyte_safe() {
        assert_eq!(truncate_chars("short", 18), "short");
        // 19 emoji: byte-index slicing panicked here before
        let emoji = "🚀".repeat(19);
        let out = truncate_chars(&emoji, 18);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 18);
    }

    #[test]
    fn help_overlay_lists_hidden_chords() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        app.mode = crate::app::Mode::Help;
        let mut terminal = Terminal::new(TestBackend::new(100, 34)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        for needle in [
            "Ctrl+Shift+C/V",
            "Ctrl+Shift+F",
            "Shift+PgUp/PgDn",
            "Ctrl+Q Ctrl+Q",
            "jump to session N",
            "resume the selected session",
        ] {
            assert!(
                text.contains(needle),
                "help overlay missing {needle}: {text}"
            );
        }
    }

    #[test]
    fn control_hint_advertises_help() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        assert!(buffer_text(&terminal).contains("[?] help"));
    }

    #[test]
    fn notice_levels_render_distinct_colors() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for (notice, want) in [
            (crate::app::Notice::info("tracing started"), Color::Cyan),
            (
                crate::app::Notice::warn("tracing: check config"),
                Color::Yellow,
            ),
            (crate::app::Notice::error("write failed"), Color::Red),
        ] {
            app.notice = Some(notice);
            terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
            let cell = &terminal.backend().buffer()[(0, 23)];
            assert_eq!(cell.style().fg, Some(want), "level color mismatch");
        }
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
            Match {
                row: 5,
                col_start: 1,
                col_end: 3,
            },
            Match {
                row: 6,
                col_start: 0,
                col_end: 2,
            },
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
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut st = crate::search::SearchState::new();
        st.query = "hello".into();
        app.search = Some(st);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Search: hello"), "got: {text}");
        assert!(
            text.contains("no matches"),
            "empty result indicator: {text}"
        );
    }

    #[test]
    fn dialog_mode_renders_profile_and_dir() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        app.mode = crate::app::Mode::NewSession(crate::app::DialogState::new(&app.profiles));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("New session"), "got: {text}");
        assert!(text.contains("Claude Code"), "got: {text}");
        assert!(text.contains("Tracing:"), "got: {text}");
        assert!(text.contains("Content Mode:"), "got: {text}");
        // the default profiles run real CLIs, so the options show
        assert!(text.contains("claude options"), "got: {text}");
        assert!(text.contains("Model:"), "got: {text}");
        assert!(text.contains("Approvals:"), "got: {text}");
        assert!(text.contains("Resume:"), "got: {text}");
        assert!(text.contains("One-shot prompt:"), "got: {text}");
    }

    #[test]
    fn launch_options_render_their_values_and_hide_for_a_plain_command() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let profiles = vec![
            crate::config::Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            crate::config::Profile {
                name: "Shell".into(),
                command: "bash".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
        ];
        let mut app = App::new(profiles.clone(), None, tx);
        let mut dialog = crate::app::DialogState::new(&profiles);
        dialog.model = "gpt-5.6".into();
        dialog.bypass_approvals = true;
        dialog.resume_last = true;
        dialog.one_shot = "fix the failing test".into();
        app.mode = crate::app::Mode::NewSession(dialog);
        let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("codex options"), "got: {text}");
        assert!(text.contains("gpt-5.6"), "got: {text}");
        // the hints name the flag this CLI actually uses
        assert!(text.contains("--yolo"), "got: {text}");
        assert!(text.contains("resume --last"), "got: {text}");
        assert!(text.contains("codex exec"), "got: {text}");
        assert!(text.contains("fix the failing test"), "got: {text}");

        // an unset model reads as the CLI's default, not as a blank
        if let crate::app::Mode::NewSession(d) = &mut app.mode {
            d.model.clear();
            d.one_shot.clear();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("(the CLI's default)"), "got: {text}");
        assert!(text.contains("(interactive)"), "got: {text}");

        // a profile that is not a known CLI shows no options at all
        if let crate::app::Mode::NewSession(d) = &mut app.mode {
            *d = crate::app::DialogState::new(&profiles[1..]);
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("Approvals:"), "got: {text}");
        assert!(!text.contains("One-shot"), "got: {text}");
        assert!(
            text.contains("Content Mode:"),
            "the rest still renders: {text}"
        );
    }

    #[test]
    fn session_history_mode_renders_split_view() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut history = crate::app::HistoryState::new(None);
        history.sessions = vec![crate::history::SessionSummary {
            session_id: "test-uuid-1234".into(),
            title: "Test History Title".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: std::path::PathBuf::from("/fake/path.jsonl"),
            turn_count: 5,
            project_slug: "-test".into(),
            timestamp_str: "2026-08-30 19:28".into(),
            provider: crate::history::AgentProvider::Claude,
            cwd: None,
        }];
        history.log_lines = vec![
            ratatui::text::Line::raw("👤 USER: hello world"),
            ratatui::text::Line::raw("🤖 CLAUDE: hi there!"),
        ];
        // HistoryState::new may have scrolled a machine-local log; this
        // synthetic 2-line log starts at the top
        history.scroll_offset = 0;
        app.mode = crate::app::Mode::SessionHistory(history);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Past Sessions"), "got: {text}");
        assert!(text.contains("Test History Title"), "got: {text}");
        assert!(text.contains("hello world"), "got: {text}");
        assert!(text.contains("hi there!"), "got: {text}");
        assert!(text.contains("switch pane"), "got: {text}");
    }

    #[test]
    fn trace_badge_shapes() {
        use crate::config::Backend;
        assert_eq!(trace_badge(None, true, Backend::Local), "[● TRACE]");
        assert_eq!(trace_badge(None, true, Backend::Langfuse), "[◆ TRACE]");
        let stats = LaunchStats {
            turns: 3,
            total_tokens: Some(12_000),
            cost_usd: Some(0.42),
            running_tool: Some("Bash".into()),
        };
        assert_eq!(
            trace_badge(Some(&stats), false, Backend::Local),
            "[● 3t $0.42]"
        );
        assert_eq!(
            trace_badge(Some(&stats), true, Backend::Both),
            "[◈ 3t $0.42 ▸ Bash]"
        );
        let unpriced = LaunchStats {
            turns: 1,
            total_tokens: Some(12_000),
            cost_usd: None,
            running_tool: None,
        };
        assert_eq!(
            trace_badge(Some(&unpriced), true, Backend::Local),
            "[● 1t 12k tok]"
        );
    }

    #[test]
    fn trace_browser_renders_sessions_turns_and_observations() {
        use crate::tracing::pricing::PriceTable;
        use crate::tracing::store::model::{
            Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
        };
        use crate::tracing::store::{OpenOptions, open_rw};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-ui".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        store
            .apply(&[
                StoreOp::Trace(TraceRow {
                    id: "t1".into(),
                    session_key: "claude:s-ui".into(),
                    provider: "claude".into(),
                    session_id: "s-ui".into(),
                    launch_id: None,
                    ordinal: 1,
                    name: "Claude Code: fix the widget".into(),
                    status: TraceStatus::Closed,
                    start_ns: 1_700_000_000_000_000_000,
                    end_ns: Some(1_700_000_004_000_000_000),
                    input: Some("fix the widget".into()),
                    output: Some("fixed".into()),
                    thinking: None,
                    skills: None,
                    reported_duration_ms: None,
                    reported_message_count: None,
                    session_cost_usd: None,
                    timing_approx: false,
                    ordinal_salted: false,
                    metadata: None,
                }),
                StoreOp::Observation(ObservationRow {
                    id: "o1".into(),
                    trace_id: "t1".into(),
                    parent_id: None,
                    obs_type: ObservationType::Tool,
                    name: "Bash".into(),
                    kind: None,
                    start_ns: 1_700_000_001_000_000_000,
                    end_ns: Some(1_700_000_002_000_000_000),
                    level: Level::Default,
                    status_message: None,
                    model: None,
                    input: Some("{\"command\":\"ls\"}".into()),
                    output: Some("Cargo.toml".into()),
                    thinking: None,
                    usage_raw: None,
                    usage: None,
                    tool_id: Some("tool-1".into()),
                    tool_name: Some("Bash".into()),
                    skill: None,
                    mcp_server: None,
                    path: None,
                    is_error: false,
                    ts_approx: false,
                    metadata: serde_json::Map::new(),
                }),
            ])
            .unwrap();
        drop(store);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut browser = crate::app::TraceBrowserState::new(Some(&db), None);
        browser.all_projects = true;
        browser.reload_sessions();
        assert_eq!(browser.sessions.len(), 1, "{:?}", browser.error);
        assert_eq!(browser.turns.len(), 1);
        assert_eq!(browser.observations.len(), 1);
        app.mode = crate::app::Mode::TraceBrowser(Box::new(browser));
        let mut terminal = Terminal::new(TestBackend::new(180, 36)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Sessions (1)"), "got: {text}");
        assert!(text.contains("fix the widget"), "got: {text}");
        assert!(text.contains("Bash"), "got: {text}");
        assert!(text.contains("[Tab] pane"), "got: {text}");
        // expanding shows the observation body
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.focused = BrowserPane::Detail;
            b.toggle_expanded();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Cargo.toml"), "got: {text}");
        // tracing off: the browser explains instead of panicking
        let off = crate::app::TraceBrowserState::new(None, None);
        assert!(off.error.as_deref().unwrap_or("").contains("off"));
    }

    #[test]
    fn tree_and_timeline_views_render_their_own_shapes() {
        use crate::app::DetailView;
        use crate::tracing::store::query::ObservationView;
        let view = |id: &str, name: &str, depth: usize, start_ns: i64, end_ns: Option<i64>| {
            ObservationView {
                id: id.into(),
                trace_id: "trace-1".into(),
                parent_id: None,
                depth,
                obs_type: if depth == 0 && id == "a" {
                    "agent".into()
                } else {
                    "tool".into()
                },
                name: name.into(),
                kind: None,
                start_ns,
                end_ns,
                level: "DEFAULT".into(),
                status_message: None,
                model: None,
                model_id: None,
                input: None,
                output: None,
                thinking: None,
                usage: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(1_000),
                total_cost_usd: Some(0.01),
                tool_id: None,
                tool_name: None,
                skill: None,
                mcp_server: None,
                path: None,
                is_error: false,
                metadata: "{}".into(),
            }
        };
        // real traces carry epoch nanos, and a running row is compared
        // against the clock: anchor the fixture just before now
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let base = now_ns - 4_000_000_000;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut browser = crate::app::TraceBrowserState::new(None, None);
        browser.error = None;
        browser.turns = vec![crate::tracing::store::query::TraceStat {
            id: "trace-1".into(),
            session_key: "claude:s".into(),
            launch_id: None,
            ordinal: 1,
            name: "Claude Code: do it".into(),
            status: "open".into(),
            start_ns: base,
            // still open, so the running row below is measured against now
            end_ns: None,
            latency_ms: 4_000,
            input: None,
            output: None,
            thinking: None,
            skills: "[]".into(),
            reported_duration_ms: None,
            session_cost_usd: None,
            closed_by: None,
            observation_count: 4,
            generation_count: 0,
            tool_count: 3,
            error_count: 0,
            open_count: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(4_000),
            total_cost_usd: Some(0.04),
            unpriced_generations: 0,
            models: None,
        }];
        browser.observations = vec![
            view("a", "agent: Explore", 0, base, Some(base + 2_000_000_000)),
            view("g", "Grep", 1, base + 200_000_000, Some(base + 900_000_000)),
            view(
                "r",
                "Read",
                1,
                base + 1_000_000_000,
                Some(base + 1_800_000_000),
            ),
            view("b", "Bash", 0, base + 2_100_000_000, None),
        ];
        browser.focused = BrowserPane::Detail;
        browser.detail_view = DetailView::Tree;
        app.mode = crate::app::Mode::TraceBrowser(Box::new(browser));
        let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("tree"), "the title names the view: {text}");
        assert!(text.contains("├─"), "connectors: {text}");
        assert!(text.contains("└─"), "connectors: {text}");
        assert!(
            text.contains("▾"),
            "an expanded parent shows a fold mark: {text}"
        );
        assert!(text.contains("[v] view"), "footer: {text}");

        // folded: the children go away and the parent reports the subtree
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.selected_observation = 0;
            b.toggle_collapsed();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("▸"), "a folded parent flips its mark: {text}");
        assert!(text.contains("+2"), "it says what it hid: {text}");
        assert!(!text.contains("Grep"), "children are hidden: {text}");

        // timeline: an axis, bars, and a cap on the running row
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.collapsed.clear();
            b.detail_view = DetailView::Timeline;
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("timeline"),
            "the title names the view: {text}"
        );
        assert!(text.contains("0ms"), "axis: {text}");
        assert!(text.contains("█"), "bars: {text}");
        assert!(text.contains("░"), "lead-in before a later bar: {text}");
        assert!(text.contains("▶"), "the running row is capped: {text}");
        assert!(text.contains("Grep"), "every row is listed: {text}");

        // a cramped terminal must not panic in either view
        let mut tiny = Terminal::new(TestBackend::new(40, 10)).unwrap();
        tiny.draw(|f| draw(f, &app, Instant::now())).unwrap();
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.detail_view = DetailView::Tree;
        }
        tiny.draw(|f| draw(f, &app, Instant::now())).unwrap();
    }
}
