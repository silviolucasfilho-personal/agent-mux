//! Drawing the Project screens (`crate::app::project`): the screens and
//! the selected screen's rows on the left, the selected row (or the item
//! screen) on the right.

use super::flow::{dim, head, hints, key_style};
use crate::app::project::{
    Drawn, Focus, ITEM_TABS, Modal, ProjectView, Row, Screen, TAB_AUDIT, TAB_PREFLIGHT, TAB_TRACE,
    TAB_TRAIL, milestone_features, needs_you,
};
use crate::project::contract::*;
use crate::project::doctor::Status;
use crate::project::{DocState, Item, ItemType, Snapshot, cli, order, stage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::collections::BTreeSet;

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn type_color(t: Option<ItemType>) -> Color {
    match t {
        Some(ItemType::Initiative | ItemType::Epic) => Color::Magenta,
        Some(ItemType::Feature) => Color::Blue,
        Some(ItemType::Story) => Color::Green,
        Some(ItemType::Task) => Color::Yellow,
        Some(ItemType::Bug) => Color::Red,
        Some(ItemType::Spike | ItemType::Rfc) | None => Color::DarkGray,
    }
}

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        dim()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title)
}

pub fn draw(f: &mut Frame, area: Rect, v: &ProjectView) {
    f.render_widget(Clear, area);
    v.drawn.set(Drawn::default());
    let [body, input, foot] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(if v.filtering { 1 } else { 0 }),
        Constraint::Length(1),
    ])
    .areas(area);
    let left_width = (body.width * 38 / 100).clamp(32, 52).min(body.width);
    let [left, right] =
        Layout::horizontal([Constraint::Length(left_width), Constraint::Min(0)]).areas(body);
    // the list of screens gives way on a short terminal: the rows of the
    // selected screen keep at least six lines, and the list scrolls
    let nav_height = (Screen::ALL.len() as u16 + 2)
        .min(left.height.saturating_sub(6).max(3))
        .min(left.height);
    let [nav, list] =
        Layout::vertical([Constraint::Length(nav_height), Constraint::Min(0)]).areas(left);
    draw_nav(f, nav, v);
    draw_list(f, list, v);
    if v.item.is_some() {
        draw_item_screen(f, right, v);
    } else {
        draw_pane(f, right, v);
    }
    if v.filtering {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" filter: ", key_style()),
                Span::raw(v.filter.clone()),
                Span::styled("▏", key_style()),
            ])),
            input,
        );
    }
    draw_foot(f, foot, v);
    if let Some(modal) = &v.modal {
        draw_modal(f, body, modal);
    }
}

