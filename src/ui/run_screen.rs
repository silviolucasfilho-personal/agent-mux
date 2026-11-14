//! Drawing the Run screen (`crate::app::run_screen`): the run's tree
//! where the Agents list is and the selected row's tabs in the main pane,
//! as the Swarm screen does (spec 2026-11-13, section 2). A step is drawn
//! by `ui::step_row`, the function the flow editor's chain uses.

use super::step_row::{self, Columns, StepLine};
use crate::app::run_screen::Picker;
use crate::app::run_screen::{
    Focus, RunScreen, RunTab, StepTab, changes_lines, document_lines, elapsed, step_lines,
};
use crate::tracing::swarm_report::{Lane, lane_cells};
use crate::tracing::view::{Window, axis};
use crate::workflows::live::{LiveKind, LiveLine};
use crate::workflows::run_view::{Row, RowKind, State, tokens_short};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
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
        if rows + 2 < visible {
            items.push(ListItem::new(Line::styled("o other runs", dim())));
        }
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

/// One decoded line of a session's output: a tool call, its outcome,
/// the agent's text.
fn live_line(l: &LiveLine) -> Line<'static> {
    let style = match l.kind {
        LiveKind::Text => Style::default(),
        LiveKind::Tool => Style::default().fg(Color::Cyan),
        LiveKind::Outcome => dim(),
        LiveKind::Error => Style::default().fg(Color::Red),
    };
    Line::styled(format!("  {}", l.text), style)
}

/// The Live tab of a step or session row: what its session printed, as
/// lines; or why there is nothing to show.
pub fn live_lines(st: &RunScreen, row: &Row) -> Vec<Line<'static>> {
    if row.session.is_none() {
        return vec![Line::styled(
            "  This step has several sessions: → opens it, and each has its own Live tab. The run's Live tab shows those running now.",
            dim(),
        )];
    }
    match st.tail().filter(|t| !t.lines.is_empty()) {
        Some(t) => t.lines.iter().map(live_line).collect(),
        None => vec![Line::styled(
            match row.state {
                State::Waiting | State::Skipped => "  It has not started.",
                State::Running | State::PartlyDone => "  Nothing printed yet.",
                State::Replayed => {
                    "  Answered from the journal of the run this one resumed: it printed nothing here."
                }
                _ => "  No output was kept for this session.",
            },
            dim(),
        )],
    }
}

/// The Trace tab: the session's turns and tools from the trace store.
pub fn trace_lines(st: &RunScreen, row: &Row) -> Vec<Line<'static>> {
    if row.session.is_none() {
        return vec![Line::styled(
            "  This step has several sessions: → opens it, and each has its own trace.",
            dim(),
        )];
    }
    match (&row.launch_id, &st.trace) {
        (Some(launch), Some((of, lines))) if launch == of => {
            lines.iter().map(|l| Line::raw(l.clone())).collect()
        }
        (Some(_), _) => vec![Line::styled("  Reading the trace…", dim())],
        (None, _) => vec![Line::styled(
            "  No traced session: it has not started, or tracing is off.",
            dim(),
        )],
    }
}

