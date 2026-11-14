//! Drawing the Run screen (`crate::app::run_screen`): the run's tree
//! where the Agents list is and the selected row's tabs in the main pane,
//! as the Swarm screen does (spec 2026-11-13, section 2). A step is drawn
//! by `ui::step_row`, the function the flow editor's chain uses.

use super::step_row::{self, Columns, StepLine};
use crate::app::run_screen::{
    Focus, RunScreen, RunTab, StepTab, changes_lines, document_lines, elapsed, step_lines,
};
use crate::tracing::swarm_report::{Lane, lane_cells};
use crate::tracing::view::{Window, axis};
use crate::workflows::run_view::{Row, RowKind, State, tokens_short};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn title_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn pad(s: &str, to: usize) -> String {
    format!("{s}{}", " ".repeat(to.saturating_sub(s.width())))
}

fn cut(s: &str, max: usize) -> String {
    super::truncate_chars(s, max)
}

/// The tree's lines, one per visible row, no wider than `width`.
pub fn tree_lines(st: &RunScreen, width: usize) -> Vec<Line<'static>> {
    let rows = &st.data.tree.rows;
    let visible = st.visible();
    let line_of = |i: &usize| StepLine::of_row(&rows[*i]);
    let steps: Vec<StepLine> = visible
        .iter()
        .filter(|i| rows[**i].kind == RowKind::Step)
        .map(line_of)
        .collect();
    let sessions: Vec<StepLine> = visible
        .iter()
        .filter(|i| rows[**i].kind == RowKind::Session)
        .map(line_of)
        .collect();
    let step_cols = Columns::of(&steps);
    let session_cols = Columns::of(&sessions);
    visible
        .iter()
        .map(|i| {
            let row = &rows[*i];
            match row.kind {
                RowKind::Run => {
                    let right =
                        format!("{}/{} {}", row.done, row.planned, tokens_short(row.tokens));
                    let room = width.saturating_sub(right.width() + 3);
                    let name = cut(&row.label, room);
                    let gap = width.saturating_sub(2 + name.width() + right.width());
                    Line::from(vec![
                        Span::styled("◆ ", step_row::state_style(row.state)),
                        Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw(" ".repeat(gap)),
                        Span::styled(right, dim()),
                    ])
                }
                RowKind::Step => {
                    let mark = if !st.has_children(&row.step_id) {
                        "  "
                    } else if st.folded(row) {
                        "▸ "
                    } else {
                        "▾ "
                    };
                    let mut spans = vec![Span::styled(mark, dim())];
                    spans.extend(step_row::spans(
                        &StepLine::of_row(row),
                        step_cols,
                        width.saturating_sub(2),
                    ));
                    Line::from(spans)
                }
                RowKind::Session => {
                    let mut spans = vec![Span::raw("    ")];
                    spans.extend(step_row::spans(
                        &StepLine::of_row(row),
                        session_cols,
                        width.saturating_sub(4),
                    ));
                    Line::from(spans)
                }
            }
        })
        .collect()
}