fn draw_nav(f: &mut Frame, area: Rect, v: &ProjectView) {
    let focused = v.item.is_none() && v.focus == Focus::Nav;
    let hub = v.hub();
    let title = if hub.is_empty() {
        " project ".to_string()
    } else {
        format!(
            " project · {} ",
            cut(&hub, usize::from(area.width).saturating_sub(14))
        )
    };
    let b = block(title, focused);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let width = usize::from(inner.width);
    let lines: Vec<Line> = Screen::ALL
        .into_iter()
        .map(|s| {
            let selected = s == v.screen;
            let count = v.screen_count(s).map(|n| n.to_string()).unwrap_or_default();
            let label = format!("{}{}", if selected { "> " } else { "  " }, s.label());
            let pad = width.saturating_sub(label.chars().count() + count.chars().count() + 1);
            let mut style = Style::default();
            if selected {
                style = style.add_modifier(if focused {
                    Modifier::REVERSED
                } else {
                    Modifier::BOLD
                });
            }
            Line::styled(format!("{label}{}{count} ", " ".repeat(pad)), style)
        })
        .collect();
    // keep the selected screen in sight when the list is cut
    let at = Screen::ALL.iter().position(|s| *s == v.screen).unwrap_or(0);
    let scroll = (at + 1).saturating_sub(usize::from(inner.height)) as u16;
    v.drawn.set(Drawn {
        nav: inner,
        nav_start: usize::from(scroll),
        ..v.drawn.get()
    });
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

/// A stage in a few characters, for the right edge of a row.
fn stage_short(stage: &str) -> String {
    let plain = stage_plain(stage);
    let first = plain.split_whitespace().next().unwrap_or(plain);
    first.chars().take(6).collect()
}

fn draw_list(f: &mut Frame, area: Rect, v: &ProjectView) {
    let focused = v.item.is_none() && v.focus == Focus::List;
    let reading = match (&v.busy, v.syncing()) {
        (Some(what), _) => format!(" · {what}…"),
        (None, true) => " · reading…".to_string(),
        (None, false) => String::new(),
    };
    let needs = v.needs_you();
    let title = format!(
        " {}{}{reading} ",
        v.screen.label(),
        if needs > 0 && v.screen == Screen::Home {
            format!(" · {needs} need you")
        } else {
            String::new()
        }
    );
    let b = block(title, focused);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &v.error {
        for l in wrap(e, width.saturating_sub(2)) {
            lines.push(Line::styled(
                format!(" {l}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let Some(s) = &v.snap else {
        lines.push(Line::styled(
            if v.syncing() {
                " reading the Project from GitHub…"
            } else {
                " not read yet: [Ctrl+R] reads it"
            },
            dim(),
        ));
        f.render_widget(Paragraph::new(lines), inner);
        return;
    };
    if !v.filter.is_empty() && !v.filtering {
        lines.push(Line::styled(format!(" filter: {}", v.filter), dim()));
    }
    let lead = lines.len();
    if v.rows.is_empty() {
        lines.push(Line::styled(
            match v.screen {
                Screen::Health if v.checks.is_none() => " running the checks…",
                Screen::Observability => " the numbers are on the right →",
                Screen::Milestones => " no open milestone",
                _ => " nothing here",
            },
            dim(),
        ));
    }
    let waiting: BTreeSet<u64> = needs_you(s).into_iter().map(|(i, _)| i.number).collect();
    let mut cursor_line = 0;
    for (i, row) in v.rows.iter().enumerate() {
        let selected = i == v.cursor();
        if selected {
            cursor_line = lines.len();
        }
        let marker = if selected { ">" } else { " " };
        let mut spans: Vec<Span> = match row {
            Row::Heading { key, text, count } => {
                let fold = if v.collapsed.contains(key) {
                    "▸"
                } else {
                    "▾"
                };
                let label = cut(text, width.saturating_sub(count.to_string().len() + 6));
                vec![Span::styled(
                    format!("{marker}{fold} {label} ({count})"),
                    head(),
                )]
            }
            Row::Item { number, depth } => match s.item(*number) {
                Some(it) => item_spans(
                    it,
                    *depth,
                    marker,
                    width,
                    v.screen,
                    waiting.contains(number),
                    // a card of the fleet says its state where others say their stage
                    v.dispatches
                        .iter()
                        .find(|d| v.screen == Screen::Development && d.item == *number)
                        .map(|d| d.state.as_str().to_string()),
                ),
                None => vec![Span::styled(format!("{marker}  #{number}"), dim())],
            },
            Row::Milestone(at) => {
                let (name, right) = match at.and_then(|i| s.milestones.get(i)) {
                    Some(m) => {
                        let total = m.open_issues + m.closed_issues;
                        (
                            m.title.clone(),
                            (m.closed_issues * 100)
                                .checked_div(total)
                                .map_or("-".to_string(), |pct| format!("{pct}%")),
                        )
                    }
                    None => ("(no milestone)".to_string(), String::new()),
                };
                let name = cut(&name, width.saturating_sub(right.len() + 4));
                let pad = width.saturating_sub(name.chars().count() + right.len() + 3);
                vec![
                    Span::raw(format!("{marker} {name}")),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(format!("{right} "), dim()),
                ]
            }
            Row::Check(at) => match v.checks.as_ref().and_then(|c| c.get(*at)) {
                Some(c) => vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled(format!("{} ", c.status.mark()), status_style(c.status)),
                    Span::raw(cut(&c.name, width.saturating_sub(5))),
                ],
                None => vec![],
            },
        };
        if selected && focused {
            for sp in &mut spans {
                sp.style = sp.style.add_modifier(Modifier::REVERSED);
            }
        } else if selected {
            for sp in &mut spans {
                sp.style = sp.style.add_modifier(Modifier::BOLD);
            }
        }
        lines.push(Line::from(spans));
    }
    let visible = usize::from(inner.height);
    let start = super::sidebar_window(cursor_line, lines.len(), visible);
    v.drawn.set(Drawn {
        list: inner,
        list_start: start,
        list_lead: lead,
        ..v.drawn.get()
    });
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        inner,
    );
}

fn item_spans(
    it: &Item,
    depth: u8,
    marker: &str,
    width: usize,
    screen: Screen,
    waiting: bool,
    card: Option<String>,
) -> Vec<Span<'static>> {
    let indent = " ".repeat(usize::from(depth) * 2);
    let right = match (card, screen) {
        (Some(state), _) => state,
        (None, Screen::Board) => it.priority.clone().unwrap_or_default(),
        (None, _) => stage_short(&stage::shown(it).stage),
    };
    let bang = if waiting { "!" } else { " " };
    let lead = format!("{marker}{indent}");
    let number = format!("#{} ", it.number);
    let room =
        width.saturating_sub(lead.chars().count() + 2 + number.len() + right.chars().count() + 3);
    let name = cut(it.name(), room);
    let used = lead.chars().count() + 2 + number.len() + name.chars().count();
    let pad = width.saturating_sub(used + right.chars().count() + 2);
    let mut name_style = Style::default();
    if !it.open {
        name_style = dim();
    }
    vec![
        Span::raw(lead),
        Span::styled(
            format!("{} ", it.item_type.map_or("?", ItemType::glyph)),
            Style::default().fg(type_color(it.item_type)),
        ),
        Span::styled(number, dim()),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(bang.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(format!("{right} "), dim()),
    ]
}

fn status_style(s: Status) -> Style {
    Style::default().fg(match s {
        Status::Ok => Color::Green,
        Status::Warn => Color::Yellow,
        Status::Fail => Color::Red,
        Status::Skip => Color::DarkGray,
    })
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// Card and order lines carry their meaning in their first characters;
/// this gives each kind its colour.
fn styled(line: String) -> Line<'static> {
    let t = line.trim_start();
    let style = if t.starts_with("! ") || t.starts_with("blocked") {
        Style::default().fg(Color::Yellow)
    } else if t.starts_with("next:") {
        Style::default().add_modifier(Modifier::BOLD)
    } else if t == "documents"
        || t == "pull requests"
        || t.starts_with("children (")
        || t.starts_with("Feature #")
    {
        head()
    } else {
        Style::default()
    };
    Line::styled(format!(" {line}"), style)
}

fn tabs_line(tabs: &[&str], at: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, t) in tabs.iter().enumerate() {
        let label = format!(" {} {t} ", i + 1);
        spans.push(if i == at {
            Span::styled(label, Style::default().add_modifier(Modifier::REVERSED))
        } else {
            Span::styled(label, dim())
        });
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// What the right side shows: a titled block, its tabs, and lines.
struct Pane<'a> {
    title: String,
    tabs: &'a [&'a str],
    tab: usize,
    lines: Vec<Line<'static>>,
    scroll: u16,
    wrap: bool,
    /// The pane has the keyboard (the item screen).
    focused: bool,
    /// The line that is the first row of the tab's list, when it is one.
    rows_at: Option<u16>,
}

fn render_pane(f: &mut Frame, area: Rect, v: &ProjectView, pane: Pane) {
    let b = block(pane.title, pane.focused);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let [tab_area, _, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(tabs_line(pane.tabs, pane.tab)), tab_area);
    let max = (pane.lines.len() as u16).saturating_sub(1);
    v.drawn.set(Drawn {
        tabs: tab_area,
        content,
        max_scroll: max,
        rows_at: pane.rows_at,
        ..v.drawn.get()
    });
    let mut p = Paragraph::new(pane.lines).scroll((pane.scroll.min(max), 0));
    if pane.wrap {
        p = p.wrap(Wrap { trim: false });
    }
    f.render_widget(p, content);
}

fn board_summary(s: &Snapshot) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled(" items by stage", head()), Line::raw("")];
    let stages: Vec<String> = if s.board_stages.is_empty() {
        STAGE_ORDER.iter().map(|x| x.to_string()).collect()
    } else {
        s.board_stages.clone()
    };
    for st in stages {
        let n = s
            .items
            .iter()
            .filter(|i| i.item_type.is_some() && stage::shown(i).stage == st)
            .filter(|i| i.open || st == STAGE_DONE)
            .count();
        let bar = "█".repeat(n.min(40));
        out.push(Line::from(vec![
            Span::raw(format!(" {:<20} {n:>4} ", cut(&st, 20))),
            Span::styled(bar, Style::default().fg(Color::Blue)),
        ]));
    }
    if let Some(e) = &s.board_error {
        out.push(Line::raw(""));
        out.push(Line::styled(
            format!(" {}", e.lines().next().unwrap_or("")),
            Style::default().fg(Color::Yellow),
        ));
        out.push(Line::styled(
            " stages shown are derived from each issue",
            dim(),
        ));
    }
    out
}

fn doc_glyph(state: DocState) -> (&'static str, Color) {
    match state {
        DocState::Base => ("✓", Color::Green),
        DocState::Pr | DocState::Branch => ("◐", Color::Yellow),
        DocState::Missing => ("·", Color::DarkGray),
        DocState::Unknown => ("?", Color::Red),
    }
}

