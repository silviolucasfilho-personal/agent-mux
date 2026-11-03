//! Drawing the Swarm screen (`crate::app::swarm_board`) in the Agents
//! layout: the swarm tree where the Agents list is (`draw_tree`), and the
//! selected row in the main pane (`draw_pane`): a live session's terminal
//! through the Agents screen's own pane (`ui::draw_session_pane`), else the
//! swarm's or the task's card. The global status bar is the only footer.
//! Spec: `docs/superpowers/specs/2026-11-02-swarm-agents-layout-design.md` §2.

use super::flow::{dim, head};
use crate::app::App;
use crate::app::swarm_board::{
    Confirm, Focus, LaunchField, LinePurpose, Modal, NeedKind, PaneView, Recipient, Row,
    SwarmBoard, Tab, TaskField,
};
use crate::app::text_area::TextArea;
use crate::bus::swarm::engine::{self, Shown};
use crate::bus::swarm::record::{BoardTask, SwarmStatus};
use crate::bus::{BusMessage, TaskStatus};
use crate::tracing::cli::{fmt_cost, fmt_tokens};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::time::Instant;

fn state_label(s: SwarmStatus) -> &'static str {
    match s {
        SwarmStatus::Planning => "planning",
        SwarmStatus::Review => "review",
        SwarmStatus::Running => "running",
        SwarmStatus::Paused => "paused",
        SwarmStatus::Integrating => "integrating",
        SwarmStatus::Finished => "finished",
    }
}

fn shown_style(s: Shown) -> Style {
    match s {
        Shown::Working => Style::default().fg(Color::Green),
        Shown::Asking | Shown::Prompt | Shown::Stalled => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Shown::Failed | Shown::Lost => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Shown::Done => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::DIM),
        Shown::Review | Shown::Proposed => Style::default().fg(Color::Cyan),
        Shown::Cancelled | Shown::Draft | Shown::Blocked => dim(),
        Shown::Ready => Style::default().fg(Color::White),
    }
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

/// Tokens and cost of a swarm's live workers and coordinator: `—` when none
/// of it is priced, `≥$x` when part of it is (agy is not priced).
pub fn spend(v: &SwarmBoard, swarm: usize) -> (i64, String) {
    let Some(d) = v.swarms.get(swarm) else {
        return (0, "—".into());
    };
    let launches = d
        .board
        .iter()
        .filter_map(|t| t.task.assigned_to.as_deref())
        .chain(d.run.planner_launch.as_deref());
    let (mut tokens, mut cost, mut priced, mut unpriced) = (0i64, 0f64, 0usize, 0usize);
    for l in launches {
        if let Some(w) = v.live.get(l) {
            tokens += w.tokens;
            if w.priced {
                cost += w.cost_usd;
                priced += 1;
            } else {
                unpriced += 1;
            }
        }
    }
    let label = match (priced, unpriced) {
        (0, _) => "—".to_string(),
        (_, 0) => fmt_cost(Some(cost)),
        _ => format!("≥{}", fmt_cost(Some(cost))),
    };
    (tokens, label)
}

