//! Overlays retain the underlying screen and present its available actions.
use crate::app::commands::{HelpRow, help_rows};
use crate::app::{App, commands::Overlay};
use crate::keymap::ACTIONS;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

pub(super) fn draw(f: &mut Frame, app: &App) {
    if let Some(probe) = &app.shortcut_probe {
        let area = super::centered(f.area(), 78, 8);
        let id = probe.current().unwrap_or(crate::keymap::ActionId::Commands);
        let label = ACTIONS
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.label)
            .unwrap_or("Commands");
        f.render_widget(Clear, area);
        let lines = vec![
            Line::raw(format!("Press a shortcut for {label}")),
            Line::raw(crate::app::shortcut_probe::candidates(id)),
            Line::raw(probe.last_event.clone().unwrap_or_default()),
            Line::raw("Space skip · Esc cancel · Keys are observed without executing actions"),
        ];
        f.render_widget(
            ratatui::widgets::Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Test shortcuts"),
            ),
            area,
        );
        return;
    }
    let Some(overlay) = &app.command_overlay else {
        return;
    };
    let size = f.area();
    let width = size.width.min(84);
    let height = size.height.min(25);
    let area = Rect::new(
        size.x + (size.width - width) / 2,
        size.y + (size.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);
    let (title, selected, rows): (String, Option<usize>, Vec<ListItem>) = match overlay {
        Overlay::Commands(menu) => {
            use unicode_width::UnicodeWidthStr;
            // borders and the highlight symbol
            let inner = usize::from(width.saturating_sub(4));
            let sections = crate::app::commands::group_commands(&app.filtered_commands());
            let total_actions: usize = sections.iter().map(|s| s.actions.len()).sum();
            let selected_action = (total_actions > 0).then(|| menu.selected.min(total_actions - 1));
            let mut rows: Vec<ListItem> = Vec::new();
            let mut action_index = 0usize;
            // Headers occupy list rows but never become the selection: the
            // highlight follows the action under the cursor, skipping them.
            let mut selected_row = None;
            for section in sections {
                rows.push(ListItem::new(Line::from(Span::styled(
                    format!(" {} ", section.theme.title()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))));
                for a in section.actions {
                    if Some(action_index) == selected_action {
                        selected_row = Some(rows.len());
                    }
                    action_index += 1;
                    let available = app.command_availability(a.id);
                    let suffix = available
                        .reason()
                        .map(|r| format!(" — {r}"))
                        .unwrap_or_default();
                    let color = if available.enabled() {
                        Color::White
                    } else {
                        Color::DarkGray
                    };
                    let (key, known) = app.command_shortcut(a.id).unwrap_or_default();
                    let key_width = key.width();
                    let room = inner.saturating_sub(if key.is_empty() { 0 } else { key_width + 2 });
                    let mut left = String::new();
                    for c in format!("{}{}", a.label, suffix).chars() {
                        if left.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
                            > room
                        {
                            break;
                        }
                        left.push(c);
                    }
                    let pad = inner.saturating_sub(left.width() + key_width);
                    let key_color = match (available.enabled(), known) {
                        (false, _) => Color::DarkGray,
                        (true, true) => Color::Cyan,
                        (true, false) => Color::Gray,
                    };
                    rows.push(ListItem::new(Line::from(vec![
                        Span::styled(left, Style::default().fg(color)),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(key, Style::default().fg(key_color)),
                    ])));
                }
            }
            if rows.is_empty() {
                rows.push(ListItem::new(Line::from(Span::styled(
                    " No matching commands",
                    Style::default().fg(Color::DarkGray),
                ))));
            }
            (
                format!(
                    "Commands: {}  • ↑↓ choose · Enter run · Esc close",
                    menu.query
                ),
                selected_row,
                rows,
            )
        }
        Overlay::DeleteSession(_) => (
            "Remove exited session? · y remove · n/Esc stay".into(),
            None,
            vec![ListItem::new("Enter does not remove the session.")],
        ),
        Overlay::Navigation(_) => (
            "Unsaved changes · y discard and continue · n/Esc stay".into(),
            None,
            vec![ListItem::new(
                "Your editor stays open until you choose. Enter does not discard changes.",
            )],
        ),
        Overlay::Help(selected) => {
            let rows = help_rows()
                .into_iter()
                .map(|row| match row {
                    HelpRow::Intro(line) => ListItem::new(line),
                    HelpRow::Header(theme) => ListItem::new(Line::from(Span::styled(
                        format!(" {} ", theme.title()),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ))),
                    HelpRow::Action(a) => {
                        let availability = app.command_availability(a.id);
                        let label = app.command_label(a.id).unwrap_or_else(|| "Commands".into());
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{label:18}"), Style::default().fg(Color::Cyan)),
                            Span::raw(format!(
                                "{} (candidates: {}){}",
                                a.label,
                                crate::app::shortcut_probe::candidates(a.id),
                                availability
                                    .reason()
                                    .map(|r| format!(" — {r}"))
                                    .unwrap_or_default()
                            )),
                        ]))
                    }
                })
                .collect();
            (
                "Help · ↑↓/PgUp/PgDn scroll · Esc close".into(),
                Some(*selected),
                rows,
            )
        }
    };
    let list = List::new(rows)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = ListState::default().with_selected(selected);
    f.render_stateful_widget(list, area, &mut state);
    if let Overlay::Commands(menu) = overlay {
        use unicode_width::UnicodeWidthStr;
        if area.width > 2 && area.height > 2 {
            let col =
                ("Commands: ".width() + menu.query.width() + 1).min(usize::from(area.width - 2));
            f.set_cursor_position((area.x + col as u16, area.y));
        }
    }
}