fn milestone_lines(s: &Snapshot, at: Option<usize>) -> Vec<Line<'static>> {
    let m = at.and_then(|i| s.milestones.get(i));
    let title = m.map(|m| m.title.as_str());
    let mut out = Vec::new();
    match m {
        Some(m) => {
            let total = m.open_issues + m.closed_issues;
            out.push(Line::styled(format!(" {}", m.title), head()));
            out.push(Line::raw(format!(
                " {} of {total} issues closed{}",
                m.closed_issues,
                m.due
                    .as_deref()
                    .map(|d| format!(" · due {}", d.chars().take(10).collect::<String>()))
                    .unwrap_or_default()
            )));
        }
        None => out.push(Line::styled(" Features with no milestone", head())),
    }
    out.push(Line::raw(""));
    out.push(Line::styled(
        "        stage            spec plan dec  qa   Feature",
        dim(),
    ));
    let features = milestone_features(s, title);
    if features.is_empty() {
        out.push(Line::styled(" no Feature", dim()));
    }
    for f in features {
        let shown = stage::shown(f);
        let mut spans = vec![Span::raw(format!(
            " #{:<5} {:<16} ",
            f.number,
            cut(stage_plain(&shown.stage), 16)
        ))];
        for doc in ["spec", "plan", "decomposition", "qa-plan"] {
            let (g, c) = doc_glyph(f.doc_state(doc));
            spans.push(Span::styled(format!(" {g}   "), Style::default().fg(c)));
        }
        let style = if f.open { Style::default() } else { dim() };
        spans.push(Span::styled(f.name().to_string(), style));
        out.push(Line::from(spans));
    }
    out.push(Line::raw(""));
    out.push(Line::styled(
        " ✓ on the default branch   ◐ in review   · not written   ? not read",
        dim(),
    ));
    out
}

/// A read tab of a milestone (QA trail, Preflight, Audit): what was read,
/// or that it is being read.
fn milestone_read_lines(
    v: &ProjectView,
    s: &Snapshot,
    at: Option<usize>,
    tab: usize,
) -> Vec<Line<'static>> {
    let Some(m) = at.and_then(|i| s.milestones.get(i)) else {
        return vec![Line::styled(
            " Features with no milestone have no trail, preflight or audit",
            dim(),
        )];
    };
    let mut out: Vec<Line> = match v.milestone_read(m.number, tab) {
        Some(r) => match &r.lines {
            Ok(lines) => lines.iter().map(|l| styled(format!(" {l}"))).collect(),
            Err(why) => vec![Line::styled(
                format!(" not read: {why}"),
                Style::default().fg(Color::Red),
            )],
        },
        None => {
            return vec![Line::styled(
                match tab {
                    TAB_PREFLIGHT => " Reading the environment from GitHub and this machine…",
                    TAB_AUDIT => " Reading the specs of the open Features…",
                    _ => " Reading the cycles…",
                },
                dim(),
            )];
        }
    };
    out.push(Line::raw(""));
    out.push(Line::styled(
        match tab {
            TAB_PREFLIGHT => " [r] read again · nothing is written",
            TAB_AUDIT => {
                " [r] read again · [C] the set critique, one read-only session over every spec"
            }
            _ => " [r] read again · [Q] the trail's next command",
        },
        dim(),
    ));
    out
}

fn order_of_row(v: &ProjectView, s: &Snapshot) -> Vec<Line<'static>> {
    let feature = match v.selected_row() {
        Some(Row::Heading { key, .. }) => key.strip_prefix("feature:").and_then(|n| n.parse().ok()),
        Some(Row::Item { number, .. }) => s
            .ancestor(*number, ItemType::Feature)
            .or_else(|| s.ancestor(*number, ItemType::Rfc))
            .map(|f| f.number),
        _ => None,
    };
    match feature.map(|n| order::feature_order(s, n)) {
        Some(Ok(o)) => cli::order_lines(&o).into_iter().map(styled).collect(),
        Some(Err(e)) => vec![Line::styled(format!(" {e}"), dim())],
        None => vec![Line::styled(" this row belongs to no Feature", dim())],
    }
}

fn draw_pane(f: &mut Frame, area: Rect, v: &ProjectView) {
    let tabs = v.screen.tabs();
    let tab = v.tab.min(tabs.len().saturating_sub(1));
    let Some(s) = &v.snap else {
        let b = block(" project ".to_string(), false);
        let inner = b.inner(area);
        f.render_widget(b, area);
        let text = match (&v.error, v.syncing()) {
            (Some(_), _) => {
                "Open this screen from a session in a repository that has .spec-wave.json."
            }
            (None, true) => "Reading issues, board, milestones and documents from GitHub…",
            (None, false) => "Nothing was read yet.",
        };
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {text}"), dim())).wrap(Wrap { trim: false }),
            inner,
        );
        return;
    };
    let mut wrap_text = false;
    let (title, lines): (String, Vec<Line>) = match (v.screen, v.selected_row()) {
        (Screen::Observability, _) => {
            let which = &v.activity[tab.min(1)];
            let lines = if which.is_empty() {
                vec![Line::styled(" reading the trace store…", dim())]
            } else {
                which
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        // the totals, and each section's title
                        if i == 0 || (!l.starts_with(' ') && !l.is_empty()) {
                            Line::styled(format!(" {l}"), head())
                        } else {
                            Line::raw(format!(" {l}"))
                        }
                    })
                    .collect()
            };
            (" what the steps cost ".into(), lines)
        }
        (Screen::Health, Some(Row::Check(at))) => {
            wrap_text = true;
            match v.checks.as_ref().and_then(|c| c.get(*at)) {
                Some(c) => (
                    format!(" {} ", c.name),
                    vec![
                        Line::from(vec![
                            Span::styled(format!(" {} ", c.status.mark()), status_style(c.status)),
                            Span::styled(c.name.clone(), head()),
                        ]),
                        Line::raw(""),
                        Line::raw(format!(" {}", c.detail)),
                    ],
                ),
                None => (" health ".into(), vec![]),
            }
        }
        (Screen::Milestones, Some(Row::Milestone(at))) => {
            let name = at
                .and_then(|i| s.milestones.get(i))
                .map_or("no milestone".to_string(), |m| m.title.clone());
            let lines = if tab == 0 {
                milestone_lines(s, *at)
            } else if tab >= TAB_TRAIL {
                milestone_read_lines(v, s, *at, tab)
            } else {
                let title = at
                    .and_then(|i| s.milestones.get(i))
                    .map(|m| m.title.as_str());
                let orders: Vec<order::FeatureOrder> = match title {
                    Some(t) => order::map(s, Some(t)),
                    None => order::map(s, None)
                        .into_iter()
                        .filter(|o| o.milestone.is_none())
                        .collect(),
                };
                let mut out = Vec::new();
                for o in &orders {
                    out.extend(cli::order_lines(o).into_iter().map(styled));
                    out.push(Line::raw(""));
                }
                if out.is_empty() {
                    out.push(Line::styled(" no Feature here has Stories yet", dim()));
                }
                out
            };
            (format!(" {name} "), lines)
        }
        (Screen::Ready, Some(_)) if tab == 0 => (" order ".into(), order_of_row(v, s)),
        (_, Some(Row::Item { number, .. })) => match s.item(*number) {
            Some(it) => {
                let mut lines: Vec<Line> = cli::card_lines(it, s).into_iter().map(styled).collect();
                lines.extend(sessions_line(v, it.number));
                if let Some(d) = v.dispatches.iter().find(|d| d.item == it.number) {
                    lines.push(Line::raw(""));
                    lines.push(Line::styled(
                        format!(
                            " fleet · {}: {}",
                            d.column().label(),
                            d.line(crate::project::now_ns())
                        ),
                        Style::default().fg(Color::Yellow),
                    ));
                    if let Some(log) = &d.log {
                        lines.push(Line::styled(format!("   output: {log}"), dim()));
                    }
                }
                (format!(" #{} ", it.number), lines)
            }
            None => (" item ".into(), vec![]),
        },
        (Screen::Board, _) => (" board ".into(), board_summary(s)),
        (_, Some(Row::Heading { text, count, .. })) => (
            format!(" {} ", cut(text, 40)),
            vec![
                Line::styled(format!(" {text}"), head()),
                Line::raw(""),
                Line::raw(format!(" {count} item(s)")),
            ],
        ),
        _ => {
            let mut lines = vec![Line::styled(format!(" {}", s.hub), head()), Line::raw("")];
            let open = s.items.iter().filter(|i| i.open).count();
            lines.push(Line::raw(format!(
                " {} issues read, {open} open · {} milestones",
                s.items.len(),
                s.milestones.len()
            )));
            for w in &s.warnings {
                lines.push(Line::styled(
                    format!(" ! {}", w.lines().next().unwrap_or("")),
                    Style::default().fg(Color::Yellow),
                ));
            }
            (" project ".into(), lines)
        }
    };
    render_pane(
        f,
        area,
        v,
        Pane {
            title,
            tabs,
            tab,
            lines,
            scroll: v.scroll,
            wrap: wrap_text,
            focused: false,
            rows_at: None,
        },
    );
}