/// The tree, where the Agents list is: each swarm, its coordinator and its
/// tasks, with `!` and `⚑n` where the operator is needed.
pub fn draw_tree(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let focused = v.focus == Focus::Board && !v.pane_focused;
    let live = v.swarms.iter().filter(|d| d.run.ended_ns.is_none()).count();
    let needs = v.needs_total();
    // like the Agents list's: the name and where the keyboard is, then what
    // fits of the counts (the rows carry the badges)
    let room = usize::from(area.width.saturating_sub(4));
    let mut title = if focused {
        "Swarms • Keyboard focus".to_string()
    } else {
        format!("Swarms · {live} live")
    };
    let badge = format!(" ⚑{needs}");
    if needs > 0 && title.chars().count() + badge.chars().count() <= room {
        title.push_str(&badge);
    }
    let title = format!(" {title} ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            title,
            if needs > 0 {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let visible = usize::from(area.height.saturating_sub(2));
    let start = super::sidebar_window(v.cursor, v.rows.len(), visible.saturating_sub(1).max(1));
    let mut items: Vec<ListItem> = Vec::new();
    for (n, row) in v.rows.iter().enumerate().skip(start).take(visible) {
        let is_cursor = n == v.cursor;
        let marker = if is_cursor { ">" } else { " " };
        let (left, lstyle, right, rstyle): (String, Style, String, Style) = match *row {
            Row::Swarm(i) => {
                let d = &v.swarms[i];
                let arrow = if v.is_folded(i) { "▸" } else { "▾" };
                let done = d
                    .board
                    .iter()
                    .filter(|t| t.task.status == TaskStatus::Done)
                    .count();
                let n_need = d.needs.len();
                let (right, rstyle) = if d.run.ended_ns.is_some() {
                    ("✓".to_string(), dim())
                } else if n_need > 0 {
                    (
                        format!("⚑{n_need}"),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    (format!("{done}/{}", d.board.len()), dim())
                };
                (
                    format!("{arrow} ◆ {}", d.run.goal),
                    if d.run.ended_ns.is_some() {
                        dim()
                    } else {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    },
                    right,
                    rstyle,
                )
            }
            Row::Coordinator(i) => {
                let d = &v.swarms[i];
                let state = d
                    .run
                    .planner_launch
                    .as_deref()
                    .and_then(|l| v.live.get(l))
                    .map(|w| match w.worker.state {
                        engine::WorkerState::Working => ("●", Style::default().fg(Color::Green)),
                        engine::WorkerState::Prompt => ("⚑", Style::default().fg(Color::Yellow)),
                        engine::WorkerState::Idle => ("○", dim()),
                        engine::WorkerState::Exited => ("·", dim()),
                    })
                    .unwrap_or(("·", dim()));
                (
                    format!("    ★ coordinator·{}", d.run.planner_harness),
                    Style::default().fg(Color::Gray),
                    state.0.to_string(),
                    state.1,
                )
            }
            Row::Task(i, j) => {
                let d = &v.swarms[i];
                let t = &d.board[j];
                let input = engine::Input {
                    swarm: &d.run,
                    board: &d.board,
                    workers: &v.workers,
                    asking: &d.asking,
                    now_ns: v.now_ns,
                };
                let st = engine::shown(t, &input);
                let needs = v.row_needs(*row);
                let plan_error = i
                    == v.rows
                        .get(v.cursor)
                        .map(|r| match r {
                            Row::Swarm(k) | Row::Coordinator(k) | Row::Task(k, _) => *k,
                        })
                        .unwrap_or(usize::MAX)
                    && v.problems.for_task(&t.ext.plan_key).any(|(e, _)| e);
                let harness = v
                    .worker_of(t)
                    .map(|w| w.provider.clone())
                    .or_else(|| t.task.target_harness.clone())
                    .unwrap_or_default();
                let (right, rstyle) = if plan_error {
                    (
                        format!("✗ {harness}"),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )
                } else if needs > 0 {
                    (
                        "!".to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    (harness, dim())
                };
                (
                    format!("    {} {} {}", st.glyph(), j + 1, t.ext.plan_key),
                    shown_style(st),
                    right,
                    rstyle,
                )
            }
        };
        let room = inner_w.saturating_sub(1 + right.chars().count() + 1);
        let left = truncate(&left, room);
        let pad = room.saturating_sub(left.chars().count());
        let item = ListItem::new(Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(left, lstyle),
            Span::raw(" ".repeat(pad + 1)),
            Span::styled(right, rstyle),
        ]));
        items.push(if is_cursor && focused {
            item.style(Style::default().add_modifier(Modifier::REVERSED))
        } else if is_cursor {
            item.style(Style::default().fg(Color::Cyan))
        } else {
            item
        });
    }
    if items.len() < visible {
        items.push(ListItem::new(Line::styled(
            if v.rows.is_empty() {
                " no swarms: n starts one"
            } else {
                " + new swarm  n"
            },
            dim(),
        )));
    }
    f.render_widget(List::new(items).block(block), area);
}

/// The main pane: the selected row's terminal or card, the command line,
/// and the forms and confirmations over them.
pub fn draw_pane(f: &mut Frame, area: Rect, v: &SwarmBoard, app: &App, now: Instant) {
    let (body, line) = if v.command.is_some() {
        let [b, l] = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(area);
        (b, Some(l))
    } else {
        (area, None)
    };
    if v.activity {
        draw_activity(f, body, v);
    } else {
        match v.pane() {
            PaneView::Session(idx) => super::draw_session_pane(f, body, app, idx, now),
            PaneView::SwarmCard => draw_swarm_card(f, body, v),
            PaneView::TaskCard => draw_task_card(f, body, v),
            PaneView::Launch => draw_launch_card(f, body, v),
        }
    }
    if let Some(l) = line {
        f.render_widget(Clear, l);
        draw_command(f, l, v);
    }
    if let Some(m) = &v.modal {
        draw_modal(f, area, m, v);
    }
    if let Some(c) = &v.confirm {
        draw_confirm(f, area, c);
    }
}

fn card_block(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            title,
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ))
}

