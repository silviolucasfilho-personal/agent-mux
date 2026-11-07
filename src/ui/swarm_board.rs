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
    SwarmBoard, SwarmTab, Tab, TaskField,
};
use crate::app::text_area::TextArea;
use crate::bus::swarm::engine::{self, Shown};
use crate::bus::swarm::record::{BoardTask, SwarmRun, SwarmStatus};
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
                    dispatch_grace_ns: engine::DISPATCH_GRACE_NS,
                    review: d.review.enabled,
                };
                let st = engine::shown(t, &input);
                let needs = v.row_needs(*row);
                let plan_error = i
                    == v.rows
                        .get(v.cursor)
                        .map(|r| match r {
                            Row::Swarm(k)
                            | Row::Coordinator(k)
                            | Row::Task(k, _)
                            | Row::Agent(k, ..) => *k,
                        })
                        .unwrap_or(usize::MAX)
                    && v.problems.for_task(&t.ext.plan_key).any(|(e, _)| e);
                let harness = v
                    .worker_of(t)
                    .map(|w| w.provider.clone())
                    .or_else(|| t.task.target_harness.clone())
                    .unwrap_or_default();
                // with room: harness · elapsed · cost (§9.2)
                let harness = if inner_w >= 44 && t.ext.attempts > 0 {
                    let since =
                        t.task.ended_ns.unwrap_or(v.now_ns) - t.task.updated_ns.min(v.now_ns);
                    let age = crate::bus::swarm::short_age(since.max(0));
                    match v.worker_of(t) {
                        Some(w) if w.priced => {
                            format!("{harness} {age} ${:.2}", w.cost_usd)
                        }
                        _ => format!("{harness} {age}"),
                    }
                } else {
                    harness
                };
                let fold = if v.has_folded_agents(i, j) {
                    " ▸"
                } else {
                    ""
                };
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
                // the review's verdict beside the task it reviews (v25)
                let reviewed = crate::bus::swarm::review::state(t, &d.board, d.review.enabled)
                    .map(|r| r.glyph())
                    .filter(|g| !g.is_empty())
                    .map(|g| format!(" {g}"))
                    .unwrap_or_default();
                let lead = if t.ext.review_of.is_some() {
                    "      ↳"
                } else {
                    "   "
                };
                (
                    format!(
                        "{lead} {} {} {}{reviewed}{fold}",
                        st.glyph(),
                        j + 1,
                        t.ext.plan_key
                    ),
                    shown_style(st),
                    right,
                    rstyle,
                )
            }
            Row::Agent(..) => {
                let a = v.agent_of(*row);
                let (name, age, live) = match a {
                    Some(a) => (
                        a.name.clone(),
                        crate::bus::swarm::short_age(
                            a.end_ns.unwrap_or(v.now_ns).saturating_sub(a.start_ns),
                        ),
                        a.end_ns.is_none(),
                    ),
                    None => (String::new(), String::new(), false),
                };
                (
                    format!("        ↳ {name}"),
                    if live {
                        Style::default().fg(Color::Green)
                    } else {
                        dim()
                    },
                    age,
                    dim(),
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
                " + new swarm  n · other swarms  o"
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
    match v.pane() {
        PaneView::Session(idx) => super::draw_session_pane(f, body, app, idx, now),
        PaneView::SwarmCard if v.swarm.is_some() => match v.swarm_tab {
            SwarmTab::Board => draw_swarm_card(f, body, v),
            SwarmTab::Live => draw_live_mosaic(f, body, v, app, now),
            SwarmTab::Timeline => draw_swarm_timeline(f, body, v),
            SwarmTab::Log => draw_swarm_log(f, body, v),
            SwarmTab::Activity => draw_activity(f, body, v),
            SwarmTab::Changes => draw_swarm_changes(f, body, v),
        },
        PaneView::SwarmCard => draw_swarm_card(f, body, v),
        PaneView::TaskCard => draw_task_card(f, body, v),
        PaneView::Launch => draw_launch_card(f, body, v),
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
        Some(Row::Swarm(i))
        | Some(Row::Coordinator(i))
        | Some(Row::Task(i, _))
        | Some(Row::Agent(i, ..)) => *i,
        None => 0,
    };
    let block = card_block(format!(" ◆ {} ", s.goal), v.focus == Focus::Panel);
    let whole = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, inner] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(whole);
    draw_swarm_tabs(f, tabs_area, v);
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
    if s.status == SwarmStatus::Review && s.gate_mode == crate::bus::swarm::record::GateMode::Chat {
        out.push(Line::styled(
            "waiting for you to confirm in the coordinator's chat — Enter on ★ (or `agent-mux swarm approve`)",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(d) = &v.launch_decision {
        out.push(Line::styled(
            format!(
                "confirmed {} {}{}",
                match d.by {
                    crate::bus::swarm::record::DecidedBy::Chat => "in the chat",
                    crate::bus::swarm::record::DecidedBy::Tui => "on the screen",
                    crate::bus::swarm::record::DecidedBy::Cli => "from the CLI",
                },
                crate::tracing::cli::fmt_time(d.created_ns),
                d.said
                    .as_deref()
                    .map(|w| format!(" “{w}”"))
                    .unwrap_or_default()
            ),
            dim(),
        ));
    } else if s.gate_mode == crate::bus::swarm::record::GateMode::Off && s.status.approved() {
        out.push(Line::styled(
            "gate off: ran as soon as it checked out",
            dim(),
        ));
    }
    if let Some(sum) = s.plan_summary.as_deref().filter(|x| !x.is_empty()) {
        out.push(Line::raw(sum.to_string()));
    }
    // the plan as a table, with each task's live state (§9.3)
    for (j, t) in v.board.iter().enumerate() {
        let st = v.shown(t);
        let who = match (&t.task.target_harness, &t.task.target_agent) {
            (Some(h), Some(a)) => format!("{h}·{a}"),
            (Some(h), None) => h.clone(),
            (None, Some(a)) => format!("·{a}"),
            (None, None) => String::new(),
        };
        let deps = if t.ext.depends_on.is_empty() {
            String::new()
        } else {
            format!(" ← {}", t.ext.depends_on.join(" "))
        };
        out.push(Line::from(vec![
            Span::styled(format!("{} {:<9}", st.glyph(), st.word()), shown_style(st)),
            Span::styled(
                format!(" {:>2} {:<16}", j + 1, truncate(&t.ext.plan_key, 16)),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!(" {}", truncate(&t.task.title, 40)),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(format!("  {who}{deps}"), dim()),
        ]));
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
            (true, _) => "This swarm is closed. n starts a new one · o another swarm",
            (_, SwarmStatus::Review) => {
                "y approve · p re-plan · n add a task · ↓ a task: e edit, d delete"
            }
            (_, SwarmStatus::Running) => {
                "space pause · > message · ! next need · n add a task · o another swarm"
            }
            (_, SwarmStatus::Paused) => "space resume · > message · ! next need · o another swarm",
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
/// The swarm row's tab bar: `1 Board · 2 Live · 3 Activity · 4 Changes`.
fn draw_swarm_tabs(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let live = v.live_tiles().len();
    for (n, tab) in SwarmTab::ALL.iter().enumerate() {
        let label = match tab {
            SwarmTab::Live if live > 0 => format!("{} Live ({live})", n + 1),
            SwarmTab::Log if !v.events.is_empty() => {
                format!("{} Log ({})", n + 1, v.events.len())
            }
            SwarmTab::Activity if !v.messages.is_empty() => {
                format!("{} Activity ({})", n + 1, v.messages.len())
            }
            _ => format!("{} {}", n + 1, tab.label()),
        };
        spans.push(if *tab == v.swarm_tab {
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
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Live tab: every live terminal of the swarm at once (§9.5). The
/// coordinator's tile first, then the workers in plan order, in a grid
/// (1×1, 1×2, 2×2, 2×3; more than six page). A tile shows the bottom of
/// its session's screen, as text: a PTY is never resized for a tile, so a
/// worker's own terminal is left as it is. `↑↓←→` move between tiles,
/// `Enter` connects to one.
fn draw_live_mosaic(f: &mut Frame, area: Rect, v: &SwarmBoard, app: &App, now: Instant) {
    let focused = v.focus == Focus::Panel;
    let title = match &v.swarm {
        Some(s) => format!(" ◆ {} · live ", s.goal),
        None => " live ".into(),
    };
    let block = card_block(title, focused);
    let whole = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(whole);
    draw_swarm_tabs(f, tabs_area, v);
    let tiles = v.live_tiles();
    if tiles.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled(
                    "  No live terminal: the coordinator and the workers have exited (Enter on a worker reconnects it).",
                    dim(),
                ),
            ]),
            body,
        );
        return;
    }
    let per_page = 6usize;
    let page = v.tile_cursor / per_page;
    let shown: Vec<&(String, usize)> = tiles.iter().skip(page * per_page).take(per_page).collect();
    let (cols, rows) = match shown.len() {
        1 => (1, 1),
        2 => (2, 1),
        3 | 4 => (2, 2),
        _ => (3, 2),
    };
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(body);
    for (n, (launch, idx)) in shown.iter().enumerate() {
        let r = n / cols;
        let c = n % cols;
        let cols_areas =
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(row_areas[r]);
        let tile = cols_areas[c];
        let Some(session) = app.sessions.get(*idx) else {
            continue;
        };
        let is_cursor = page * per_page + n == v.tile_cursor;
        let who = match &v.swarm {
            Some(s) if s.planner_launch.as_deref() == Some(launch.as_str()) => {
                format!("★ coordinator · {}", s.planner_harness)
            }
            _ => v
                .board
                .iter()
                .position(|t| t.task.assigned_to.as_deref() == Some(launch.as_str()))
                .map(|j| {
                    let t = &v.board[j];
                    format!(
                        "{} {} · {} · {}",
                        j + 1,
                        t.ext.plan_key,
                        v.worker_of(t)
                            .map(|w| w.provider.clone())
                            .unwrap_or_default(),
                        v.shown(t).word()
                    )
                })
                .unwrap_or_else(|| crate::bus::short_id(launch).to_string()),
        };
        let (label, _) = super::status_label_style(session.status(now));
        let needs = v
            .board
            .iter()
            .find(|t| t.task.assigned_to.as_deref() == Some(launch.as_str()))
            .map(|t| {
                v.needs
                    .iter()
                    .any(|n| n.task_id.as_deref() == Some(t.task.id.as_str()))
            })
            .unwrap_or(false);
        let tile_block = Block::default()
            .borders(Borders::ALL)
            .border_style(if is_cursor && focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(Span::styled(
                format!(" {who} [{label}]{} ", if needs { " !" } else { "" }),
                if needs {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if is_cursor {
                    head()
                } else {
                    Style::default().fg(Color::Gray)
                },
            ));
        let inner = tile_block.inner(tile);
        f.render_widget(tile_block, tile);
        if inner.height == 0 || inner.width == 0 {
            continue;
        }
        // the bottom of the screen, as text, cut to the tile's width
        let screen = session.parser.screen();
        let all: Vec<String> = screen.rows(0, inner.width).collect();
        let last_full = all
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        let start = last_full.saturating_sub(inner.height as usize);
        let lines: Vec<Line<'static>> = all[start..last_full]
            .iter()
            .map(|l| Line::styled(l.clone(), Style::default().fg(Color::White)))
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }
    if tiles.len() > per_page {
        let note = format!(
            " {}–{} of {} · PgUp/PgDn ",
            page * per_page + 1,
            (page * per_page + shown.len()).min(tiles.len()),
            tiles.len()
        );
        let w = note.chars().count() as u16;
        if body.width > w {
            f.render_widget(
                Paragraph::new(Line::styled(note, dim())),
                Rect::new(
                    body.x + body.width - w,
                    body.y + body.height.saturating_sub(1),
                    w,
                    1,
                ),
            );
        }
    }
}

/// The swarm's Changes tab: every worktree task's branch against the base,
/// then the workspace's own diff (§9.3).
fn draw_swarm_changes(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    let focused = v.focus == Focus::Panel;
    let title = match &v.swarm {
        Some(s) => format!(" ◆ {} · changes ", s.goal),
        None => " changes ".into(),
    };
    let block = card_block(title, focused);
    let whole = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(whole);
    draw_swarm_tabs(f, tabs_area, v);
    f.render_widget(
        Paragraph::new(diff_lines(v, content.height as usize)),
        content,
    );
}

/// `diff_lines` coloured as a diff, from the panel's scroll.
fn diff_lines(v: &SwarmBoard, height: usize) -> Vec<Line<'static>> {
    v.diff_lines
        .iter()
        .skip(v.panel_scroll)
        .take(height)
        .map(|l| {
            let style = if l.starts_with('+') && !l.starts_with("+++") {
                Style::default().fg(Color::Green)
            } else if l.starts_with('-') && !l.starts_with("---") {
                Style::default().fg(Color::Red)
            } else if l.starts_with("@@") || l.starts_with("diff ") || l.starts_with("── ") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::styled(l.clone(), style)
        })
        .collect()
}

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
                        Shown::Lost => "  Its session exited mid-task: Enter reconnects it (the same conversation), r retries the task.",
                        _ => "  No terminal here: its worker is gone or runs in another window. Enter reconnects to its conversation.",
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
        Tab::Trace => {
            let lines: Vec<Line<'static>> = v
                .trace_lines
                .iter()
                .skip(v.panel_scroll)
                .take(content.height as usize)
                .map(|l| {
                    if l.starts_with("turn ") {
                        Line::styled(l.clone(), head())
                    } else if l.contains(" ✗") {
                        Line::styled(l.clone(), Style::default().fg(Color::Red))
                    } else if l.contains(" agent ") {
                        Line::styled(l.clone(), Style::default().fg(Color::Cyan))
                    } else {
                        Line::raw(l.clone())
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines), indented(content));
        }
        Tab::Changes => {
            f.render_widget(
                Paragraph::new(diff_lines(v, content.height as usize)),
                content,
            );
        }
    }
}

/// A lane's colour, by its task's state word.
fn lane_style(state: &str) -> Style {
    match state {
        "working" | "starting" | "running" => Style::default().fg(Color::Green),
        "asking" | "prompt" | "stalled" | "review" | "planning" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "failed" | "lost" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "done" | "closed" | "finished" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::DIM),
        "coordinator" | "integrating" | "paused" => Style::default().fg(Color::Cyan),
        _ => dim(),
    }
}

/// The Timeline tab (§9.3): one lane per agent, the coordinator first,
/// sub-agents indented under their worker, time left to right from the
/// swarm's start; `█` where the agent worked, `░` while it was alive, `?`
/// while it waited on an answer, marks for decisions and task events.
/// `←`/`→` pan, `-`/`+` zoom, `0` the whole swarm.
fn draw_swarm_timeline(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    use crate::tracing::swarm_report::{LEGEND, lane_cells};
    let focused = v.focus == Focus::Panel;
    let block = card_block(
        format!(
            " Timeline · {}× {}· ←→ pan · -+ zoom · 0 all ",
            v.tl_zoom,
            if v.tl_pan != 0 { "panned " } else { "" }
        ),
        focused,
    );
    let whole = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, inner] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(whole);
    draw_swarm_tabs(f, tabs_area, v);
    let (Some(tl), Some(w)) = (&v.timeline, v.timeline_window()) else {
        f.render_widget(
            Paragraph::new(Line::styled("  Nothing traced yet.", dim())),
            inner,
        );
        return;
    };
    let width = inner.width as usize;
    let label_w = (width / 3).clamp(12, 32).min(width.saturating_sub(10));
    let track = width.saturating_sub(label_w + 2);
    let mut lines: Vec<Line<'static>> = vec![Line::styled(
        format!(
            "{:<label_w$}  {}",
            format!(
                "{} → {}",
                &crate::tracing::cli::fmt_time(w.start_ns)[11..16],
                &crate::tracing::cli::fmt_time(w.end_ns)[11..16]
            ),
            crate::tracing::view::axis(&w, track)
        ),
        dim(),
    )];
    for lane in tl.lanes.iter().skip(v.panel_scroll) {
        let label = format!("{}{}", "  ".repeat(lane.depth.min(3)), lane.label);
        let cells = lane_cells(lane, &w, track, v.now_ns);
        let mut spans = vec![Span::styled(
            format!("{:<label_w$}  ", truncate(&label, label_w)),
            if lane.depth > 0 {
                dim()
            } else {
                lane_style(&lane.state)
            },
        )];
        let style = lane_style(&lane.state);
        let mut run = String::new();
        let mut run_kind: Option<u8> = None;
        let flush = |spans: &mut Vec<Span<'static>>, run: &mut String, kind: Option<u8>| {
            if run.is_empty() {
                return;
            }
            let st = match kind {
                Some(0) => dim(),                              // alive
                Some(1) => style,                              // working
                Some(2) => Style::default().fg(Color::Yellow), // asking
                Some(3) => Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD), // a mark
                _ => Style::default(),
            };
            spans.push(Span::styled(std::mem::take(run), st));
        };
        for c in cells {
            let kind = match c {
                '░' => Some(0),
                '█' | '▶' => Some(1),
                '?' => Some(2),
                ' ' => None,
                _ => Some(3),
            };
            if kind != run_kind {
                flush(&mut spans, &mut run, run_kind);
                run_kind = kind;
            }
            run.push(c);
        }
        flush(&mut spans, &mut run, run_kind);
        lines.push(Line::from(spans));
    }
    let height = inner.height as usize;
    if lines.len() + 2 <= height {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!("{:<label_w$}  {LEGEND}", ""), dim()));
    } else {
        lines.truncate(height.saturating_sub(1));
        lines.push(Line::styled(format!("{:<label_w$}  {LEGEND}", ""), dim()));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The Log tab (§9.3): `swarm_events` newest last, one line each (time,
/// kind, actor, detail); `t` cycles the kinds (all, decisions, tasks,
/// messages), `/` filters by text, `Enter` opens the row the event is about.
fn draw_swarm_log(f: &mut Frame, area: Rect, v: &SwarmBoard) {
    use crate::tracing::swarm_report::LogKind;
    let focused = v.focus == Focus::Panel;
    let block = card_block(
        " Log · t kinds · / filter · Enter opens the row ".to_string(),
        focused,
    );
    let whole = block.inner(area);
    f.render_widget(block, area);
    let [tabs_area, head_area, inner] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(whole);
    draw_swarm_tabs(f, tabs_area, v);
    let mut head_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for k in LogKind::ALL {
        head_spans.push(if k == v.log_kind {
            Span::styled(
                format!(" {} ", k.label()),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            )
        } else {
            Span::styled(format!(" {} ", k.label()), dim())
        });
    }
    head_spans.push(Span::raw("   "));
    head_spans.push(if v.log_editing {
        Span::styled(
            format!("/{}▏", v.log_filter),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if !v.log_filter.is_empty() {
        Span::styled(format!("/{}", v.log_filter), head())
    } else {
        Span::styled("/ filter", dim())
    });
    f.render_widget(Paragraph::new(Line::from(head_spans)), head_area);
    let entries = v.log_entries();
    if entries.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                if v.events.is_empty() {
                    "  Nothing logged yet."
                } else {
                    "  No event matches."
                },
                dim(),
            )),
            inner,
        );
        return;
    }
    let height = inner.height as usize;
    let selected = v.selected_message.min(entries.len() - 1);
    let start = (selected + 1).saturating_sub(height);
    let lines: Vec<Line<'static>> = entries
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, e)| {
            let line = crate::bus::swarm::view::event_line(e);
            let style = if e.kind.starts_with("decision.") || e.kind.starts_with("plan.") {
                Style::default().fg(Color::Cyan)
            } else if e.kind.ends_with(".failed") || e.kind == "task.lost" {
                Style::default().fg(Color::Red)
            } else if e.kind == "task.done" || e.kind == "check.done" {
                Style::default().fg(Color::Green)
            } else if e.kind.starts_with("question.") || e.kind == "message" {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            let marker = if focused && i == selected {
                "▸ "
            } else {
                "  "
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(line, style),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
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
    // its review (v25): where it stands, by whom, and the findings
    let review = v.review_settings();
    if let Some(state) = crate::bus::swarm::review::state(t, &v.board, review.enabled) {
        use crate::bus::swarm::review::ReviewState as R;
        out.push(Line::raw(""));
        out.push(section("Review"));
        let by = t
            .ext
            .review_task
            .as_deref()
            .and_then(|id| v.board.iter().find(|o| o.task.id == id));
        let who = by
            .map(|r| {
                format!(
                    " · `{}` on {}",
                    r.ext.plan_key,
                    r.task.target_harness.as_deref().unwrap_or("?")
                )
            })
            .unwrap_or_default();
        let color = match state {
            R::Approved => Color::Green,
            R::Changes | R::Failed => Color::Red,
            R::Unclear => Color::Yellow,
            _ => Color::Gray,
        };
        out.push(Line::styled(
            format!("  {} {}{who}", state.glyph(), state.as_str()),
            Style::default().fg(color),
        ));
        if let Some(text) = by.and_then(crate::bus::swarm::review::result_text) {
            out.extend(text.lines().map(|l| Line::raw(format!("  {l}"))));
        }
        if state == R::Changes {
            out.push(Line::styled(
                "  Tell the coordinator to send it back to the worker, then `review` it again.",
                dim(),
            ));
        }
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
        Line::from(vec![
            Span::styled(
                "  o",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" opens a swarm that runs elsewhere."),
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

/// The workers' pool: a tick box per harness, the cursor's in brackets.
fn pool_line(v: &SwarmBoard, pool: &[bool], cursor: usize, active: bool) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{}Workers may run on: ", if active { "▸ " } else { "  " }),
        if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        },
    )];
    for (i, h) in v.harnesses.iter().enumerate() {
        let ticked = pool.get(i).copied().unwrap_or(false);
        let text = format!("[{}] {}", if ticked { "x" } else { " " }, h.as_str());
        let mut style = Style::default().fg(if ticked {
            Color::White
        } else {
            Color::DarkGray
        });
        if active && i == cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        spans.push(Span::styled(text, style));
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

fn draw_modal(f: &mut Frame, area: Rect, m: &Modal, v: &SwarmBoard) {
    let (title, lines, footer): (&str, Vec<Line<'static>>, &str) = match m {
        Modal::Launch {
            goal,
            planner,
            pool,
            pool_cursor,
            workers,
            isolation,
            gate,
            check,
            field,
            error,
            advanced,
            review,
            ..
        } => {
            let mut l = Vec::new();
            if let Some(e) = error {
                l.push(Line::styled(
                    e.clone(),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
                l.push(Line::raw(""));
            }
            l.push(choice(
                "Coordinator",
                v.harnesses
                    .get(*planner)
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_else(|| "claude".into()),
                *field == LaunchField::Planner,
            ));
            l.push(choice(
                "Reviews",
                v.review_label(*review),
                *field == LaunchField::Review,
            ));
            l.push(Line::raw(""));
            for t in [
                "  The coordinator opens as a session. Tell it the goal there; it asks",
                "  what it needs, proposes the plan as a table and starts the workers",
                "  once you say go. The screen mirrors the plan; you confirm in the chat.",
                "  Each finished task is reviewed read-only before you merge it.",
            ] {
                l.push(Line::styled(t, dim()));
            }
            l.push(Line::raw(""));
            l.push(field_line(
                &format!(
                    "{} Advanced: goal now, workers' pool, limits, isolation, gate, check",
                    if *advanced { "▾" } else { "▸" }
                ),
                *field == LaunchField::Advanced,
            ));
            if *advanced {
                l.push(Line::raw(""));
                l.push(field_line(
                    "Goal (optional here: with it the coordinator plans without asking)",
                    *field == LaunchField::Goal,
                ));
                l.extend(text_lines(goal, *field == LaunchField::Goal, 6));
                l.push(pool_line(
                    v,
                    pool,
                    *pool_cursor,
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
                    "Plan approval",
                    if *gate {
                        "in the coordinator's chat".into()
                    } else {
                        "off: a plan that checks out runs at once".into()
                    },
                    *field == LaunchField::Gate,
                ));
                l.push(field_line(
                    "Check when done (optional command, e.g. cargo test)",
                    *field == LaunchField::Check,
                ));
                l.extend(text_lines(check, *field == LaunchField::Check, 1));
            }
            (
                " New swarm ",
                l,
                " Enter start · Tab field · ←→ choose · Space tick/unfold · Alt+Enter newline · Esc cancel ",
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
            note,
            ..
        } => {
            let mut l = Vec::new();
            if let Some(n) = note {
                let style = if n.starts_with('✗') {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                };
                l.push(Line::styled(n.clone(), style));
            }
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
        Modal::Picker { swarms, cursor } => (
            " Swarms ",
            picker_lines(v, swarms, *cursor),
            " ↑↓ choose · Enter show · Esc close ",
        ),
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

/// The swarm picker's rows, around the cursor: `●` the swarm shown, the
/// short id, the status, the goal and, outside this folder, where it runs.
fn picker_lines(v: &SwarmBoard, swarms: &[SwarmRun], cursor: usize) -> Vec<Line<'static>> {
    const SHOWN: usize = 16;
    let here = crate::bus::store::normalize_workspace(&v.workspace);
    let first = cursor
        .saturating_sub(SHOWN / 2)
        .min(swarms.len().saturating_sub(SHOWN));
    let mut out = Vec::new();
    for (i, s) in swarms.iter().enumerate().skip(first).take(SHOWN) {
        let shown = v.scope.as_deref() == Some(s.id.as_str());
        let ended = s.ended_ns.is_some();
        let state = if ended { "closed" } else { s.status.as_str() };
        let place = if s.workspace == here {
            String::new()
        } else {
            format!(
                "  {}",
                crate::app::workspaces_view::tilde_path(std::path::Path::new(&s.workspace))
            )
        };
        let text = format!(
            "{} {:<8} {:<11} {:>5}  {}{}",
            if shown { "●" } else { " " },
            crate::bus::short_id(&s.id),
            state,
            fmt_clock(s.started_ns),
            truncate(s.goal.lines().next().unwrap_or(""), 56),
            place
        );
        let mut style = if ended { dim() } else { Style::default() };
        if i == cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        out.push(Line::styled(text, style));
    }
    if swarms.len() > SHOWN {
        out.push(Line::styled(
            format!("  {} of {}", cursor + 1, swarms.len()),
            dim(),
        ));
    }
    out
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