fn list_tab(lines: Vec<(String, Style)>, cursor: usize, empty: &str) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::styled(format!(" {empty}"), dim())];
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, (text, style))| {
            let marker = if i == cursor { ">" } else { " " };
            let style = if i == cursor {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            Line::styled(format!("{marker} {text} "), style)
        })
        .collect()
}

fn draw_item_screen(f: &mut Frame, area: Rect, v: &ProjectView) {
    let (Some(screen), Some(s)) = (&v.item, &v.snap) else {
        return;
    };
    let Some(it) = s.item(screen.number) else {
        render_pane(
            f,
            area,
            v,
            Pane {
                title: format!(" #{} ", screen.number),
                tabs: &ITEM_TABS,
                tab: screen.tab,
                lines: vec![Line::styled(
                    " this issue is no longer in the Project",
                    dim(),
                )],
                scroll: 0,
                wrap: false,
                focused: true,
                rows_at: None,
            },
        );
        return;
    };
    let mut wrap_text = false;
    let mut scroll = screen.scroll;
    let lines: Vec<Line> = match screen.tab {
        0 => {
            let mut lines: Vec<Line> = cli::card_lines(it, s).into_iter().map(styled).collect();
            lines.extend(sessions_line(v, it.number));
            // what a refine or an estimate proposed, waiting for a yes
            for p in v.proposals.iter().filter(|p| p.number == it.number) {
                let key = match p.kind {
                    crate::project::assist::Kind::Estimate => "E",
                    crate::project::assist::Kind::Refine => "W",
                };
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!(" a proposal waits · [{key}] writes it after you read it"),
                    Style::default().fg(Color::Yellow),
                ));
                for l in crate::project::assist::lines(p, true) {
                    lines.push(Line::raw(format!("   {l}")));
                }
            }
            lines
        }
        1 => {
            scroll = 0;
            list_tab(
                it.docs
                    .iter()
                    .map(|(name, d)| {
                        let (g, c) = doc_glyph(d.state);
                        let extra = match (d.pr, &d.branch) {
                            (Some(pr), _) => format!("  pull request #{pr}"),
                            (None, Some(b)) => format!("  branch {b}"),
                            _ => String::new(),
                        };
                        (
                            format!("{g} {name:<14} {:<8} {}{extra}", d.state.as_str(), d.path),
                            Style::default().fg(c),
                        )
                    })
                    .collect(),
                screen.cursor,
                "this type has no documents",
            )
        }
        2 => {
            scroll = 0;
            list_tab(
                s.children_of(it.number)
                    .into_iter()
                    .map(|k| {
                        (
                            cli::row_line(k),
                            if k.open { Style::default() } else { dim() },
                        )
                    })
                    .collect(),
                screen.cursor,
                "no children",
            )
        }
        3 => {
            scroll = 0;
            list_tab(
                it.pulls
                    .iter()
                    .map(|p| {
                        (
                            format!(
                                "{}#{}  {}{}{}  {} → {}",
                                p.repo,
                                p.number,
                                p.state.to_lowercase(),
                                if p.draft { " (draft)" } else { "" },
                                if p.review.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {}", p.review.to_lowercase().replace('_', " "))
                                },
                                p.head,
                                p.base
                            ),
                            Style::default().fg(if p.merged() {
                                Color::Magenta
                            } else if p.open() {
                                Color::Green
                            } else {
                                Color::DarkGray
                            }),
                        )
                    })
                    .collect(),
                screen.cursor,
                "no pull request is linked",
            )
        }
        TAB_TRACE => {
            scroll = 0;
            trace_tab(v, it.number, screen.cursor)
        }
        _ => {
            wrap_text = true;
            match &v.thread {
                Some(t) if t.number == it.number => match &t.lines {
                    Ok(lines) => lines
                        .iter()
                        .map(|l| {
                            if l.starts_with("── ") || l.starts_with('@') {
                                Line::styled(format!(" {l}"), head())
                            } else {
                                Line::raw(format!(" {l}"))
                            }
                        })
                        .collect(),
                    Err(e) => vec![Line::styled(
                        format!(" the thread was not read: {e}"),
                        Style::default().fg(Color::Yellow),
                    )],
                },
                _ => vec![Line::styled(" reading the issue's thread…", dim())],
            }
        }
    };
    let title = format!(
        " {} #{} · {} ",
        it.item_type.map_or("issue", ItemType::as_str),
        it.number,
        cut(it.name(), usize::from(area.width).saturating_sub(24))
    );
    render_pane(
        f,
        area,
        v,
        Pane {
            title,
            tabs: &ITEM_TABS,
            tab: screen.tab,
            lines,
            scroll,
            wrap: wrap_text,
            focused: true,
            rows_at: match screen.tab {
                1..=3 => Some(0),
                // the totals and a blank line come first
                TAB_TRACE => Some(2),
                _ => None,
            },
        },
    );
}