/// Render complete controls only, and record their actual geometry for clicks.
pub(super) fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    use crate::keymap::ActionId;
    use ratatui::widgets::Paragraph;
    use unicode_width::UnicodeWidthStr;
    let mut spans = Vec::new();
    let mut controls = Vec::new();
    let mut used = 0usize;
    let width = usize::from(area.width);
    let commands = format!(
        "[{} Commands]",
        app.command_label(ActionId::Commands)
            .unwrap_or_else(|| "F1".into())
    );
    let command_text = if commands.width() + 1 + app.keyboard_focus_short().width() <= width {
        commands
    } else {
        "[F1]".into()
    };
    if command_text.width() <= width {
        controls.push((
            Rect::new(area.x, area.y, command_text.width() as u16, 1),
            ActionId::Commands,
        ));
        used += command_text.width();
        spans.push(Span::styled(
            command_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
        ));
    }
    let focus = app.keyboard_focus_label();
    let focus = if used + 1 + focus.width() <= width {
        format!(" {focus}")
    } else {
        format!(" {}", app.keyboard_focus_short())
    };
    if used + focus.width() <= width {
        used += focus.width();
        spans.push(Span::styled(focus, Style::default().fg(Color::Yellow)));
    }
    let search = app
        .search
        .as_ref()
        .map(|search| {
            let count = if search.matches.is_empty() {
                "no matches".into()
            } else {
                format!("{}/{}", search.current + 1, search.matches.len())
            };
            format!(" Search: {}  {count}  Enter next · Esc close", search.query)
        })
        .or_else(|| match &app.mode {
            crate::app::Mode::TraceBrowser(browser) => browser.search_input.as_ref().map(|input| {
                let label = if browser.zoomed { "Find" } else { "Search" };
                format!(" {label}: {input}▏  Enter search · Esc close")
            }),
            _ => None,
        });
    let status = search
        .as_ref()
        .map(|s| (s.clone(), Color::White))
        .or_else(|| {
            app.notice.as_ref().map(|notice| {
                let color = match notice.level {
                    crate::app::NoticeLevel::Info => Color::Cyan,
                    crate::app::NoticeLevel::Warn => Color::Yellow,
                    crate::app::NoticeLevel::Error => Color::Red,
                };
                (format!(" {}", notice.text), color)
            })
        })
        .or_else(|| {
            (matches!(app.mode, crate::app::Mode::Main) && app.loop_registry.pause_all)
                .then(|| (" LOOPS PAUSED".into(), Color::Yellow))
        });
    if let Some((text, color)) = status {
        // Status messages and editable queries outrank optional shortcuts.
        let remaining = width.saturating_sub(used);
        let mut fitted = String::new();
        for c in text.chars() {
            if fitted.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > remaining {
                break;
            }
            fitted.push(c);
        }
        spans.push(Span::styled(fitted, Style::default().fg(color)));
        if search.is_some()
            && app.command_overlay.is_none()
            && app.shortcut_probe.is_none()
            && remaining > 0
        {
            let query = app
                .search
                .as_ref()
                .map(|s| s.query.as_str())
                .or(match &app.mode {
                    crate::app::Mode::TraceBrowser(b) => b.search_input.as_deref(),
                    _ => None,
                })
                .unwrap_or("");
            let col = (used + " Search: ".width() + query.width()).min(width.saturating_sub(1));
            f.set_cursor_position((area.x + col as u16, area.y));
        }
    } else {
        let mut hints = app.local_hints();
        if matches!(app.mode, crate::app::Mode::Main) && app.list_focused() {
            use crate::app::agents_list::AgentKind;
            let enter = if app.sidebar_section == crate::app::SidebarSection::History {
                "resume"
            } else {
                match app.agent_row() {
                    Some(AgentKind::Harness(_)) => "new session",
                    Some(AgentKind::Session(_)) => "attach",
                    Some(AgentKind::Persona(_)) => "open agent",
                    Some(AgentKind::Loop(_) | AgentKind::Flow(_) | AgentKind::Run(_)) => "open run",
                    Some(AgentKind::Skill(_)) => "launch skill",
                    _ => "open",
                }
            };
            hints = format!(" [Enter] {enter}{hints}");
        }
        if matches!(app.mode, crate::app::Mode::TraceBrowser(_)) {
            hints = format!(" [/] search · [1-5] view{hints}");
        }
        for (id, label) in [
            (ActionId::FocusAgents, "Focus Agents"),
            (ActionId::ReturnToSession, "Return to session"),
        ] {
            let text = format!(" [{label}]");
            if app.command_availability(id).enabled() && used + text.width() <= width {
                controls.push((
                    Rect::new(
                        area.x + used as u16 + 1,
                        area.y,
                        (text.width() - 1) as u16,
                        1,
                    ),
                    id,
                ));
                used += text.width();
                spans.push(Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
        }
        if matches!(app.mode, crate::app::Mode::Main) && app.inbox_count() > 0 {
            let text = format!(" [Inbox: {} need you]", app.inbox_count());
            if used + text.width() <= width {
                controls.push((
                    Rect::new(
                        area.x + used as u16 + 1,
                        area.y,
                        (text.width() - 1) as u16,
                        1,
                    ),
                    ActionId::Inbox,
                ));
                used += text.width();
                spans.push(Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
        }
        // Keep complete hints; navigation controls have priority.

        for piece in hints.split(" [").filter(|s| !s.is_empty()) {
            let text = format!(" [{piece}");
            if used + text.width() <= width {
                used += text.width();
                spans.push(Span::raw(text));
            }
        }
    }
    *app.footer_controls.borrow_mut() = controls;
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
