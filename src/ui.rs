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
    let title = format!(
        " {} — {} [{label}] ",
        session.profile.name,
        session.dir.display()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let screen = session.parser.screen();
    f.render_widget(PseudoTerminal::new(screen), inner);
    // real cursor while attached, so the agent's input caret is visible
    if matches!(app.mode, Mode::Attached) && !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        f.set_cursor_position((inner.x + col, inner.y + row));
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let text = if let Some(err) = &app.error {
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