/// What an item's sessions add up to, as the card's last line; nothing
/// when no step ran a session for it or for what is under it.
fn sessions_line(v: &ProjectView, number: u64) -> Option<Line<'static>> {
    let text = v
        .totals
        .get(&number)
        .and_then(crate::tracing::project_report::card_line)?;
    Some(styled(text))
}

/// The Trace tab: the runs of the item's steps, and the sessions of the
/// selected one.
fn trace_tab(v: &ProjectView, number: u64, cursor: usize) -> Vec<Line<'static>> {
    use crate::tracing::cli::fmt_tokens;
    use crate::tracing::project_report::{run_line, session_line};
    use crate::tracing::swarm_report::cost_label;
    let Some(t) = v.trace.as_ref().filter(|t| t.number == number) else {
        return vec![Line::styled(" reading the trace store…", dim())];
    };
    let r = match &t.report {
        Ok(r) => r,
        Err(e) => {
            return vec![Line::styled(
                format!(" the trace was not read: {e}"),
                Style::default().fg(Color::Yellow),
            )];
        }
    };
    let mut lines = vec![
        Line::styled(
            format!(
                " {} session(s) in {} run(s) · {} tok · {}{}",
                r.launches.len(),
                t.runs.len(),
                fmt_tokens(r.tokens),
                cost_label(r.cost_usd, r.unpriced),
                if r.items.len() > 1 {
                    format!(" · with the {} item(s) under it", r.items.len() - 1)
                } else {
                    String::new()
                }
            ),
            head(),
        ),
        Line::raw(""),
    ];
    lines.extend(list_tab(
        t.runs
            .iter()
            .map(|x| {
                (
                    run_line(x, t.read_ns),
                    if x.ended_ns.is_some() {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Green)
                    },
                )
            })
            .collect(),
        cursor,
        "no step of this item has run a session yet (steps from before sessions carried the item's tag are not listed)",
    ));
    if let Some(x) = t.runs.get(cursor) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(
                " run {}",
                if x.run.is_empty() {
                    "?"
                } else {
                    x.run.as_str()
                }
            ),
            dim(),
        ));
        for at in &x.sessions {
            if let Some(l) = r.launches.get(*at) {
                lines.push(Line::raw(format!("   {}", session_line(l, t.read_ns))));
            }
        }
    }
    lines
}

