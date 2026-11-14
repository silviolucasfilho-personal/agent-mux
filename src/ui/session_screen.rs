//! Drawing the session run screen (`crate::app::session_screen`): the
//! session and its sub-agents where the Agents list is, and the selected
//! row in the main pane, in the Run screen's layout. The Live tab is the
//! session's own terminal, drawn by `ui::draw_session_pane` over the whole
//! pane so the PTY keeps its size.

use crate::app::App;
use crate::app::session_screen::{SessionScreen, Tab, card_lines};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn cut(s: &str, max: usize) -> String {
    super::truncate_chars(s, max)
}

/// A row: its text on the left, a word on the right, `width` columns.
fn row(left: &str, right: &str, width: usize) -> String {
    let room = width.saturating_sub(right.width() + 1);
    let left = cut(left, room);
    let gap = width.saturating_sub(left.width() + right.width());
    format!("{left}{}{right}", " ".repeat(gap))
}

/// The tree's rows as text: the session, then its sub-agents.
pub fn tree_rows(st: &SessionScreen, width: usize) -> Vec<String> {
    let c = &st.card;
    let mut rows = vec![row(
        &format!("{} {}  {}", c.kind.glyph(), c.name, c.harness),
        &c.short_stats(),
        width,
    )];
    for a in &st.agents {
        rows.push(row(
            &format!("  ↳ {}", a.name),
            if a.running { "▶" } else { "✓" },
            width,
        ));
    }
    rows
}

pub fn draw_tree(f: &mut Frame, area: Rect, st: &SessionScreen) {
    let focused = !st.pane_focused;
    let room = usize::from(area.width.saturating_sub(4));
    let title = cut(&format!("{} · {}", st.card.name, st.card.state), room);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(focused))
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let visible = usize::from(area.height.saturating_sub(2));
    let rows = tree_rows(st, inner_w.saturating_sub(1));
    let count = rows.len();
    let start = super::sidebar_window(st.cursor, count, visible.max(1));
    let mut items: Vec<ListItem> = rows
        .into_iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(n, text)| {
            let on = n == st.cursor;
            let line = Line::raw(format!("{}{text}", if on { ">" } else { " " }));
            if on && focused {
                ListItem::new(line).style(Style::default().add_modifier(Modifier::REVERSED))
            } else if on {
                ListItem::new(line).style(Style::default().fg(Color::Cyan))
            } else if n == 0 {
                ListItem::new(line).style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(line)
            }
        })
        .collect();
    // the tabs and the screen's own keys, under the rows: the terminal
    // takes the whole pane, so the pane has no room for a tab bar
    let hints: [String; 4] = [
        String::new(),
        Tab::ALL
            .iter()
            .enumerate()
            .map(|(n, t)| {
                if *t == st.tab {
                    format!("[{} {}]", n + 1, t.label())
                } else {
                    format!("{} {}", n + 1, t.label())
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        if st.card.ended {
            "the session has ended".into()
        } else {
            "↵ type in its terminal".into()
        },
        "i back to the session".into(),
    ];
    for h in hints {
        if items.len() < visible {
            items.push(ListItem::new(Line::styled(cut(&h, inner_w), dim())));
        }
    }
    f.render_widget(List::new(items).block(block), area);
}

pub fn draw_pane(f: &mut Frame, area: Rect, st: &SessionScreen, app: &App, now: Instant) {
    let agent = st.selected_agent();
    // the terminal itself, as the Agents list draws it
    if agent.is_none()
        && st.tab == Tab::Live
        && let Some(idx) = app.session_screen_index(st)
    {
        st.max_scroll.set(0);
        super::draw_session_pane(f, area, app, idx, now);
        return;
    }
    let room = usize::from(area.width.saturating_sub(4));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(false))
        .title(Span::styled(
            format!(" {} ", cut(&st.pane_title(), room)),
            Style::default().fg(Color::Gray),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let trace = |st: &SessionScreen| -> Vec<Line<'static>> {
        match &st.trace {
            Some((_, lines)) => lines.iter().map(|l| Line::raw(l.clone())).collect(),
            None => vec![Line::styled(
                "  No traced session: there is no trace to read.",
                dim(),
            )],
        }
    };
    // a sub-agent's row is its own part of the trace
    let (lines, wrap, body) = if agent.is_some() {
        (trace(st), false, inner)
    } else {
        let [tabs, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
        let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
        for (n, t) in Tab::ALL.iter().enumerate() {
            let label = format!(" {} {} ", n + 1, t.label());
            spans.push(if *t == st.tab {
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(label, dim())
            });
            spans.push(Span::raw(" "));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), tabs);
        let (lines, wrap) = match st.tab {
            Tab::Card => (card_lines(&st.card), true),
            Tab::Live => (
                vec![Line::styled(
                    if st.card.ended {
                        "  The session has ended: its terminal is gone. Trace has its turns and tools."
                    } else {
                        "  This run is not a session of this agent-mux: its terminal is not here. Trace has its turns and tools."
                    },
                    dim(),
                )],
                true,
            ),
            Tab::Trace => (trace(st), false),
            Tab::Changes => (
                st.changes
                    .clone()
                    .unwrap_or_else(|| vec![Line::styled("  Reading…", dim())]),
                true,
            ),
        };
        (lines, wrap, body)
    };
    let mut p = Paragraph::new(lines);
    if wrap {
        p = p.wrap(Wrap { trim: false });
    }
    let max = p
        .line_count(body.width)
        .saturating_sub(usize::from(body.height));
    st.max_scroll.set(max);
    let off = u16::try_from(st.scroll.min(max)).unwrap_or(u16::MAX);
    f.render_widget(p.scroll((off, 0)), body);
}