pub fn draw_tree(f: &mut Frame, area: Rect, st: &RunScreen) {
    let focused = st.focus == Focus::Tree;
    let run = st.data.tree.run();
    let room = usize::from(area.width.saturating_sub(4));
    let status = match elapsed(run, st.data.now_ns) {
        Some(d) => format!("{} {d}", st.data.tree.status.label()),
        None => st.data.tree.status.label().to_string(),
    };
    let title = cut(&format!("{} · {status}", run.label), room);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(format!(" {title} "), title_style(focused)));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let visible = usize::from(area.height.saturating_sub(2));
    let lines = tree_lines(st, inner_w.saturating_sub(1));
    let start = super::sidebar_window(st.cursor, lines.len(), visible.max(1));
    let rows = lines.len();
    let mut items: Vec<ListItem> = lines
        .into_iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(n, line)| {
            let on = n == st.cursor;
            if on && focused {
                // one plain bar: the row's own colours would break it up
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                ListItem::new(Line::raw(format!(
                    ">{}",
                    pad(&text, inner_w.saturating_sub(1))
                )))
                .style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                let mut spans = vec![Span::styled(
                    if on { ">" } else { " " },
                    Style::default().fg(Color::Cyan),
                )];
                spans.extend(line.spans);
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    // the screen's own keys, under the rows when there is room
    if rows + 1 < visible {
        items.push(ListItem::new(Line::raw("")));
        items.push(ListItem::new(Line::styled(
            cut(
                if st.data.live {
                    "x stop · e edit · Esc back"
                } else {
                    "r resume · e edit · Esc back"
                },
                inner_w,
            ),
            dim(),
        )));
    }
    f.render_widget(List::new(items).block(block), area);
}

fn tab_bar(st: &RunScreen) -> Line<'static> {
    let (names, open) = st.tabs();
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (n, name) in names.iter().enumerate() {
        let label = format!(" {} {name} ", n + 1);
        spans.push(if n == open {
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
    Line::from(spans)
}

/// `claude · skeptic · gpt-5`, the harness dimmed when it is the flow's.
fn who_spans(row: &Row, width: usize) -> Vec<Span<'static>> {
    let who = &row.who;
    let mut spans = vec![Span::styled(
        who.harness.clone(),
        if who.inherited {
            dim()
        } else {
            Style::default()
        },
    )];
    let mut used = who.harness.width();
    if let Some(a) = &who.agent {
        spans.push(Span::styled("·", dim()));
        spans.push(Span::styled(a.clone(), Style::default().fg(Color::Magenta)));
        used += 1 + a.width();
    }
    if let Some(m) = &who.model
        && used + 1 + m.width() <= width
    {
        spans.push(Span::styled("·", dim()));
        spans.push(Span::styled(m.clone(), dim()));
        used += 1 + m.width();
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    spans
}

fn who_width(row: &Row) -> usize {
    let who = &row.who;
    who.harness.width()
        + who.agent.as_ref().map_or(0, |a| 1 + a.width())
        + who.model.as_ref().map_or(0, |m| 1 + m.width())
}

/// What the board says of a step's state: how long it took, or how far
/// it is.
fn state_text(row: &Row, now_ns: i64) -> String {
    let more = match row.counts() {
        Some((done, planned)) if !row.state.ended() => Some(format!("{done}/{planned}")),
        _ => elapsed(row, now_ns),
    };
    match more {
        Some(m) => format!("{} {m}", row.state.glyph()),
        None => row.state.glyph().to_string(),
    }
}

/// The Board tab: one line per step, then the run's totals.
pub fn board_lines(st: &RunScreen, width: usize) -> Vec<Line<'static>> {
    let data = &st.data;
    let steps: Vec<&Row> = data.tree.steps().collect();
    let id_w = steps
        .iter()
        .map(|r| r.label.width())
        .max()
        .unwrap_or(0)
        .clamp(4, 18);
    let shape_w = steps
        .iter()
        .filter_map(|r| r.shape.as_ref())
        .map(|s| s.words.width())
        .max()
        .unwrap_or(0)
        .clamp(4, 20);
    let states: Vec<String> = steps.iter().map(|r| state_text(r, data.now_ns)).collect();
    let state_w = states.iter().map(|s| s.width()).max().unwrap_or(0).max(5);
    // number, gaps and the token column are fixed; who takes what is left
    let fixed = 2 + 3 + id_w + 2 + shape_w + 2 + 2 + state_w + 2 + 6;
    let who_w = steps
        .iter()
        .map(|r| who_width(r))
        .max()
        .unwrap_or(0)
        .max(3)
        .min(width.saturating_sub(fixed).max(8));
    let mut lines = vec![Line::styled(
        format!(
            "  {:>2} {}  {}  {}  {}  {:>6}",
            "#",
            pad("step", id_w),
            pad("runs", shape_w),
            pad("who", who_w),
            pad("state", state_w),
            "tok"
        ),
        dim(),
    )];
    for (row, state) in steps.iter().zip(&states) {
        let skipped = row.state == State::Skipped;
        let plain = if skipped { dim() } else { Style::default() };
        let mut spans = vec![
            Span::styled(format!("  {:>2} ", row.number.unwrap_or(0)), dim()),
            Span::styled(
                pad(&cut(&row.label, id_w), id_w),
                if skipped {
                    dim()
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            ),
            Span::raw("  "),
            Span::styled(
                pad(
                    &cut(
                        row.shape.as_ref().map(|s| s.words.as_str()).unwrap_or(""),
                        shape_w,
                    ),
                    shape_w,
                ),
                plain,
            ),
            Span::raw("  "),
        ];
        spans.extend(who_spans(row, who_w));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            pad(state, state_w),
            step_row::state_style(row.state),
        ));
        spans.push(Span::styled(
            format!(
                "  {:>6}",
                if row.tokens > 0 {
                    tokens_short(row.tokens)
                } else {
                    String::new()
                }
            ),
            dim(),
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(format!("  {}", data.tree.totals.line())));
    if let Some(e) = &data.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  ✗ {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "  {} · profile {}{}",
            data.workspace,
            data.profile,
            match &data.resumed_from {
                Some(r) => format!(" · resumed from {}", cut(r, 8)),
                None => String::new(),
            }
        ),
        dim(),
    ));
    lines
}

/// The Timeline tab: one lane per step and session over the run's time.
pub fn timeline_lines(st: &RunScreen, width: usize) -> Vec<Line<'static>> {
    let data = &st.data;
    let rows = &data.tree.rows;
    let run = data.tree.run();
    let Some(start) = run
        .started_ns
        .or_else(|| rows.iter().filter_map(|r| r.started_ns).min())
    else {
        return vec![Line::styled("  Nothing has started yet.", dim())];
    };
    let end = run.ended_ns.unwrap_or(data.now_ns).max(start + 1);
    let w = Window {
        start_ns: start,
        end_ns: end,
    };
    let label_w = (width / 3).clamp(12, 28).min(width.saturating_sub(10));
    let track = width.saturating_sub(label_w + 4);
    let clock = |ns: i64| {
        crate::tracing::cli::fmt_time(ns)
            .get(11..16)
            .unwrap_or_default()
            .to_string()
    };
    let mut lines = vec![Line::styled(
        format!(
            "  {}  {}",
            pad(&format!("{} → {}", clock(start), clock(end)), label_w),
            axis(&w, track)
        ),
        dim(),
    )];
    for row in rows.iter().filter(|r| r.kind != RowKind::Run) {
        let label = match row.kind {
            RowKind::Step => format!("{} {}", row.number.unwrap_or(0), row.label),
            _ => format!("  {}", row.label),
        };
        let style = step_row::state_style(row.state);
        let mut spans = vec![Span::styled(
            format!("  {}  ", pad(&cut(&label, label_w), label_w)),
            if row.kind == RowKind::Step {
                style
            } else {
                dim()
            },
        )];
        match row.started_ns {
            Some(s) => {
                let end_ns = match row.ended_ns {
                    Some(e) => Some(e),
                    None if row.state.ended() => Some(s),
                    None => None,
                };
                let lane = Lane {
                    label,
                    depth: usize::from(row.depth.saturating_sub(1)),
                    launch: None,
                    task_id: None,
                    start_ns: s,
                    end_ns,
                    working: vec![(s, end_ns)],
                    asking: Vec::new(),
                    marks: Vec::new(),
                    state: row.state.label().to_string(),
                };
                let cells: String = lane_cells(&lane, &w, track, data.now_ns)
                    .into_iter()
                    .collect();
                spans.push(Span::styled(cells, style));
            }
            None => spans.push(Span::styled(row.state.label(), dim())),
        }
        lines.push(Line::from(spans));
    }
    lines
}

pub fn draw_pane(f: &mut Frame, area: Rect, st: &RunScreen) {
    let focused = st.focus == Focus::Pane;
    let room = usize::from(area.width.saturating_sub(4));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            format!(" {} ", cut(&st.pane_title(), room)),
            title_style(focused),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    f.render_widget(Paragraph::new(tab_bar(st)), tabs);
    let width = usize::from(body.width);
    let row = st.selected();
    // a table and a chart keep their lines; the rest wraps
    let (lines, wrap) = if row.kind == RowKind::Run {
        match st.run_tab {
            RunTab::Board => (board_lines(st, width), false),
            RunTab::Timeline => (timeline_lines(st, width), false),
            RunTab::Result => (st.data.result.clone(), true),
            RunTab::Document => (document_lines(&st.data), true),
        }
    } else {
        match st.step_tab {
            StepTab::Step => (step_lines(&st.data, row), true),
            StepTab::Changes => (changes_lines(&st.data, row), true),
        }
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