/// A dialog over the middle of the screen.
fn draw_modal(f: &mut Frame, area: Rect, modal: &Modal) {
    let (title, mut lines, hint): (String, Vec<Line>, Line) = match modal {
        Modal::Pick {
            title,
            options,
            cursor,
            ..
        } => (
            format!(" {title} "),
            options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    if i == *cursor {
                        Line::styled(
                            format!(" > {o} "),
                            Style::default().add_modifier(Modifier::REVERSED),
                        )
                    } else {
                        Line::raw(format!("   {o} "))
                    }
                })
                .collect(),
            hints(&[("↑↓", "choose"), ("Enter", "next"), ("Esc", "cancel")]),
        ),
        Modal::Text { label, text, .. } => (
            format!(" {label} "),
            vec![Line::from(vec![
                Span::raw(format!(" {text}")),
                Span::styled("▏", key_style()),
            ])],
            hints(&[("Enter", "next"), ("Esc", "cancel")]),
        ),
        Modal::New(form) => {
            let field = |i: usize, name: &str, value: String, choice: bool| {
                let on = form.field == i;
                let shown = if choice {
                    format!("‹ {value} ›")
                } else if on {
                    format!("{value}▏")
                } else {
                    value
                };
                Line::from(vec![
                    Span::styled(
                        format!(" {} {name:<9}", if on { ">" } else { " " }),
                        if on { key_style() } else { dim() },
                    ),
                    Span::raw(shown),
                ])
            };
            let t = crate::project::ItemType::ALL
                .get(form.item_type)
                .map_or("Feature", |t| t.as_str());
            let priority = form
                .priority
                .checked_sub(1)
                .and_then(|i| PRIORITIES.get(i))
                .copied()
                .unwrap_or("none");
            (
                " New work item ".to_string(),
                vec![
                    field(0, "type", t.to_string(), true),
                    field(1, "title", form.title.clone(), false),
                    field(
                        2,
                        "parent",
                        if form.parent.is_empty() && form.field != 2 {
                            "(none)".to_string()
                        } else {
                            format!("#{}", form.parent)
                        },
                        false,
                    ),
                    field(3, "priority", priority.to_string(), true),
                ],
                hints(&[
                    ("Tab", "field"),
                    ("←→", "choose"),
                    ("Enter", "next"),
                    ("Esc", "cancel"),
                ]),
            )
        }
        Modal::Edit(form) => {
            let field = |i: usize, name: &str, value: String, choice: bool| {
                let on = form.field == i;
                let shown = if choice {
                    format!("‹ {value} ›")
                } else if on {
                    format!("{value}▏")
                } else {
                    value
                };
                Line::from(vec![
                    Span::styled(
                        format!(" {} {name:<10}", if on { ">" } else { " " }),
                        if on { key_style() } else { dim() },
                    ),
                    Span::raw(shown),
                ])
            };
            let priority = form
                .priority
                .checked_sub(1)
                .and_then(|i| PRIORITIES.get(i))
                .copied()
                .unwrap_or("none");
            let milestone = form
                .milestones
                .get(form.milestone)
                .cloned()
                .unwrap_or_else(|| "none".to_string());
            (
                format!(" Edit #{} ", form.number),
                vec![
                    field(0, "title", form.title.clone(), false),
                    field(1, "priority", priority.to_string(), true),
                    field(2, "milestone", milestone, true),
                    Line::from(Span::styled(
                        "   the description opens in your editor, in place of this form",
                        dim(),
                    )),
                ],
                hints(&[
                    ("Tab", "field"),
                    ("←→", "choose"),
                    ("Enter", "review"),
                    ("Ctrl+O", "description"),
                    ("Esc", "cancel"),
                ]),
            )
        }
        Modal::Confirm { lines, .. } => (
            " This writes to GitHub ".to_string(),
            lines.iter().map(|l| Line::raw(format!(" {l}"))).collect(),
            hints(&[("y", "do it"), ("n", "cancel")]),
        ),
    };
    lines.push(Line::raw(""));
    lines.push(hint);
    let width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(20)
        .max(title.chars().count())
        .saturating_add(4)
        .min(usize::from(area.width).saturating_sub(4))
        .max(24) as u16;
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width.min(area.width),
        height,
    );
    f.render_widget(Clear, rect);
    let b = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .title(title);
    let inner = b.inner(rect);
    f.render_widget(b, rect);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_foot(f: &mut Frame, area: Rect, v: &ProjectView) {
    let line = if v.filtering {
        hints(&[("Enter", "keep"), ("Esc", "clear")])
    } else if let Some(screen) = &v.item {
        let enter = match screen.tab {
            1 => "open document",
            2 => "open child",
            3 => "open pull request",
            TAB_TRACE => "open run",
            _ => "",
        };
        let mut pairs = vec![("←→ 1-6", "tabs")];
        if !enter.is_empty() {
            pairs.push(("↑↓", "move"));
            pairs.push(("Enter", enter));
        }
        pairs.extend([
            ("m", "move"),
            ("e", "edit"),
            ("c", "chat"),
            ("a", "agent"),
            ("E", "estimate"),
            ("W", "refine"),
            ("o", "issue in browser"),
            ("Ctrl+R", "reload"),
            ("Esc", "back"),
        ]);
        hints(&pairs)
    } else {
        let mut pairs = vec![
            ("[ ]", "screen"),
            ("Enter", "open"),
            ("r", "run next step"),
            ("n", "new"),
            ("e", "edit"),
            ("m", "move"),
        ];
        if v.screen == Screen::Uat {
            pairs.push(("Q", "run QA / plan"));
        }
        if v.screen == Screen::Milestones {
            pairs.extend([("←→ 1-5", "tabs"), ("Q", "QA trail"), ("C", "set critique")]);
        }
        if v.screen == Screen::Development {
            pairs.extend([("d", "dispatch"), ("x", "take back")]);
        } else if v.screen == Screen::CodeReview {
            pairs.extend([("f", "fix review"), ("P", "merge")]);
        } else if v.screen == Screen::Bugs {
            pairs.extend([("t", "accept"), ("R", "reject"), ("D", "duplicate")]);
        } else {
            pairs.extend([("M", "repair"), ("A", "catch up board")]);
        }
        pairs.extend([
            ("I", "insights"),
            ("/", "filter"),
            ("o", "browser"),
            ("Ctrl+R", "reload"),
            ("Esc", "close"),
        ]);
        hints(&pairs)
    };
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{self, tests as fixture};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen_text(v: &ProjectView, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), v)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn view() -> ProjectView {
        let dir = tempfile::tempdir().unwrap();
        let mut v = ProjectView::new(dir.path().to_path_buf());
        v.cfg = Some(fixture::cfg());
        v.error = None;
        v.snap = Some(snapshot::build(&fixture::raw(), &fixture::cfg(), 1));
        v.rebuild();
        v
    }

    use crate::app::project::{Outcome, tab_at};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    const W: u16 = 120;
    const H: u16 = 34;

    /// Where a text is on the drawn screen, left of column `max_x`.
    fn find(v: &ProjectView, needle: &str, min_x: u16, max_x: u16) -> (u16, u16) {
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        term.draw(|f| draw(f, f.area(), v)).unwrap();
        let buf = term.backend().buffer().clone();
        for y in 0..H {
            for x in min_x..max_x.min(W) {
                let rest: String = (x..W).map(|c| buf[(c, y)].symbol()).collect();
                if rest.starts_with(needle) {
                    return (x, y);
                }
            }
        }
        panic!("{needle:?} is not drawn:\n{}", screen_text(v, W, H));
    }

    fn mouse(v: &mut ProjectView, kind: MouseEventKind, at: (u16, u16)) -> Outcome {
        v.handle_mouse(&MouseEvent {
            kind,
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn click(v: &mut ProjectView, at: (u16, u16)) -> Outcome {
        mouse(v, MouseEventKind::Down(MouseButton::Left), at)
    }

    /// The column where the right side starts.
    fn split(v: &ProjectView) -> u16 {
        screen_text(v, W, H);
        v.drawn.get().list.right() + 1
    }

    fn row_of(v: &ProjectView, number: u64) -> usize {
        v.rows
            .iter()
            .position(|r| matches!(r, Row::Item { number: n, .. } if *n == number))
            .unwrap()
    }

    #[test]
    fn a_click_chooses_a_screen_a_row_and_a_tab() {
        let mut v = view();
        let mid = split(&v);
        // a screen, by its name in the list of screens
        let at = find(&v, "Board", 0, mid);
        assert_eq!(click(&mut v, at), Outcome::None);
        assert_eq!((v.screen, v.focus), (Screen::Board, Focus::Nav));

        // a row: the first click chooses it, the second opens it
        let numbers: Vec<u64> = v
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Item { number, .. } => Some(*number),
                _ => None,
            })
            .collect();
        let number = *numbers.last().unwrap();
        assert_ne!(v.cursor(), row_of(&v, number));
        let at = find(&v, &format!("#{number} "), 0, mid);
        click(&mut v, at);
        assert_eq!(v.cursor(), row_of(&v, number));
        assert_eq!(v.focus, Focus::List);
        assert!(v.item.is_none());
        click(&mut v, at);
        assert_eq!(v.item.as_ref().map(|i| i.number), Some(number));

        // a tab of the item screen; Thread is read when it opens
        let at = find(&v, "3 Children", mid, W);
        click(&mut v, at);
        assert_eq!(v.item.as_ref().unwrap().tab, 2);
        let at = find(&v, "5 Thread", mid, W);
        assert_eq!(click(&mut v, at), Outcome::LoadThread(number));

        // a row of the list under an open item closes the item
        let other = numbers[0];
        let at = find(&v, &format!("#{other} "), 0, mid);
        click(&mut v, at);
        assert!(v.item.is_none());
        assert_eq!(v.cursor(), row_of(&v, other));

        // a tab of a screen
        let tabs = v.screen.tabs();
        if tabs.len() > 1 {
            let at = find(&v, &format!("2 {}", tabs[1]), mid, W);
            click(&mut v, at);
            assert_eq!(v.tab, 1);
        }

        // a heading folds on one click
        let (key, text) = v
            .rows
            .iter()
            .find_map(|r| match r {
                Row::Heading { key, text, .. } => Some((key.clone(), text.clone())),
                _ => None,
            })
            .unwrap();
        let folded = v.collapsed.contains(&key);
        // a heading is drawn as its text and its count
        let word = text.split_whitespace().last().unwrap();
        let at = find(&v, &format!("{word} ("), 0, mid);
        click(&mut v, at);
        assert_ne!(v.collapsed.contains(&key), folded);
    }

    #[test]
    fn a_click_in_an_items_list_chooses_then_opens() {
        let mut v = view();
        let snap = v.snap.clone().unwrap();
        let parent = snap
            .items
            .iter()
            .find(|i| snap.children_of(i.number).len() > 1)
            .unwrap()
            .number;
        let second = snap.children_of(parent)[1].number;
        v.item = Some(crate::app::project::ItemScreen {
            number: parent,
            tab: 2,
            cursor: 0,
            scroll: 0,
            back: Vec::new(),
        });
        let mid = split(&v);
        let at = find(&v, &format!("#{second} "), mid, W);
        click(&mut v, at);
        assert_eq!(v.item.as_ref().unwrap().cursor, 1);
        assert_eq!(v.item.as_ref().unwrap().number, parent);
        click(&mut v, at);
        let open = v.item.as_ref().unwrap();
        assert_eq!((open.number, open.back.clone()), (second, vec![parent]));

        // the wheel over a list moves its cursor and stops at its ends
        v.item.as_mut().unwrap().number = parent;
        v.item.as_mut().unwrap().tab = 2;
        v.item.as_mut().unwrap().cursor = 0;
        screen_text(&v, W, H);
        let last = snap.children_of(parent).len() - 1;
        for _ in 0..last + 3 {
            mouse(&mut v, MouseEventKind::ScrollDown, at);
        }
        assert_eq!(v.item.as_ref().unwrap().cursor, last);
        // and over the lists on the left it moves nothing under an item
        let before = v.cursor();
        mouse(&mut v, MouseEventKind::ScrollDown, (2, H - 6));
        assert_eq!(v.cursor(), before);
    }

    #[test]
    fn the_wheel_moves_what_it_is_over_and_a_dialog_takes_no_click() {
        let mut v = view();
        v.screen = Screen::Board;
        v.rebuild();
        let mid = split(&v);
        let d = v.drawn.get();
        // the rows
        let on_list = (d.list.x + 1, d.list.y + 1);
        mouse(&mut v, MouseEventKind::ScrollDown, on_list);
        assert_eq!(v.cursor(), 1);
        mouse(&mut v, MouseEventKind::ScrollUp, on_list);
        mouse(&mut v, MouseEventKind::ScrollUp, on_list);
        assert_eq!(v.cursor(), 0);
        // the right side scrolls no further than its last line
        let on_pane = (mid + 4, d.content.y + 1);
        for _ in 0..200 {
            mouse(&mut v, MouseEventKind::ScrollDown, on_pane);
        }
        assert_eq!(v.scroll, d.max_scroll);
        mouse(&mut v, MouseEventKind::ScrollUp, on_pane);
        assert_eq!(v.scroll, d.max_scroll.saturating_sub(3));
        // the list of screens, which stops at its ends
        let on_nav = (d.nav.x + 1, d.nav.y);
        mouse(&mut v, MouseEventKind::ScrollUp, on_nav);
        assert_eq!(v.screen, Screen::ALL[0]);
        mouse(&mut v, MouseEventKind::ScrollUp, on_nav);
        assert_eq!(v.screen, Screen::ALL[0]);
        for _ in 0..Screen::ALL.len() + 2 {
            mouse(&mut v, MouseEventKind::ScrollDown, on_nav);
        }
        assert_eq!(v.screen, *Screen::ALL.last().unwrap());

        // a dialog is answered on the keyboard
        v.modal = Some(Modal::Confirm {
            lines: vec!["write?".into()],
            op: crate::app::project::Op::Advance(Vec::new()),
        });
        screen_text(&v, W, H);
        assert_eq!(click(&mut v, on_nav), Outcome::None);
        assert_eq!(v.screen, *Screen::ALL.last().unwrap());
        assert!(v.modal.is_some());
    }

    #[test]
    fn a_column_of_the_line_of_tabs_is_the_tab_drawn_there() {
        for tabs in [
            &ITEM_TABS[..],
            Screen::Milestones.tabs(),
            Screen::Home.tabs(),
        ] {
            let mut x = 0u16;
            for (i, span) in tabs_line(tabs, 0).spans.iter().enumerate() {
                let width = span.content.chars().count() as u16;
                // spans: a space, then a label and a space for each tab
                let want = (i % 2 == 1).then_some(i / 2);
                for col in x..x + width {
                    assert_eq!(tab_at(tabs, col), want, "{tabs:?} column {col}");
                }
                x += width;
            }
            assert_eq!(tab_at(tabs, x + 3), None);
        }
    }

    #[test]
    fn a_folder_with_no_configuration_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let v = ProjectView::new(dir.path().to_path_buf());
        let text = screen_text(&v, 110, 20);
        assert!(text.contains("not a spec-wave"), "{text}");
        assert!(text.contains(".spec-wave.json"));
    }

    #[test]
    fn every_screen_draws_at_two_sizes() {
        let mut v = view();
        for screen in Screen::ALL {
            v.screen = screen;
            v.rebuild();
            for (w, h) in [(120, 34), (64, 14)] {
                let text = screen_text(&v, w, h);
                assert!(text.contains(screen.label()), "{screen:?} at {w}x{h}");
            }
        }
    }

    #[test]
    fn the_board_lists_stages_and_the_card_of_the_selected_item() {
        let mut v = view();
        v.screen = Screen::Board;
        v.rebuild();
        let at = v
            .rows
            .iter()
            .position(|r| matches!(r, Row::Item { number: 1, .. }))
            .unwrap();
        v.cursors[1] = at;
        let text = screen_text(&v, 150, 44);
        // the Feature, and the Story whose stage is derived
        assert!(text.contains("Ready (2)"), "{text}");
        assert!(text.contains("Triagem (1)"));
        assert!(text.contains("Feature #1  Cadastro de Pedidos"));
        assert!(text.contains("blocked (pr-pending)"));
        // the Feature waits on the operator
        assert!(text.contains("acme/hub"));
    }

    #[test]
    fn the_item_screen_lists_documents_children_and_pulls() {
        let mut v = view();
        v.item = Some(crate::app::project::ItemScreen {
            number: 1,
            tab: 1,
            cursor: 0,
            scroll: 0,
            back: vec![],
        });
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("Documents"));
        assert!(
            text.contains("docs/features/cadastro-de-pedidos/spec.md"),
            "{text}"
        );
        v.item.as_mut().unwrap().tab = 2;
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("ver pedidos") && text.contains("cancelar pedido"));
        v.item.as_mut().unwrap().number = 2;
        v.item.as_mut().unwrap().tab = 3;
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("acme/api#12  open (draft)"), "{text}");
        v.item.as_mut().unwrap().tab = 4;
        assert!(screen_text(&v, 150, 30).contains("reading the issue's thread"));
    }

    #[test]
    fn a_waiting_proposal_is_on_the_items_card_with_the_key_that_writes_it() {
        use crate::project::assist::{Kind, Proposal};
        let mut v = view();
        v.item = Some(crate::app::project::ItemScreen {
            number: 2,
            tab: 0,
            cursor: 0,
            scroll: 0,
            back: vec![],
        });
        let text = screen_text(&v, 150, 40);
        assert!(!text.contains("a proposal waits"), "{text}");
        assert!(text.contains("estimate") && text.contains("refine") && text.contains("chat"));
        v.proposals = vec![Proposal {
            kind: Kind::Refine,
            hub: "acme/hub".into(),
            number: 2,
            title: "[STORY] ver pedidos".into(),
            created_ns: 0,
            harness: "claude".into(),
            body: Some("## Objetivo\nVer os pedidos do mês".into()),
            base_hash: Some("h".into()),
            changes: vec!["organizado em seções".into()],
            questions: vec![],
            points: None,
            answered: None,
            rationale: String::new(),
            risks: vec![],
        }];
        let text = screen_text(&v, 150, 40);
        assert!(text.contains("a proposal waits · [W] writes it"), "{text}");
        assert!(text.contains("change: organizado em seções"), "{text}");
        assert!(text.contains("Ver os pedidos do mês"), "{text}");
        // a proposal of another item is not shown here
        v.proposals[0].number = 3;
        assert!(!screen_text(&v, 150, 40).contains("a proposal waits"));
    }

    #[test]
    fn the_development_screen_is_a_kanban_once_something_is_dispatched() {
        use crate::project::fleet::{Dispatch, State};
        let mut v = view();
        v.screen = Screen::Development;
        v.rebuild();
        let plain = screen_text(&v, 150, 30);
        assert!(!plain.contains("fleet ·"), "{plain}");
        assert!(
            plain.contains("dispatch") && plain.contains("take back"),
            "{plain}"
        );

        let mut running = Dispatch::queued("acme/hub", 2, None, 0);
        running.state = State::Running;
        running.owner = Some("ana@host".into());
        running.started_ns = Some(crate::project::now_ns());
        v.dispatches = vec![running];
        v.rebuild();
        // the card is first, under its column, and selected
        v.set_cursor_for_test(1);
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("fleet · WIP (1)"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("fleet · WIP: running for"), "{text}");
        assert!(text.contains("as ana@host"), "{text}");
    }

    #[test]
    fn a_card_says_what_its_sessions_cost_and_observability_adds_them_up() {
        use crate::tracing::project_report::{ItemTotals, Totals};
        let mut v = view();
        v.item = Some(crate::app::project::ItemScreen {
            number: 2,
            tab: 0,
            cursor: 0,
            scroll: 0,
            back: vec![],
        });
        assert!(!screen_text(&v, 150, 40).contains("Sessions:"));
        let own = Totals {
            sessions: 2,
            tokens: 12_400,
            cost_usd: 0.42,
            unpriced: false,
            wall_ns: 18 * 60_000_000_000,
            errors: 0,
        };
        v.totals.insert(2, ItemTotals { own, rolled: own });
        let text = screen_text(&v, 150, 40);
        assert!(text.contains("Sessions:  2 session(s)"), "{text}");
        assert!(text.contains("18m"), "{text}");

        // the screen has no rows: its pane is the numbers, in two windows
        v.item = None;
        v.screen = Screen::Observability;
        v.rebuild();
        assert!(v.rows.is_empty());
        assert!(screen_text(&v, 150, 30).contains("reading the trace store"));
        v.activity = [
            vec![
                "the last 30 days: 2 session(s) · 12.4k tok · $0.42 · 18m".into(),
                String::new(),
                "By step".into(),
                "  build                        2 session(s)".into(),
            ],
            vec!["ever: 9 session(s)".into()],
        ];
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("Observability"), "{text}");
        assert!(
            text.contains("the last 30 days: 2 session(s)") && text.contains("By step"),
            "{text}"
        );
        v.tab = 1;
        assert!(screen_text(&v, 150, 30).contains("ever: 9 session(s)"));
    }

    #[test]
    fn the_trace_tab_lists_the_runs_and_the_sessions_of_the_selected_one() {
        use crate::app::project::ItemTrace;
        use crate::tracing::project_report::tests::tag;
        use crate::tracing::project_report::{ItemRef, ProjectReport, StepLaunch, runs};
        let mut v = view();
        v.item = Some(crate::app::project::ItemScreen {
            number: 2,
            tab: TAB_TRACE,
            cursor: 0,
            scroll: 0,
            back: vec![],
        });
        // before the store is read, and when it has nothing for the item
        assert!(screen_text(&v, 150, 30).contains("reading the trace store"));
        let report = |launches: Vec<StepLaunch>| ProjectReport {
            hub: "acme/hub".into(),
            item: 2,
            items: vec![ItemRef {
                number: 2,
                item_type: Some("Story".into()),
                title: "ver pedidos".into(),
            }],
            tokens: Some(1500),
            cost_usd: None,
            unpriced: false,
            launches,
        };
        let trace = |r: ProjectReport| ItemTrace {
            number: 2,
            runs: runs(&r),
            report: Ok(r),
            read_ns: 2_000_000_000,
        };
        v.trace = Some(trace(report(Vec::new())));
        let text = screen_text(&v, 150, 30);
        assert!(
            text.contains("no step of this item has run a session yet"),
            "{text}"
        );

        let launch = |id: &str, at: i64, run: &str, label: &str| StepLaunch {
            launch_id: id.into(),
            project: tag(2, "build", run, label),
            provider: "claude".into(),
            session_key: None,
            started_ns: at,
            ended_ns: Some(at + 1_000_000_000),
            turns: Vec::new(),
            tools: 4,
            errors: 0,
            tokens: Some(500),
            cost_usd: None,
            unpriced: false,
            models: Vec::new(),
        };
        v.trace = Some(trace(report(vec![
            launch("L-1", 0, "run-a", "build"),
            launch("L-2", 10, "run-a", "build/review"),
            launch("L-3", 20, "run-b", "build"),
        ])));
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("3 session(s) in 2 run(s)"), "{text}");
        assert!(text.contains("#2 build"), "{text}");
        // the first run is selected: its two sessions are below the list
        assert!(text.contains("run run-a"), "{text}");
        assert!(text.contains("review · claude"), "{text}");
        assert!(text.contains("open run"), "{text}");
        v.item.as_mut().unwrap().cursor = 1;
        let text = screen_text(&v, 150, 30);
        assert!(
            text.contains("run run-b") && !text.contains("review · claude"),
            "{text}"
        );
        // a store that could not be read says so
        v.trace = Some(ItemTrace {
            number: 2,
            report: Err("locked".into()),
            runs: Vec::new(),
            read_ns: 0,
        });
        assert!(screen_text(&v, 150, 30).contains("the trace was not read: locked"));
    }

    #[test]
    fn milestones_show_each_features_documents() {
        let mut v = view();
        v.screen = Screen::Milestones;
        v.rebuild();
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("v1"));
        assert!(
            text.contains("1 of 4 issues closed · due 2026-12-01"),
            "{text}"
        );
        assert!(text.contains("Cadastro de Pedidos"));

        // a read tab: that it is being read, then what was read
        v.tab = crate::app::project::TAB_AUDIT;
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("Reading the specs"), "{text}");
        let number = v.selected_milestone().unwrap().number;
        v.milestone_reads.push(crate::app::project::MilestoneRead {
            milestone: number,
            tab: crate::app::project::TAB_AUDIT,
            lines: Ok(vec![
                "Milestone \"v1\" of acme/hub: 1 Feature(s) audited".into(),
            ]),
        });
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("1 Feature(s) audited"), "{text}");
        assert!(text.contains("[C] the set critique"), "{text}");
        v.milestone_reads[0].lines = Err("no checkout of the hub".into());
        let text = screen_text(&v, 150, 30);
        assert!(text.contains("not read: no checkout of the hub"), "{text}");
    }
}