/// The swarm's card: where it stands, what needs you, the plan, the check
/// and its verbs.
fn draw_swarm_card(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let Some(s) = &v.swarm else {
        draw_launch_card(f, area, v);
        return;
    };
    let si = match v.rows.get(v.cursor) {
        Some(Row::Swarm(i)) | Some(Row::Coordinator(i)) | Some(Row::Task(i, _)) => *i,
        None => 0,
    };
    let block = card_block(format!(" ◆ {} ", s.goal), v.focus == Focus::Panel);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let section = |t: &str| {
        Line::styled(
            format!("── {t} ──"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let total = v.board.len();
    let done = v
        .board
        .iter()
        .filter(|t| t.task.status == TaskStatus::Done)
        .count();
    let (tokens, cost) = spend(v, si);
    let elapsed = crate::bus::swarm::short_age(v.now_ns - s.started_ns);
    let state = if s.ended_ns.is_some() {
        "closed".to_string()
    } else {
        state_label(s.status).to_string()
    };
    let mut out = vec![
        Line::from(vec![
            Span::styled(state, head()),
            Span::styled(
                format!(
                    " · {done}/{total} done · {}/{} workers · {elapsed} · {} tok · {cost}",
                    v.busy_workers(),
                    s.max_workers,
                    fmt_tokens(Some(tokens)),
                ),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::styled(
            format!(
                "{} · {} · coordinator {}",
                s.id,
                crate::app::workspaces_view::tilde_path(std::path::Path::new(&s.workspace)),
                s.planner_harness
            ),
            dim(),
        ),
        Line::raw(""),
    ];
    if s.status == SwarmStatus::Planning {
        let since = crate::bus::swarm::short_age(v.now_ns - s.updated_ns);
        out.push(Line::styled(
            format!(
                "◌ the coordinator ({}) has been planning for {since}. Its terminal is the ★ row: watch it, or Enter there to talk to it.",
                s.planner_harness
            ),
            Style::default().fg(Color::Yellow),
        ));
        out.push(Line::raw(""));
    }
    out.push(section(&format!("Needs you ({})", v.needs.len())));
    if v.needs.is_empty() {
        out.push(Line::styled(
            "✓ Nothing needs you.",
            Style::default().fg(Color::Green),
        ));
    }
    for n in &v.needs {
        let kind_style = match n.kind {
            NeedKind::Failed | NeedKind::Lost | NeedKind::PlannerFailed | NeedKind::CheckFailed => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
            NeedKind::Review | NeedKind::Proposed => Style::default().fg(Color::Cyan),
            NeedKind::Finished => Style::default().fg(Color::Green),
            _ => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        };
        out.push(Line::from(vec![
            Span::styled(format!("{:<10}", n.kind.label()), kind_style),
            Span::raw(n.text.clone()),
        ]));
    }
    if !v.needs.is_empty() {
        out.push(Line::styled(
            "! goes to the next one; its row takes the verbs.",
            dim(),
        ));
    }
    out.push(Line::raw(""));
    out.push(section("Plan"));
    if let Some(sum) = s.plan_summary.as_deref().filter(|x| !x.is_empty()) {
        out.push(Line::raw(sum.to_string()));
    }
    if let Some(e) = &s.plan_error {
        out.push(Line::styled(
            format!("The planner gave no plan: {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    for (k, m) in v
        .problems
        .errors
        .iter()
        .chain(v.problems.warnings.iter())
        .filter(|(k, _)| k.is_empty())
    {
        let _ = k;
        out.push(Line::styled(m.clone(), Style::default().fg(Color::Yellow)));
    }
    out.push(Line::styled(
        format!(
            "{total} task(s){}",
            if v.problems.errors.is_empty() {
                String::new()
            } else {
                format!(
                    ", {} error(s) to fix before approving",
                    v.problems.errors.len()
                )
            }
        ),
        dim(),
    ));
    if let Some(cmd) = &s.check_cmd {
        out.push(Line::raw(""));
        out.push(section("Check when done"));
        match s
            .check_result
            .as_deref()
            .and_then(crate::bus::swarm::check::CheckResult::parse)
        {
            Some(r) => {
                out.push(Line::styled(
                    r.summary(cmd),
                    if r.running {
                        Style::default().fg(Color::Yellow)
                    } else if r.passed {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Red)
                    },
                ));
                if !r.running && !r.passed {
                    out.extend(
                        r.tail
                            .lines()
                            .map(|l| Line::styled(format!("  {l}"), dim())),
                    );
                }
            }
            None => out.push(Line::styled(
                format!("{cmd} (runs once everything is settled)"),
                dim(),
            )),
        }
    }
    out.push(Line::raw(""));
    out.push(Line::styled(
        match (s.ended_ns.is_some(), s.status) {
            (true, _) => "This swarm is closed. n starts a new one.",
            (_, SwarmStatus::Review) => {
                "y approve · p re-plan · n add a task · ↓ a task: e edit, d delete"
            }
            (_, SwarmStatus::Running) => "space pause · > message · ! next need · n add a task",
            (_, SwarmStatus::Paused) => "space resume · > message · ! next need",
            (_, SwarmStatus::Finished) => "y close the swarm",
            (_, SwarmStatus::Integrating) => "↓ a branch: y merge, x set aside",
            (_, SwarmStatus::Planning) => "p sends it notes · on ★: x stops it, r starts a new one",
        },
        dim(),
    ));
    let lines: Vec<Line<'static>> = out.into_iter().skip(v.panel_scroll).collect();
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        indented(inner),
    );
}

/// A task's card: its tabs (Task, Live, Thread, Changes) and the chosen one.
fn draw_task_card(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let Some(t) = v.selected_task() else {
        draw_swarm_card(f, area, v);
        return;
    };
    let st = v.shown(t);
    let focused = v.focus == Focus::Panel;
    let block = card_block(
        format!(" {} · {} · {} ", t.ext.plan_key, st.as_str(), t.task.title),
        focused,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let current = v.tab();
    let thread_len = v.thread().len();
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (n, tab) in Tab::ALL.iter().enumerate() {
        let label = match tab {
            Tab::Thread if thread_len > 0 => format!("{} Thread ({thread_len})", n + 1),
            _ => format!("{} {}", n + 1, tab.label()),
        };
        spans.push(if *tab == current {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), dim())
        });
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);
    match current {
        Tab::Task => {
            let lines: Vec<Line<'static>> =
                task_card(v, t).into_iter().skip(v.panel_scroll).collect();
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }),
                indented(content),
            );
        }
        Tab::Live => f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled(
                    match st {
                        Shown::Draft | Shown::Ready | Shown::Blocked | Shown::Proposed => {
                            "  No worker yet: agent-mux starts one when the task is ready."
                        }
                        Shown::Lost => "  Its session exited mid-task: r retries it.",
                        _ => "  No terminal here: its worker is gone or runs in another window.",
                    },
                    dim(),
                ),
            ]),
            content,
        ),
        Tab::Thread => {
            let msgs = v.thread();
            if msgs.is_empty() {
                f.render_widget(
                    Paragraph::new(vec![
                        Line::raw(""),
                        Line::styled("  No messages yet. m writes to its worker.", dim()),
                    ]),
                    content,
                );
            } else {
                draw_messages(f, content, v, &msgs, focused);
            }
        }
        Tab::Changes => {
            let lines: Vec<Line<'static>> = v
                .diff_lines
                .iter()
                .skip(v.panel_scroll)
                .take(content.height as usize)
                .map(|l| {
                    let style = if l.starts_with('+') && !l.starts_with("+++") {
                        Style::default().fg(Color::Green)
                    } else if l.starts_with('-') && !l.starts_with("---") {
                        Style::default().fg(Color::Red)
                    } else if l.starts_with("@@") || l.starts_with("diff ") {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::styled(l.clone(), style)
                })
                .collect();
            f.render_widget(Paragraph::new(lines), content);
        }
    }
}

/// The Task tab: the card, its plan checks, and what it produced.
fn task_card(v: &SwarmBoard, t: &BoardTask) -> Vec<Line<'static>> {
    let section = |s: &str| {
        Line::styled(
            format!("── {s} ──"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let st = v.shown(t);
    let mut out = vec![
        Line::from(vec![
            Span::styled(format!("{} {}", st.glyph(), st.as_str()), shown_style(st)),
            Span::styled(
                format!(
                    "   {} · {} · {}",
                    t.task.id,
                    t.task.target_harness.as_deref().unwrap_or("?"),
                    t.task.isolation.as_str()
                ),
                dim(),
            ),
        ]),
        Line::raw(""),
    ];
    let problems: Vec<(bool, String)> = v
        .problems
        .for_task(&t.ext.plan_key)
        .map(|(e, m)| (e, m.to_string()))
        .collect();
    for (is_err, m) in problems {
        out.push(Line::styled(
            format!("{} {m}", if is_err { "✗ error:" } else { "! warning:" }),
            if is_err {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Yellow)
            },
        ));
    }
    if st == Shown::Blocked {
        out.push(Line::styled(
            format!(
                "waits for: {}",
                engine::unmet_dependencies(t, &v.board).join(", ")
            ),
            Style::default().fg(Color::Yellow),
        ));
    }
    out.push(section("Instructions"));
    out.extend(
        t.task
            .instructions
            .lines()
            .map(|l| Line::raw(l.to_string())),
    );
    if let Some(a) = &t.ext.acceptance {
        out.push(Line::raw(""));
        out.push(section("Done when"));
        out.push(Line::raw(a.clone()));
    }
    if !t.task.files.is_empty() {
        out.push(Line::raw(""));
        out.push(section("Files"));
        out.extend(t.task.files.iter().map(|f| Line::raw(format!("  {f}"))));
    }
    if !t.ext.depends_on.is_empty() {
        out.push(Line::raw(""));
        out.push(section("Depends on"));
        out.push(Line::raw(format!("  {}", t.ext.depends_on.join(", "))));
    }
    if let Some(r) = &t.task.result {
        out.push(Line::raw(""));
        out.push(section("Result"));
        let text = match r {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.extend(text.lines().map(|l| Line::raw(l.to_string())));
    }
    if let Some(e) = &t.task.error {
        out.push(Line::raw(""));
        out.push(section("Error"));
        out.extend(
            e.lines()
                .map(|l| Line::styled(l.to_string(), Style::default().fg(Color::Red))),
        );
    }
    if let Some(b) = &t.task.branch {
        out.push(Line::raw(""));
        out.push(section("Branch"));
        out.push(Line::raw(format!(
            "  {b}{}",
            t.review
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        )));
    }
    if t.ext.attempts > 1 {
        out.push(Line::styled(format!("attempt {}", t.ext.attempts), dim()));
    }
    out.push(Line::raw(""));
    out.push(Line::styled(
        match st {
            Shown::Draft => "e edits · d deletes · y approves the plan",
            Shown::Review => "y merges the branch · x sets it aside",
            Shown::Failed | Shown::Lost | Shown::Cancelled => "r retries it",
            Shown::Proposed => "y accepts it · x drops it",
            _ => "",
        },
        dim(),
    ));
    out
}

fn draw_activity(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let focused = v.focus == Focus::Panel;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            " Activity · every message of the swarm ",
            head(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let msgs = v.panel_messages();
    if msgs.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("  No messages yet.", dim())),
            inner,
        );
    } else {
        draw_messages(f, inner, v, &msgs, focused);
    }
}

fn topic_style(topic: &str) -> Style {
    match topic {
        "urgent" | "blocker" => Style::default().fg(Color::Red),
        "question" => Style::default().fg(Color::Yellow),
        "answer" => Style::default().fg(Color::Green),
        "task-update" => Style::default().fg(Color::Cyan),
        _ => dim(),
    }
}

/// Messages as a conversation, the newest at the bottom: a header (time,
/// sender → recipient, topic) and the body wrapped, up to six lines.
fn draw_messages(f: &mut Frame, area: Rect, v: &SwarmBoard, msgs: &[&BusMessage], focused: bool) {
    let width = (area.width as usize).saturating_sub(4).max(16);
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        let selected = focused && i == v.selected_message;
        let to = match m.to_launch.as_deref() {
            None => "everyone".to_string(),
            Some(l) => v.name_of(l),
        };
        let mut b = vec![Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(fmt_clock(m.created_ns), dim()),
            Span::styled(
                format!(" {} → {to} ", v.name_of(&m.from_launch)),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("#{}", m.topic), topic_style(&m.topic)),
        ])];
        let body: Vec<String> = m.body.lines().flat_map(|l| wrap(l, width)).collect();
        for l in body.iter().take(6) {
            b.push(Line::raw(format!("    {l}")));
        }
        if body.len() > 6 {
            b.push(Line::styled("    … Enter reads it all", dim()));
        }
        blocks.push(b);
    }
    // Keep the selected message in view: take blocks from the end until
    // the area is full, starting at the selection when it is above.
    let height = area.height as usize;
    let last = v.selected_message.min(blocks.len().saturating_sub(1));
    let mut start = last;
    let mut used = blocks.get(last).map_or(0, Vec::len);
    while start > 0 && used + blocks[start - 1].len() <= height {
        start -= 1;
        used += blocks[start].len();
    }
    let lines: Vec<Line<'static>> = blocks
        .into_iter()
        .skip(start)
        .flatten()
        .take(height)
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_launch_card(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let lines = vec![
        Line::raw(""),
        Line::styled("  No swarm in this workspace.", head()),
        Line::raw(""),
        Line::raw("  A swarm splits one goal into tasks that agents work on in parallel."),
        Line::raw("  agent-mux runs it: a planner writes the plan, you approve it, and"),
        Line::raw("  agent-mux starts one worker per task, in order, up to your limit."),
        Line::raw(""),
        Line::styled(
            format!(
                "  Workspace: {}",
                crate::app::workspaces_view::tilde_path(&v.workspace)
            ),
            dim(),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  r",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" or "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" starts one."),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_command(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let Some(line) = &v.command else {
        return;
    };
    let preview = if line.purpose == LinePurpose::ReplanNotes {
        Line::from(vec![
            Span::styled(" ↻ re-plan: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                "say what should change; the planner keeps the tasks that ran · Enter send · Esc cancel",
                dim(),
            ),
        ])
    } else {
        match v.command_preview() {
            Some(Ok(out)) => {
                let to = match &out.to {
                    Recipient::Everyone => "everyone".to_string(),
                    Recipient::Launch { name, .. } => name.clone(),
                };
                let how = if out.urgent {
                    "delivered at its next tool call, or wakes it"
                } else {
                    "wakes it if idle"
                };
                let mut spans = vec![
                    Span::styled(" → ", Style::default().fg(Color::Green)),
                    Span::styled(
                        to,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" · {}", out.topic), topic_style(&out.topic)),
                    Span::styled(format!(" · {how}"), dim()),
                ];
                if out.reply_to.is_some() {
                    spans.push(Span::styled(" · answers its message", dim()));
                }
                spans.push(Span::styled(
                    "   Enter send · Tab complete @ · Esc cancel",
                    dim(),
                ));
                Line::from(spans)
            }
            Some(Err(why)) => Line::from(vec![
                Span::styled(" ✗ ", Style::default().fg(Color::Red)),
                Span::styled(why, Style::default().fg(Color::Red)),
                Span::styled("   @all @<task> · ! urgent · #topic · Esc cancel", dim()),
            ]),
            None => Line::raw(""),
        }
    };
    let text = &line.text.text;
    let cursor = line.text.cursor();
    let shown = format!("{}▏{}", &text[..cursor], &text[cursor..]).replace('\n', " ⏎ ");
    let room = (area.width as usize).saturating_sub(3);
    let chars: Vec<char> = shown.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(room)..].iter().collect();
    let input = Line::from(vec![
        Span::styled(
            " > ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(tail, Style::default().fg(Color::White)),
    ]);
    f.render_widget(Paragraph::new(vec![preview, input]), area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn field_line(label: &str, active: bool) -> Line<'static> {
    Line::styled(
        format!("{} {label}", if active { "▸" } else { " " }),
        if active {
            head()
        } else {
            Style::default().fg(Color::Gray)
        },
    )
}

fn text_lines(t: &TextArea, active: bool, max: usize) -> Vec<Line<'static>> {
    let text = if active {
        let c = t.cursor();
        format!("{}▏{}", &t.text[..c], &t.text[c..])
    } else if t.text.is_empty() {
        "—".into()
    } else {
        t.text.clone()
    };
    text.lines()
        .take(max)
        .map(|l| Line::styled(format!("    {l}"), Style::default().fg(Color::White)))
        .collect::<Vec<_>>()
        .into_iter()
        .chain(text.is_empty().then(|| Line::raw("    ")))
        .collect()
}