/// The run row's Live tab: every session running now at once, each a
/// tail of its output in a tile (1, 1×2, 2×2, 2×3; the first six).
fn draw_mosaic(f: &mut Frame, area: Rect, st: &RunScreen) {
    let running = &st.data.running;
    if running.is_empty() {
        let text = if st.data.live {
            "  No session is running right now."
        } else {
            "  The run has ended. A session's own Live tab keeps what it printed."
        };
        f.render_widget(Paragraph::new(Line::styled(text, dim())), area);
        return;
    }
    let shown = running.len().min(6);
    let cols = match shown {
        1 => 1,
        2..=4 => 2,
        _ => 3,
    };
    let rows = shown.div_ceil(cols);
    let more = running.len() - shown;
    let (grid, foot) = if more > 0 {
        let [g, l] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
        (g, Some(l))
    } else {
        (area, None)
    };
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(grid);
    for (n, label) in running.iter().take(shown).enumerate() {
        let col_areas = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols])
            .split(row_areas[n / cols]);
        let tile = col_areas[n % cols];
        let who = st
            .data
            .tree
            .rows
            .iter()
            .find(|r| r.session.as_deref() == Some(label.as_str()))
            .map(|r| r.who.short())
            .unwrap_or_default();
        let room = usize::from(tile.width.saturating_sub(4));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim())
            .title(Span::styled(
                format!(" {} ", cut(&format!("▶ {label} · {who}"), room)),
                Style::default().fg(Color::Yellow),
            ));
        let inner = block.inner(tile);
        f.render_widget(block, tile);
        let height = usize::from(inner.height);
        let width = usize::from(inner.width);
        let lines: Vec<Line> = match st.tails.get(label).filter(|t| !t.lines.is_empty()) {
            Some(t) => t.lines[t.lines.len().saturating_sub(height)..]
                .iter()
                .map(|l| {
                    let mut line = live_line(l);
                    for s in &mut line.spans {
                        s.content = cut(&s.content, width).into();
                    }
                    line
                })
                .collect(),
            None => vec![Line::styled("  Nothing printed yet.", dim())],
        };
        f.render_widget(Paragraph::new(lines), inner);
    }
    if let Some(l) = foot {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("  and {more} more running: each has its own Live tab"),
                dim(),
            )),
            l,
        );
    }
}

/// The run picker (`o`), over the pane.
fn draw_picker(f: &mut Frame, area: Rect, p: &Picker) {
    let height = (p.items.len() as u16 + 2).clamp(3, 18).min(area.height);
    let width = 64.min(area.width);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(true))
        .title(Span::styled(
            " Runs · ↑↓ · ↵ open · Esc ",
            title_style(true),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let visible = usize::from(inner.height).max(1);
    let w = usize::from(inner.width);
    let start = super::sidebar_window(p.cursor, p.items.len(), visible);
    let mut lines: Vec<Line> = p
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(n, item)| {
            let glyph = if item.live { "▶" } else { " " };
            let room = w.saturating_sub(item.word.width() + 5);
            let name = cut(&item.name, room);
            let text = format!(" {glyph} {}{} ", pad(&name, room), item.word);
            if n == p.cursor {
                Line::styled(text, Style::default().add_modifier(Modifier::REVERSED))
            } else if item.live {
                Line::styled(text, Style::default().fg(Color::Yellow))
            } else {
                Line::raw(text)
            }
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(" no runs yet", dim()));
    }
    f.render_widget(Paragraph::new(lines), inner);
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
    if row.kind == RowKind::Run && st.run_tab == RunTab::Live {
        st.max_scroll.set(0);
        draw_mosaic(f, body, st);
        if let Some(p) = &st.picker {
            draw_picker(f, area, p);
        }
        return;
    }
    // a table and a chart keep their lines; the rest wraps
    let (lines, wrap) = if row.kind == RowKind::Run {
        match st.run_tab {
            RunTab::Board => (board_lines(st, width), false),
            RunTab::Live => (Vec::new(), false),
            RunTab::Timeline => (timeline_lines(st, width), false),
            RunTab::Result => (st.data.result.clone(), true),
            RunTab::Document => (document_lines(&st.data), true),
        }
    } else {
        match st.step_tab {
            StepTab::Step => (step_lines(&st.data, row), true),
            StepTab::Live => (live_lines(st, row), true),
            StepTab::Trace => (trace_lines(st, row), false),
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
    // a tail stays on its newest line until the user scrolls up
    let at = if st.on_live_tail() && st.follow {
        max
    } else {
        st.scroll.min(max)
    };
    let off = u16::try_from(at).unwrap_or(u16::MAX);
    f.render_widget(p.scroll((off, 0)), body);
    if let Some(p) = &st.picker {
        draw_picker(f, area, p);
    }
}
