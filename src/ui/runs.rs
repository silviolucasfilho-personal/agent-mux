//! Drawing the runs view (`crate::app::runs_view`): the runs on the left
//! in their groups, the selected run's tabs on the right, following the
//! design canvas "Agent-first agent-mux" (Runs).

use super::flow::{dim, head, hints, key_style};
use super::{pane_border, truncate_chars};
use crate::app::App;
use crate::app::runs_view::{Group, RunRef, RunTab, RunsViewState, tab_lines};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};

pub fn draw(f: &mut Frame, st: &RunsViewState, app: &App) {
    let full = f.area();
    let area = Rect::new(full.x, full.y, full.width, full.height.saturating_sub(1));
    f.render_widget(Clear, area);
    let [top, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let (runs, tokens, cost) = st.today;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Runs  ", head()),
            Span::styled(
                format!(
                    "every run of every agent · today {runs} run(s) · {} tokens · ${cost:.2}",
                    compact(tokens)
                ),
                dim(),
            ),
        ])),
        top,
    );
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Min(0)]).areas(body);
    draw_list(f, left, st);
    draw_detail(f, right, st, app);
    f.render_widget(Paragraph::new(footer(st)), foot);
}

fn compact(n: i64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn glyph_color(g: &str) -> Color {
    match g {
        "⟳" | "‖" => Color::Cyan,
        "✗" => Color::Red,
        "!" => Color::Yellow,
        "·" => Color::DarkGray,
        _ => Color::Green,
    }
}

fn draw_list(f: &mut Frame, area: Rect, st: &RunsViewState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(true))
        .title(Span::styled(" Runs ", key_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line = 0;
    let mut group: Option<Group> = None;
    for (i, item) in st.items.iter().enumerate() {
        if group != Some(item.group) {
            if group.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(format!(" {}", item.group.label()), dim()));
            group = Some(item.group);
        }
        let on = i == st.selected;
        if on {
            cursor_line = lines.len();
        }
        let right = format!("{} ", item.word);
        let name = format!("{} · {}", item.agent, item.when);
        let room = width.saturating_sub(right.chars().count() + 4);
        let name = truncate_chars(&name, room);
        let pad = width.saturating_sub(name.chars().count() + right.chars().count() + 3);
        let base = if on {
            Style::default()
                .bg(Color::Rgb(31, 58, 64))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", item.glyph),
                base.fg(glyph_color(item.glyph)),
            ),
            Span::styled(name, base),
            Span::styled(" ".repeat(pad), base),
            Span::styled(
                right,
                if item.glyph == "!" {
                    base.fg(Color::Yellow)
                } else if item.glyph == "✗" {
                    base.fg(Color::Red)
                } else {
                    base.fg(Color::DarkGray)
                },
            ),
        ]));
    }
    if st.items.is_empty() {
        lines.push(Line::styled(" no runs yet", dim()));
    }
    if let Some(e) = &st.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" {e}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    let h = usize::from(inner.height);
    let skip = cursor_line.saturating_sub(h.saturating_sub(2));
    f.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), inner);
}