fn choice(label: &str, value: String, active: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} {label}: ", if active { "▸" } else { " " }),
            if active {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
        Span::styled(
            if active {
                format!("◂ {value} ▸")
            } else {
                value
            },
            Style::default().fg(Color::White),
        ),
    ])
}

fn draw_modal(f: &mut Frame, area: Rect, m: &Modal, v: &SwarmBoard) {
    let (title, lines, footer): (&str, Vec<Line<'static>>, &str) = match m {
        Modal::Launch {
            goal,
            planner,
            worker_harness,
            workers,
            isolation,
            gate,
            check,
            field,
        } => {
            let mut l = vec![field_line(
                "Goal: what done looks like, and what's out of scope",
                *field == LaunchField::Goal,
            )];
            l.extend(text_lines(goal, *field == LaunchField::Goal, 6));
            l.push(Line::raw(""));
            l.push(choice(
                "Coordinator",
                v.harnesses
                    .get(*planner)
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_else(|| "claude".into()),
                *field == LaunchField::Planner,
            ));
            l.push(choice(
                "Workers' harness (tasks that name none)",
                v.harnesses
                    .get(*worker_harness)
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_else(|| "claude".into()),
                *field == LaunchField::WorkerHarness,
            ));
            l.push(choice(
                "Max parallel workers",
                workers.to_string(),
                *field == LaunchField::Workers,
            ));
            l.push(choice(
                "Default isolation",
                isolation.as_str().to_string(),
                *field == LaunchField::Isolation,
            ));
            l.push(choice(
                "Approve the plan before workers start",
                if *gate { "yes".into() } else { "no".into() },
                *field == LaunchField::Gate,
            ));
            l.push(field_line(
                "Check when done (optional command, e.g. cargo test)",
                *field == LaunchField::Check,
            ));
            l.extend(text_lines(check, *field == LaunchField::Check, 1));
            (
                " Start a swarm ",
                l,
                " Tab field · ←→ choose · Alt+Enter newline · Ctrl+S start · Esc cancel ",
            )
        }
        Modal::Task {
            editing,
            retry,
            runs,
            harness,
            agent,
            isolation,
            key,
            title,
            instructions,
            acceptance,
            files,
            depends_on,
            field,
            ..
        } => {
            let mut l = Vec::new();
            if *retry {
                l.push(Line::styled(
                    "Saving puts the task back on the board; change its harness or agent if it got stuck.",
                    Style::default().fg(Color::Yellow),
                ));
            }
            let fields: [(TaskField, &TextArea, usize); 6] = [
                (TaskField::Key, key, 1),
                (TaskField::Title, title, 1),
                (TaskField::Instructions, instructions, 8),
                (TaskField::Acceptance, acceptance, 2),
                (TaskField::Files, files, 2),
                (TaskField::DependsOn, depends_on, 1),
            ];
            for (fl, t, max) in fields {
                if fl == TaskField::Key && editing.is_some() {
                    l.push(Line::styled(format!("  Key: {}", key.text), dim()));
                    continue;
                }
                l.push(field_line(fl.label(), *field == fl));
                l.extend(text_lines(t, *field == fl, max));
            }
            let harness_name = harness
                .checked_sub(1)
                .and_then(|i| v.harnesses.get(i))
                .map(|h| h.as_str().to_string())
                .unwrap_or_else(|| "the workers' harness".into());
            if *runs {
                l.push(choice(
                    "Harness",
                    harness_name,
                    *field == TaskField::Harness,
                ));
                l.push(field_line(
                    TaskField::Agent.label(),
                    *field == TaskField::Agent,
                ));
                l.extend(text_lines(agent, *field == TaskField::Agent, 1));
                l.push(choice(
                    "Isolation",
                    isolation.as_str().to_string(),
                    *field == TaskField::Isolation,
                ));
            } else {
                l.push(Line::styled(
                    format!(
                        "  {harness_name} · agent {} · {} (fixed while it runs; r retries with others)",
                        if agent.text.is_empty() { "none" } else { agent.text.as_str() },
                        isolation.as_str()
                    ),
                    dim(),
                ));
            }
            (
                if *retry {
                    " Retry task "
                } else if editing.is_some() {
                    " Edit task "
                } else {
                    " Add a task "
                },
                l,
                " Tab field · Alt+Enter newline · Ctrl+S save · Esc cancel ",
            )
        }
        Modal::Message { message, scroll } => {
            let mut l = vec![Line::from(vec![
                Span::styled(fmt_clock(message.created_ns), dim()),
                Span::styled(
                    format!(
                        " {} → {} ",
                        v.name_of(&message.from_launch),
                        message
                            .to_launch
                            .as_deref()
                            .map(|t| v.name_of(t))
                            .unwrap_or_else(|| "everyone".into())
                    ),
                    head(),
                ),
                Span::styled(format!("#{}", message.topic), topic_style(&message.topic)),
            ])];
            l.push(Line::raw(""));
            l.extend(
                message
                    .body
                    .lines()
                    .skip(*scroll)
                    .map(|x| Line::raw(x.to_string())),
            );
            (" Message ", l, " ↑↓ scroll · m reply · Esc close ")
        }
    };
    let h = (lines.len() as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(8);
    let r = centered_rect(area.width.saturating_sub(8).min(100), h, area);
    f.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(title, head()))
        .title_bottom(Span::styled(footer, dim()));
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_confirm(f: &mut Frame, area: Rect, c: &Confirm) {
    let q = c.question();
    let r = centered_rect((q.chars().count() as u16 + 6).clamp(40, 90), 5, area);
    f.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Confirm ",
            Style::default().fg(Color::Yellow),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(format!(" {q}")),
            Line::styled(" y yes · any other key no", dim()),
        ])
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn indented(r: Rect) -> Rect {
    Rect::new(r.x + 1, r.y, r.width.saturating_sub(2), r.height)
}

fn fmt_clock(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .map(|t| {
            let off = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
            t.to_offset(off)
        })
        .map(|t| format!("{:02}:{:02}", t.hour(), t.minute()))
        .unwrap_or_else(|| "--:--".into())
}

fn wrap(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split(' ') {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    out.push(cur);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}
