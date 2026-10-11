//! Overlays retain the underlying screen and present its available actions.
use crate::app::{App, commands::Overlay};
use crate::keymap::ACTIONS;
use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{Block, Borders, Clear, List, ListItem, ListState}};

pub(super) fn draw(f: &mut Frame, app: &App) {
    let Some(overlay) = &app.command_overlay else { return };
    let size = f.area();
    let width = size.width.min(84);
    let height = size.height.min(25);
    let area = Rect::new(size.x + (size.width - width)/2, size.y + (size.height - height)/2, width, height);
    f.render_widget(Clear, area);
    let (title, selected, rows): (String, Option<usize>, Vec<ListItem>) = match overlay {
        Overlay::Commands(menu) => {
            let rows = app.filtered_commands().iter().map(|a| {
                let available = app.command_availability(a.id);
                let suffix = available.reason().map(|r| format!(" — {r}")).unwrap_or_default();
                let label = app.command_label(a.id).map(|s| format!(" [{s}]")).unwrap_or_default();
                let color = if available.enabled() { Color::White } else { Color::DarkGray };
                ListItem::new(Line::styled(format!("{}{}{}", a.label, label, suffix), Style::default().fg(color)))
            }).collect();
            (format!("Commands: {}  • ↑↓ choose · Enter run · Esc close", menu.query), Some(menu.selected), rows)
        }
        Overlay::Navigation(_) => ("Unsaved changes · y discard and continue · n/Esc stay".into(), None, vec![ListItem::new("Your editor stays open until you choose. Enter does not discard changes.")]),
        Overlay::Help => {
            let mut rows = vec![ListItem::new("F1 Commands · Esc back · Tab next control · 1–9 numbered tabs"),
                ListItem::new("Session pane: ordinary keys go to the harness"),
                ListItem::new("Use Test shortcuts to verify modifier bindings in this terminal")];
            rows.extend(ACTIONS.iter().map(|a| {
                let availability = app.command_availability(a.id);
                let label = app.command_label(a.id).unwrap_or_else(|| "Commands".into());
                ListItem::new(Line::from(vec![Span::styled(format!("{label:18}"), Style::default().fg(Color::Cyan)),
                    Span::raw(format!("{}{}", a.label, availability.reason().map(|r| format!(" — {r}")).unwrap_or_default()))]))
            }));
            ("Help for this screen · Esc close".into(), None, rows)
        }
    };
    let list = List::new(rows).block(Block::default().title(title).borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)).highlight_symbol("› ");
    let mut state = ListState::default().with_selected(selected);
    f.render_stateful_widget(list, area, &mut state);
}
