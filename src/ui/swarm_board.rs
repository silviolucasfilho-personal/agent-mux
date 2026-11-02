//! Drawing the Swarm screen (`crate::app::swarm_board`): the mission strip,
//! the Needs-you strip, the board, the panel on the selected task (Task,
//! Live, Thread, Changes) or the swarm's Activity, the footer or the `>`
//! command line, and the forms and confirmations over them (§8.1 of
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`).

use super::flow::{dim, head, hints};
use crate::app::App;
use crate::app::swarm_board::{
    Confirm, Focus, LaunchField, LinePurpose, Modal, NeedKind, Recipient, SwarmBoard, Tab,
    TaskField,
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
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

pub fn draw(f: &mut Frame, area: Rect, v: &SwarmBoard, app: &App) {
    f.render_widget(Clear, area);
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(mission_strip(v, area.width.saturating_sub(4) as usize));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let shown = v
        .needs
        .len()
        .min(if v.focus == Focus::Needs { 5 } else { 3 });
    let needs_h = if v.needs.is_empty() {
        1
    } else {
        1 + shown as u16
    };
    let foot_h = if v.command.is_some() { 2 } else { 1 };
    let [needs_area, body, foot] = Layout::vertical([
        Constraint::Length(needs_h.min(inner.height.saturating_sub(4))),
        Constraint::Min(0),
        Constraint::Length(foot_h),
    ])
    .areas(inner);
    draw_needs(f, needs_area, v, shown);

    if v.swarm.is_none() {
        draw_launch_card(f, body, v);
    } else {
        let board_w = (body.width * 55 / 100).clamp(40.min(body.width), 96);
        let [board_area, panel_area] =
            Layout::horizontal([Constraint::Length(board_w), Constraint::Min(0)]).areas(body);
        draw_board(f, board_area, v);
        if v.activity {
            draw_activity(f, panel_area, v);
        } else {
            draw_panel(f, panel_area, v, app);
        }
    }
    draw_footer(f, foot, v);
    if let Some(m) = &v.modal {
        draw_modal(f, area, m, v);
    }
    if let Some(c) = &v.confirm {
        draw_confirm(f, area, c);
    }
}

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

/// The top border: goal, state, tasks done and a bar, workers against the
/// cap, elapsed time, tokens and cost. The goal gives way first.
fn mission_strip(v: &SwarmBoard, width: usize) -> Line<'static> {
    let Some(s) = &v.swarm else {
        return Line::from(vec![
            Span::styled(" ◆ Swarm ", head()),
            Span::styled(
                format!(
                    "· {} · no swarm ",
                    crate::app::workspaces_view::tilde_path(&v.workspace)
                ),
                dim(),
            ),
        ]);
    };
    let total = v.board.len();
    let done = v
        .board
        .iter()
        .filter(|t| t.task.status == TaskStatus::Done)
        .count();
    let bar_w = 10usize;
    let filled = (done * bar_w).checked_div(total).unwrap_or(0);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_w - filled));
    let (tokens, cost) = v
        .board
        .iter()
        .filter_map(|t| v.worker_of(t))
        .fold((0i64, 0f64), |(t, c), w| (t + w.tokens, c + w.cost_usd));
    let elapsed = crate::bus::swarm::short_age(v.now_ns - s.started_ns);
    let ended = if s.ended_ns.is_some() {
        " · closed"
    } else {
        ""
    };
    let tail = format!(
        " · {}{ended} · {done}/{total} done {bar} · {}/{} workers · {elapsed} · {} tok {} ",
        state_label(s.status),
        v.busy_workers(),
        s.max_workers,
        fmt_tokens(Some(tokens)),
        fmt_cost(Some(cost)),
    );
    let room = width.saturating_sub(tail.chars().count() + 4);
    Line::from(vec![
        Span::styled(format!(" ◆ {}", truncate(&s.goal, room.max(8))), head()),
        Span::styled(tail, Style::default().fg(Color::Gray)),
    ])
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
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_needs(f: &mut Frame, area: Rect, v: &SwarmBoard, shown: usize) {
    if area.height == 0 {
        return;
    }
    if v.needs.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ✓ ", Style::default().fg(Color::Green)),
                Span::styled("Nothing needs you.", dim()),
            ])),
            area,
        );
        return;
    }
    let focused = v.focus == Focus::Needs;
    let width = area.width as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" ⚑ NEEDS YOU ({}) ", v.needs.len()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if focused {
                "  ↑↓ choose · Enter act · d dismiss · Esc back"
            } else {
                "  ! to act"
            },
            dim(),
        ),
    ])];
    let start = if focused {
        super::sidebar_window(v.selected_need, v.needs.len(), shown.max(1))
    } else {
        0
    };
    for (i, need) in v.needs.iter().enumerate().skip(start).take(shown) {
        let selected = focused && i == v.selected_need;
        let kind_style = match need.kind {
            NeedKind::Failed | NeedKind::Lost | NeedKind::PlannerFailed => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
            NeedKind::Question | NeedKind::Prompt | NeedKind::Plan | NeedKind::Stalled => {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            }
            NeedKind::Review | NeedKind::Proposed => Style::default().fg(Color::Cyan),
            NeedKind::Finished => Style::default().fg(Color::Green),
        };
        let label = format!("{:<10}", need.kind.label());
        let verbs = format!("  {}", need.kind.verbs());
        let room = width.saturating_sub(4 + label.len() + verbs.chars().count());
        lines.push(
            Line::from(vec![
                Span::styled(
                    if selected { " ▸ " } else { "   " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(label, kind_style),
                Span::styled(
                    truncate(&need.text, room),
                    Style::default().fg(Color::White),
                ),
                Span::styled(verbs, dim()),
            ])
            .style(if selected {
                Style::default().bg(Color::Rgb(52, 44, 20))
            } else {
                Style::default()
            }),
        );
    }
    let hidden = v.needs.len().saturating_sub(shown);
    if hidden > 0
        && !focused
        && let Some(last) = lines.last_mut()
    {
        last.spans
            .push(Span::styled(format!("   +{hidden} more"), dim()));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_board(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let focused = v.focus == Focus::Board;
    let s = v.swarm.as_ref().expect("a swarm");
    let summary = s
        .plan_summary
        .as_deref()
        .filter(|x| !x.is_empty())
        .map(|x| format!(" · \"{x}\""))
        .unwrap_or_default();
    let title = format!(
        " Plan ({}) · planner: {}{summary} ",
        v.board.len(),
        s.planner_harness
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            truncate(&title, area.width.saturating_sub(4) as usize),
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.board.is_empty() {
        let mut lines = vec![Line::raw("")];
        match (s.status, &s.plan_error) {
            (SwarmStatus::Planning, _) => {
                lines.push(Line::styled(
                    format!(
                        "  ◌ {} is planning: {}",
                        s.planner_harness,
                        truncate(&s.goal, 60)
                    ),
                    Style::default().fg(Color::Yellow),
                ));
                lines.push(Line::styled(
                    "  Its output is on the right; Enter attaches to it.",
                    dim(),
                ));
            }
            (_, Some(e)) => {
                lines.push(Line::styled(
                    "  The planner gave no plan:",
                    Style::default().fg(Color::Red),
                ));
                lines.push(Line::raw(format!("  {e}")));
                lines.push(Line::styled(
                    "  p re-plans with your notes; n adds a task by hand.",
                    dim(),
                ));
            }
            _ => lines.push(Line::styled("  No tasks. n adds one.", dim())),
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }

    let width = inner.width as usize;
    let rows_visible = inner.height as usize;
    let start = super::sidebar_window(v.selected, v.board.len(), rows_visible.max(1));
    let key_w = v
        .board
        .iter()
        .map(|t| t.ext.plan_key.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 18);
    let mut lines = Vec::new();
    for (i, t) in v.board.iter().enumerate().skip(start).take(rows_visible) {
        let st = v.shown(t);
        let selected = i == v.selected;
        let problems: Vec<(bool, &str)> = v.problems.for_task(&t.ext.plan_key).collect();
        let mark = if problems.iter().any(|(e, _)| *e) {
            Span::styled(
                "!",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        } else if !problems.is_empty() {
            Span::styled("?", Style::default().fg(Color::Yellow))
        } else {
            Span::raw(" ")
        };
        let who = match v.worker_of(t) {
            Some(w) => w.provider.clone(),
            None => t.task.target_harness.clone().unwrap_or_default(),
        };
        let deps = if t.ext.depends_on.is_empty() {
            String::new()
        } else {
            format!(" ← {}", t.ext.depends_on.join(","))
        };
        let money = v
            .worker_of(t)
            .map(|w| format!(" {}", fmt_cost(Some(w.cost_usd))))
            .unwrap_or_default();
        let right = format!("{deps}  {who}{money}");
        let left = format!("{:>2} {:<key_w$} ", i + 1, truncate(&t.ext.plan_key, key_w));
        let room = width.saturating_sub(4 + left.chars().count() + right.chars().count());
        lines.push(
            Line::from(vec![
                Span::styled(
                    if selected { "▸" } else { " " },
                    Style::default().fg(Color::Cyan),
                ),
                mark,
                Span::styled(format!("{} ", st.glyph()), shown_style(st)),
                Span::styled(left, Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{:<room$}", truncate(&t.task.title, room)),
                    if st == Shown::Cancelled {
                        dim()
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(right, dim()),
            ])
            .style(if selected && focused {
                Style::default().bg(Color::Rgb(30, 40, 56))
            } else {
                Style::default()
            }),
        );
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_panel(f: &mut Frame, area: Rect, v: &SwarmBoard, app: &App) {
    let focused = v.focus == Focus::Panel;
    let task = v.selected_task();
    let title = match task {
        Some(t) => {
            let st = v.shown(t);
            format!(" {} · {} · {} ", t.ext.plan_key, st.as_str(), t.task.title)
        }
        None => " Planner ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            truncate(&title, area.width.saturating_sub(4) as usize),
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(t) = task else {
        // While planning, the planner's terminal.
        match v
            .planner_session()
            .and_then(|p| app.sessions.get(p.session_idx))
        {
            Some(s) => f.render_widget(PseudoTerminal::new(s.parser.screen()), inner),
            None => f.render_widget(
                Paragraph::new(Line::styled("  Nothing selected.", dim())),
                inner,
            ),
        }
        return;
    };

    let [tabs_area, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let current = v.tab();
    let thread_len = v.thread().len();
    let compact = inner.width < 52;
    let pad = if compact { "" } else { " " };
    let mut spans: Vec<Span<'static>> = vec![Span::raw(pad)];
    for (n, tab) in Tab::ALL.iter().enumerate() {
        let label = match tab {
            Tab::Thread if thread_len > 0 && !compact => format!("{} Thread ({thread_len})", n + 1),
            _ => format!("{} {}", n + 1, tab.label()),
        };
        spans.push(if *tab == current {
            Span::styled(
                format!("{pad}{label}{pad}"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!("{pad}{label}{pad}"), dim())
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
        Tab::Live => match v.worker_of(t).and_then(|w| app.sessions.get(w.session_idx)) {
            Some(s) => f.render_widget(PseudoTerminal::new(s.parser.screen()), content),
            None => f.render_widget(
                Paragraph::new(vec![
                    Line::raw(""),
                    Line::styled(
                        match v.shown(t) {
                            Shown::Draft | Shown::Ready | Shown::Blocked | Shown::Proposed => {
                                "  No worker yet: agent-mux starts one when the task is ready."
                            }
                            Shown::Lost => "  Its session exited mid-task: r retries it.",
                            _ => "  No terminal: its worker is gone or runs in another window.",
                        },
                        dim(),
                    ),
                ]),
                content,
            ),
        },
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

fn draw_footer(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    if v.command.is_some() {
        draw_command(f, area, v);
        return;
    }
    if let Some(msg) = &v.status_msg {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!(" {msg}"),
                Style::default().fg(Color::Yellow),
            )),
            area,
        );
        return;
    }
    let status = v
        .swarm
        .as_ref()
        .filter(|s| s.ended_ns.is_none())
        .map(|s| s.status);
    let lists = v.activity || v.tab() == Tab::Thread;
    let pairs: &[(&str, &str)] = if v.focus == Focus::Needs {
        &[
            ("↑↓", "need"),
            ("Enter", "act"),
            ("d", "dismiss"),
            ("Esc", "board"),
        ]
    } else if status.is_none() {
        &[("r", "start a swarm"), ("Esc", "close")]
    } else if v.focus == Focus::Panel && lists {
        &[
            ("↑↓", "message"),
            ("Enter", "read"),
            ("m", "reply"),
            ("1-4", "view"),
            ("Tab/Esc", "board"),
        ]
    } else if v.focus == Focus::Panel {
        &[
            ("↑↓", "scroll"),
            ("1-4", "view"),
            ("a", "activity"),
            ("Tab/Esc", "board"),
        ]
    } else if status == Some(SwarmStatus::Review) {
        &[
            ("y", "approve"),
            ("p", "re-plan"),
            ("e", "edit"),
            ("n", "add"),
            ("d", "delete"),
            ("↑↓", "task"),
            ("1-4", "view"),
            ("Esc", "close"),
        ]
    } else if status == Some(SwarmStatus::Finished) {
        &[
            ("y", "close the swarm"),
            ("↑↓", "task"),
            ("1-4", "view"),
            ("a", "activity"),
            ("Esc", "close"),
        ]
    } else {
        &[
            ("↑↓", "task"),
            ("Enter", "attach"),
            ("1-4", "view"),
            (">", "message"),
            ("space", "pause"),
            ("r", "retry"),
            ("x", "stop"),
            ("!", "needs"),
            ("a", "activity"),
            ("Esc", "close"),
        ]
    };
    f.render_widget(Paragraph::new(hints(pairs)), area);
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
                "Planner",
                v.harnesses
                    .get(*planner)
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_else(|| "claude".into()),
                *field == LaunchField::Planner,
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
            key,
            title,
            instructions,
            acceptance,
            files,
            depends_on,
            field,
        } => {
            let mut l = Vec::new();
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
            (
                if editing.is_some() {
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