fn draw_detail(f: &mut Frame, area: Rect, st: &RunsViewState, app: &App) {
    let title = st
        .selected_item()
        .map(|i| format!(" {} · {} ", i.agent, i.when))
        .unwrap_or_else(|| " Run ".into());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(Span::styled(title, head()))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let mut spans = Vec::new();
    let flow = st.selected_item().is_some_and(|i| i.run.is_flow());
    for (i, t) in RunTab::ALL.iter().enumerate() {
        let text = format!(" {} {} ", i + 1, t.label(flow));
        spans.push(if *t == st.tab {
            Span::styled(
                text,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(text, dim())
        });
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs);
    // the loop's and the workflow's own detail builders, where they apply
    if let Some(detail) = &st.detail {
        let scroll = clamp_scroll(st, detail, body);
        f.render_widget(
            Paragraph::new(detail.clone())
                .wrap(Wrap { trim: false })
                .scroll((scroll as u16, 0)),
            body,
        );
        return;
    }
    let lines: Vec<Line> = tab_lines(app, st)
        .into_iter()
        .map(|l| {
            if l.starts_with('!') {
                Line::styled(
                    l,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if let Some((k, v)) = l.split_once("  ").filter(|(k, _)| {
                !k.is_empty()
                    && !k.starts_with(' ')
                    && k.chars().all(|c| c.is_ascii_lowercase() || c == ' ')
            }) {
                Line::from(vec![
                    Span::styled(format!("{k}  "), dim()),
                    Span::raw(v.to_string()),
                ])
            } else {
                Line::raw(l)
            }
        })
        .collect();
    let scroll = clamp_scroll(st, &lines, body);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        body,
    );
}

/// The rows `lines` wrap to in `area`; records how far the view can
/// scroll and returns the scroll to draw with.
fn clamp_scroll(st: &RunsViewState, lines: &[Line], area: Rect) -> usize {
    let w = usize::from(area.width).max(1);
    let rows: usize = lines.iter().map(|l| l.width().max(1).div_ceil(w)).sum();
    let max = rows.saturating_sub(usize::from(area.height));
    st.max_scroll.set(max);
    st.scroll.min(max)
}

fn footer(st: &RunsViewState) -> Line<'static> {
    use crate::app::inbox::InboxItem;
    if st.confirm_discard {
        return Line::from(vec![Span::styled(" Discard this plan? [y/n]", key_style())]);
    }
    if let Some(name) = &st.save_name {
        return Line::from(vec![
            Span::styled(" Save the document as: ", key_style()),
            Span::raw(format!("{name}▏")),
            Span::styled("   ↩ save   Esc cancel", dim()),
        ]);
    }
    let own: Vec<(&str, &str)> = match st.selected_item().map(|i| &i.run) {
        Some(RunRef::Inbox(InboxItem::Loop(r))) if r.branch.is_some() => vec![
            ("↩", "apply"),
            ("d", "reject"),
            ("r", "run again"),
            ("p", "pause"),
            ("e", "edit the agent"),
        ],
        Some(RunRef::Inbox(InboxItem::Loop(_))) => vec![
            ("d", "dismiss"),
            ("r", "run again"),
            ("p", "pause"),
            ("e", "edit the agent"),
            ("T", "traces"),
        ],
        Some(RunRef::Inbox(InboxItem::Plan { .. })) => vec![
            ("↩", "run it"),
            ("e", "edit"),
            ("s", "save"),
            ("d", "discard"),
        ],
        Some(RunRef::Inbox(InboxItem::Run { .. })) => vec![
            ("r", "resume"),
            ("s", "save"),
            ("d", "dismiss"),
            ("e", "edit the agent"),
        ],
        Some(RunRef::LoopLive { .. }) => {
            vec![("↩", "attach"), ("x", "stop"), ("e", "edit the agent")]
        }
        Some(RunRef::FlowLive(_)) => vec![
            ("↩", "attach"),
            ("x", "stop"),
            ("s", "save"),
            ("e", "edit the agent"),
        ],
        Some(RunRef::Loop(_)) => vec![
            ("r", "run again"),
            ("p", "pause"),
            ("e", "edit the agent"),
            ("T", "traces"),
        ],
        Some(RunRef::Flow(_)) => vec![("r", "resume"), ("s", "save"), ("e", "edit the agent")],
        Some(RunRef::Next(_)) => vec![("r", "run now"), ("p", "pause"), ("e", "edit the agent")],
        None => vec![],
    };
    let mut v: Vec<(&str, &str)> = vec![("↑↓", "runs"), ("1-5", "tabs")];
    v.extend(own);
    v.push(("⌃D ⌃U", "scroll"));
    v.push(("Esc", "back"));
    hints(&v)
}
