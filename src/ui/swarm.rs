//! Drawing Swarm Mission Control (`crate::app::swarm_view`): the mission
//! strip, the Needs-you strip, the lanes, the focus panel (Live, Thread,
//! Changes, Task) or the swarm's Activity, and the footer.

use super::flow::{dim, head, hints};
use crate::app::App;
use crate::app::swarm_view::{
    ComposeField, Confirm, Focus, HARNESS_CYCLE, LaunchField, MESSAGE_TOPICS, Modal, PanelTab,
    SwarmView, TaskField, WorkerField, format_task_detail_lines,
};
use crate::app::workspaces_view::tilde_path;
use crate::bus::swarm::{ActivityItem, Lane, LaneKind, LaneState, NeedKind, Phase};
use crate::bus::{TaskStatus, short_id};
use crate::tracing::cli::{fmt_cost, fmt_tokens};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

pub fn draw(f: &mut Frame, area: Rect, v: &SwarmView, app: &App) {
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(mission_strip(v, area.width.saturating_sub(4) as usize));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // The strip lists up to five needs while it has focus, three otherwise,
    // and one line when nothing needs the operator.
    let shown = v
        .needs
        .len()
        .min(if v.focus == Focus::Needs { 5 } else { 3 });
    let needs_h = if v.needs.is_empty() {
        1
    } else {
        1 + shown as u16
    };
    let [needs_area, body, foot] = Layout::vertical([
        Constraint::Length(needs_h.min(inner.height.saturating_sub(4))),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    draw_needs(f, needs_area, v, shown);
    let lanes_w = (body.width * 44 / 100).clamp(36.min(body.width), 64);
    let [lanes_area, panel_area] =
        Layout::horizontal([Constraint::Length(lanes_w), Constraint::Min(0)]).areas(body);

    draw_lanes(f, lanes_area, v);
    if v.activity {
        draw_activity(f, panel_area, v);
    } else if let Some(lane) = v.selected() {
        draw_panel(f, panel_area, v, lane, app);
    } else {
        draw_launch_card(f, panel_area);
    }
    draw_footer(f, foot, v);

    if let Some(modal) = &v.modal {
        draw_modal(f, area, modal, v);
    }
    if let Some(confirm) = &v.confirm {
        draw_confirm(f, area, confirm);
    }
}

/// The top border: goal, phase, progress, workers against the cap, elapsed,
/// tokens and cost. The goal gives way first when the line is too long.
fn mission_strip(v: &SwarmView, width: usize) -> Line<'static> {
    let snap = &v.snapshot;
    let phase = snap.phase();
    let total = snap.tasks.len();
    let done = snap
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    let failed = snap
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();
    let mut facts: Vec<Span<'static>> = vec![Span::styled(
        phase.label().to_string(),
        Style::default()
            .fg(match phase {
                Phase::Executing => Color::Green,
                Phase::Planning | Phase::WrappingUp => Color::Yellow,
                Phase::Done => Color::Cyan,
                Phase::NotStarted => Color::Gray,
            })
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(filled) = (done * 10).checked_div(total) {
        let cells = 10usize;
        facts.push(Span::styled(format!(" · {done}/{total} done "), dim()));
        facts.push(Span::styled(
            "█".repeat(filled),
            Style::default().fg(Color::Green),
        ));
        facts.push(Span::styled("░".repeat(cells - filled), dim()));
        if failed > 0 {
            facts.push(Span::styled(
                format!(" {failed} failed"),
                Style::default().fg(Color::Red),
            ));
        }
    }
    let workers = snap.workers.iter().filter(|w| w.is_active()).count();
    let mut tail = match snap.max_workers {
        Some(cap) => format!(" · {workers}/{cap} workers"),
        None => format!(" · {workers} worker{}", if workers == 1 { "" } else { "s" }),
    };
    if let Some(start) = snap.started_ns {
        tail.push_str(&format!(
            " · {}",
            crate::bus::swarm::short_age(v.now_ns - start)
        ));
    }
    let len = |spans: &[Span<'static>]| -> usize {
        spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum()
    };
    // Spend is the first fact to go when the line is short: the goal keeps
    // at least 16 cells.
    if snap.total_tokens > 0 {
        let spend = format!(
            " · {} tok {}",
            fmt_tokens(Some(snap.total_tokens)),
            fmt_cost(Some(snap.total_cost_usd))
        );
        if len(&facts) + tail.len() + spend.len() + 16 + 8 <= width {
            tail.push_str(&spend);
        }
    }
    facts.push(Span::styled(tail, dim()));

    let facts_len = len(&facts);
    let goal = snap
        .goal
        .clone()
        .unwrap_or_else(|| format!("Swarm · {}", tilde_path(&v.workspace)));
    let room = width.saturating_sub(facts_len + 8).max(8);
    let mut spans = vec![
        Span::styled(" ◆ ", Style::default().fg(Color::Cyan)),
        Span::styled(
            truncate_str(goal.lines().next().unwrap_or(""), room),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", dim()),
    ];
    spans.extend(facts);
    spans.push(Span::raw(" "));
    Line::from(spans)
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

fn state_style(state: LaneState) -> Style {
    match state {
        LaneState::Failed | LaneState::Lost => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        LaneState::Attention | LaneState::Waiting => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        LaneState::Working => Style::default().fg(Color::Green),
        LaneState::Done => Style::default().fg(Color::Green),
        LaneState::Ready => Style::default().fg(Color::Cyan),
        LaneState::Idle | LaneState::Queued => Style::default().fg(Color::Gray),
        LaneState::Exited => Style::default().fg(Color::DarkGray),
    }
}

/// One color per agent, the same wherever its name appears.
fn agent_color(key: &str) -> Color {
    const PALETTE: [Color; 6] = [
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightRed,
    ];
    let h = key
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[(h as usize) % PALETTE.len()]
}

fn draw_lanes(f: &mut Frame, area: Rect, v: &SwarmView) {
    let focused = v.focus == Focus::Lanes;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            format!(" Lanes ({}) ", v.lanes.len()),
            if focused { head() } else { dim() },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.lanes.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled("  No agents in this workspace's swarm.", dim()),
            ]),
            inner,
        );
        return;
    }

    let width = inner.width as usize;
    let per = 2usize;
    let room = (inner.height as usize / per).max(1);
    let start = super::sidebar_window(v.selected_lane, v.lanes.len(), room);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, lane) in v.lanes.iter().enumerate().skip(start).take(room) {
        let selected = i == v.selected_lane;
        let bg = if selected {
            Style::default().bg(if focused {
                Color::Rgb(24, 38, 52)
            } else {
                Color::Rgb(30, 30, 30)
            })
        } else {
            Style::default()
        };
        let marker = if selected { "▸" } else { " " };
        let name = match lane.kind {
            LaneKind::Coordinator => format!("★ {}", lane.name),
            _ => lane.name.clone(),
        };
        let name_style = match lane.kind {
            LaneKind::Queued => dim(),
            _ => Style::default()
                .fg(agent_color(&lane.key))
                .add_modifier(Modifier::BOLD),
        };
        let used = 4 + unicode_width::UnicodeWidthStr::width(name.as_str()) + 2;
        let activity = truncate_str(&lane.activity, width.saturating_sub(used).max(6));
        lines.push(
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{} ", lane.state.glyph()), state_style(lane.state)),
                Span::styled(name, name_style),
                Span::raw("  "),
                Span::styled(
                    activity,
                    state_style(lane.state).remove_modifier(Modifier::BOLD),
                ),
            ])
            .style(bg),
        );
        let second = match (lane.kind, &lane.task) {
            (LaneKind::Coordinator, _) => lane.placement.clone(),
            (_, Some(t)) if lane.placement.is_empty() => t.title.clone(),
            (_, Some(t)) => format!("{} · {}", t.title, lane.placement),
            (_, None) => String::new(),
        };
        lines.push(
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    truncate_str(&second, width.saturating_sub(5).max(6)),
                    Style::default().fg(Color::Gray),
                ),
            ])
            .style(bg),
        );
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_panel(f: &mut Frame, area: Rect, v: &SwarmView, lane: &Lane, app: &App) {
    let focused = v.focus == Focus::Panel;
    let title = match &lane.task {
        Some(t) if lane.kind != LaneKind::Queued => format!(" {} · {} ", lane.name, t.title),
        Some(t) => format!(" queued · {} ", t.title),
        None => format!(" {} ", lane.name),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            truncate_str(&title, area.width.saturating_sub(4) as usize),
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [tabs_area, content] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let current = v.tab();
    let thread_len = v.panel_messages().len();
    // Narrow panels drop the padding and the thread count, never a tab.
    let compact = inner.width < 52;
    let pad = if compact { "" } else { " " };
    let mut tabs: Vec<Span<'static>> = vec![Span::raw(pad)];
    for (n, tab) in PanelTab::ALL.iter().enumerate() {
        let label = match tab {
            PanelTab::Thread if thread_len > 0 && !compact => {
                format!("{} {} ({thread_len})", n + 1, tab.label())
            }
            _ => format!("{} {}", n + 1, tab.label()),
        };
        tabs.push(if *tab == current {
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
        tabs.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(tabs)), tabs_area);

    match current {
        PanelTab::Live => match lane.session_idx.and_then(|i| app.sessions.get(i)) {
            Some(session) => {
                f.render_widget(PseudoTerminal::new(session.parser.screen()), content);
            }
            None => f.render_widget(
                Paragraph::new(vec![
                    Line::raw(""),
                    Line::styled(
                        match lane.kind {
                            LaneKind::Queued => "No agent holds this task yet: r delegates it.",
                            _ => "No terminal here: this agent runs outside this window.",
                        },
                        dim(),
                    ),
                    Line::styled("2 Thread shows its messages, 4 Task its task.", dim()),
                ])
                .wrap(Wrap { trim: true }),
                indented(content),
            ),
        },
        PanelTab::Thread => {
            let msgs = v.panel_messages();
            if msgs.is_empty() {
                let hint = if lane.launch_id.is_some() {
                    "  No messages yet. m writes to this lane."
                } else {
                    "  A queued task has no conversation yet."
                };
                f.render_widget(
                    Paragraph::new(vec![Line::raw(""), Line::styled(hint, dim())]),
                    content,
                );
            } else {
                let items: Vec<ActivityItem<'_>> =
                    msgs.into_iter().map(ActivityItem::Message).collect();
                draw_messages(f, content, v, &items, focused);
            }
        }
        PanelTab::Changes => {
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
        PanelTab::Task => {
            let Some(task) = &lane.task else {
                f.render_widget(
                    Paragraph::new(vec![
                        Line::raw(""),
                        Line::styled("  This lane holds no task.", dim()),
                    ]),
                    content,
                );
                return;
            };
            let mut raw = format_task_detail_lines(task, &v.snapshot);
            if !lane.leases.is_empty() {
                raw.push(String::new());
                raw.push("── Leases held ──".into());
                raw.extend(lane.leases.iter().map(|l| format!("{l}   (d releases)")));
            }
            let lines: Vec<Line<'static>> = raw
                .into_iter()
                .skip(v.panel_scroll)
                .map(|l| {
                    if l.starts_with("Task [") || l.starts_with("── ") {
                        Line::styled(
                            l,
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Line::raw(l)
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), content);
        }
    }
}

fn draw_activity(f: &mut Frame, area: Rect, v: &SwarmView) {
    let focused = v.focus == Focus::Panel;
    let items = v.activity_items();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            format!(
                " Activity · whole swarm ({}) · a back to the lane ",
                items.len()
            ),
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled("  Nothing has happened yet. b writes to everyone.", dim()),
            ]),
            inner,
        );
        return;
    }
    draw_messages(f, inner, v, &items, focused);
}

/// The Needs-you strip: a header and up to `shown` needs, each with its kind,
/// what it is about and how long ago; one quiet line when there are none.
fn draw_needs(f: &mut Frame, area: Rect, v: &SwarmView, shown: usize) {
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
                "  Enter act · r retry · d dismiss · Esc back"
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
            NeedKind::Failed | NeedKind::Lost => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
            NeedKind::Attention | NeedKind::Question | NeedKind::Stuck => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            NeedKind::CapReached | NeedKind::Review => Style::default().fg(Color::Cyan),
        };
        let age = format!(
            "  {}",
            crate::bus::swarm::short_age(v.now_ns - need.since_ns)
        );
        let label = format!("{:<10}", need.kind.label());
        let room = width.saturating_sub(4 + label.len() + age.len());
        lines.push(
            Line::from(vec![
                Span::styled(
                    if selected { " ▸ " } else { "   " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(label, kind_style),
                Span::styled(
                    truncate_str(&need.text, room),
                    Style::default().fg(Color::White),
                ),
                Span::styled(age, dim()),
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

/// Messages as a conversation: a header line (time, sender → recipient,
/// topic) and the body, wrapped, up to six lines; an event is one line (time,
/// who, what). The chosen line is kept in view and marked.
fn draw_messages(
    f: &mut Frame,
    area: Rect,
    v: &SwarmView,
    items: &[ActivityItem<'_>],
    focused: bool,
) {
    let width = (area.width as usize).saturating_sub(4).max(16);
    let open: Vec<&str> = v
        .snapshot
        .open_questions()
        .into_iter()
        .map(|q| q.id.as_str())
        .collect();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sel_range = (0usize, 0usize);
    for (i, item) in items.iter().enumerate() {
        let selected = i == v.selected_message;
        let first = lines.len();
        let marker = Span::styled(
            if selected && focused { "▸ " } else { "  " },
            Style::default().fg(Color::Cyan),
        );
        let m = match item {
            ActivityItem::Message(m) => *m,
            ActivityItem::Event {
                at_ns, who, text, ..
            } => {
                let who_label = who
                    .as_deref()
                    .map(|w| v.snapshot.member_label(w))
                    .unwrap_or_default();
                let room = (area.width as usize)
                    .saturating_sub(13 + unicode_width::UnicodeWidthStr::width(who_label.as_str()));
                let text = truncate_str(text, room.max(8));
                lines.push(Line::from(vec![
                    marker,
                    Span::styled(format!("{} ", fmt_clock(*at_ns)), dim()),
                    Span::styled(
                        who_label,
                        Style::default().fg(who.as_deref().map_or(Color::Gray, agent_color)),
                    ),
                    Span::styled(format!(" {text}"), dim()),
                ]));
                if selected {
                    sel_range = (first, lines.len());
                }
                continue;
            }
        };
        let from = v.snapshot.member_label(&m.from_launch);
        let to = m
            .to_launch
            .as_deref()
            .map(|t| v.snapshot.member_label(t))
            .unwrap_or_else(|| "everyone".into());
        let head = vec![
            marker,
            Span::styled(format!("{} ", fmt_clock(m.created_ns)), dim()),
            Span::styled(
                from,
                Style::default()
                    .fg(agent_color(&m.from_launch))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" → ", dim()),
            Span::styled(to, Style::default().fg(Color::White)),
            Span::raw("  "),
            if open.contains(&m.id.as_str()) {
                Span::styled(
                    "open question",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(m.topic.clone(), topic_style(&m.topic))
            },
        ];
        lines.push(Line::from(head));
        let body: Vec<String> = m.body.lines().flat_map(|l| wrap(l, width)).collect();
        let shown = body.len().min(6);
        for l in body.iter().take(shown) {
            lines.push(Line::from(vec![Span::raw("    "), Span::raw(l.clone())]));
        }
        if body.len() > shown {
            lines.push(Line::styled("    … Enter reads it all", dim()));
        }
        lines.push(Line::raw(""));
        if selected {
            sel_range = (first, lines.len());
        }
    }
    let height = area.height as usize;
    let start = sel_range.1.saturating_sub(height).min(sel_range.0);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(start).take(height).collect();
    f.render_widget(Paragraph::new(visible), area);
}

fn wrap(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split(' ') {
        let need = cur.chars().count() + usize::from(!cur.is_empty()) + word.chars().count();
        if need > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
        while cur.chars().count() > width {
            let head: String = cur.chars().take(width).collect();
            cur = cur.chars().skip(width).collect();
            out.push(head);
        }
    }
    out.push(cur);
    out
}

fn draw_launch_card(f: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let key = |k: &'static str| {
        Span::styled(
            k,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let lines = vec![
        Line::raw(""),
        Line::styled(
            "No swarm in this workspace yet.",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            "A swarm is a coordinator that splits a goal into tasks and hands each \
             to a worker agent, in parallel.",
            dim(),
        ),
        Line::raw(""),
        Line::from(vec![
            key("r"),
            Span::raw(
                " start one: the goal, the coordinator's harness, how many workers \
                 at most, shared files or worktrees.",
            ),
        ]),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        indented(inner),
    );
}

/// `r` less two cells on each side, so wrapped text keeps its margin.
fn indented(r: Rect) -> Rect {
    Rect {
        x: r.x + 2.min(r.width),
        width: r.width.saturating_sub(4),
        ..r
    }
}

fn topic_style(topic: &str) -> Style {
    match topic {
        "question" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "answer" => Style::default().fg(Color::Green),
        "blocker" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "api-contract" => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        "handoff" => Style::default().fg(Color::Cyan),
        _ => Style::default().fg(Color::Blue),
    }
}

fn draw_footer(f: &mut Frame, area: Rect, v: &SwarmView) {
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
    let lists_messages = v.activity || v.tab() == PanelTab::Thread;
    let queued = v.selected().is_some_and(|l| l.kind == LaneKind::Queued);
    let pairs: &[(&str, &str)] = if v.focus == Focus::Needs {
        &[
            ("↑↓", "need"),
            ("Enter", "act"),
            ("r", "retry"),
            ("d", "dismiss"),
            ("1-4", "view"),
            ("Tab/Esc", "lanes"),
        ]
    } else if v.lanes.is_empty() {
        &[("r", "start a swarm"), ("Esc", "close")]
    } else if v.focus == Focus::Panel && lists_messages {
        &[
            ("↑↓", "message"),
            ("Enter", "read"),
            ("m", "reply"),
            ("1-4", "view"),
            ("a", "activity"),
            ("Tab/Esc", "lanes"),
        ]
    } else if v.focus == Focus::Panel {
        &[
            ("↑↓", "scroll"),
            ("1-4", "view"),
            ("a", "activity"),
            ("Tab/Esc", "lanes"),
        ]
    } else if queued {
        &[
            ("↑↓", "lane"),
            ("r", "delegate"),
            ("e", "edit"),
            ("x", "cancel"),
            ("Tab", "panel"),
            ("1-4", "view"),
            ("n", "worker"),
            ("a", "activity"),
            ("Esc", "close"),
        ]
    } else {
        &[
            ("↑↓", "lane"),
            ("Tab", "panel"),
            ("1-4", "view"),
            ("Enter", "attach"),
            ("m", "message"),
            ("b", "all"),
            ("n", "worker"),
            ("x", "stop"),
            ("a", "activity"),
            ("Esc", "close"),
        ]
    };
    f.render_widget(Paragraph::new(hints(pairs)), area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(20);
    let h = height.min(area.height.saturating_sub(2)).max(6);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn draw_modal(f: &mut Frame, area: Rect, modal: &Modal, v: &SwarmView) {
    let popup = centered_rect(74, 20, area);
    f.render_widget(Clear, popup);

    match modal {
        Modal::LaunchSwarm {
            goal,
            harness_idx,
            max_workers,
            isolation,
            prewarm_workers,
            field,
        } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .title(" Launch Agent Swarm (Coordinator + Workers) ");
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let harness = HARNESS_CYCLE[*harness_idx % HARNESS_CYCLE.len()];
            let mut lines = Vec::new();
            lines.push(field_header(
                "Mission Goal (multi-line):",
                *field == LaunchField::Goal,
            ));
            for l in goal.text.lines().take(4) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            if goal.text.is_empty() {
                lines.push(Line::styled(
                    "  (Describe what the Coordinator and Workers should build or verify…)",
                    dim(),
                ));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                field_span("Coordinator Harness: ", *field == LaunchField::Harness),
                Span::styled(
                    format!("◀ {} ▶", harness.as_str()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                field_span("Max Parallel Workers: ", *field == LaunchField::MaxWorkers),
                Span::styled(
                    format!("◀ {max_workers} ▶"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                field_span("Default Task Isolation: ", *field == LaunchField::Isolation),
                Span::styled(
                    format!("◀ {} ▶", isolation.as_str()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                field_span(
                    "Pre-warm Standby Workers: ",
                    *field == LaunchField::PrewarmWorkers,
                ),
                Span::styled(
                    format!("◀ {prewarm_workers} ▶"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("Tab", "next field"),
                ("←/→", "change option"),
                ("Ctrl+S", "launch swarm"),
                ("Esc", "cancel"),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }

        Modal::NewWorker {
            title,
            instructions,
            files,
            harness_idx,
            agent,
            isolation,
            field,
        } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .title(" Spawn & Delegate Swarm Worker ");
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let harness = HARNESS_CYCLE[*harness_idx % HARNESS_CYCLE.len()];
            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                field_span("Task Title: ", *field == WorkerField::Title),
                Span::styled(title.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(field_header(
                "Instructions:",
                *field == WorkerField::Instructions,
            ));
            for l in instructions.text.lines().take(3) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::from(vec![
                field_span("Files (comma globs): ", *field == WorkerField::Files),
                Span::styled(files.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                field_span("Worker Harness: ", *field == WorkerField::Harness),
                Span::styled(
                    format!("◀ {} ▶", harness.as_str()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                field_span("Persona (optional): ", *field == WorkerField::Agent),
                Span::styled(agent.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                field_span("Isolation: ", *field == WorkerField::Isolation),
                Span::styled(
                    format!("◀ {} ▶", isolation.as_str()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("Tab", "next field"),
                ("Ctrl+S", "spawn worker"),
                ("Esc", "cancel"),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }

        Modal::EditTask {
            task_id,
            title,
            instructions,
            files,
            field,
        } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .title(format!(" Edit Task [{}] ", short_id(task_id)));
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                field_span("Task Title: ", *field == TaskField::Title),
                Span::styled(title.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(field_header(
                "Instructions:",
                *field == TaskField::Instructions,
            ));
            for l in instructions.text.lines().take(5) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::from(vec![
                field_span("File Globs (comma): ", *field == TaskField::Files),
                Span::styled(files.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("Tab", "next field"),
                ("Ctrl+S", "save task"),
                ("Esc", "cancel"),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }

        Modal::ComposeMessage {
            to_label,
            topic_idx,
            body,
            urgent,
            wake,
            field,
            ..
        } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .title(format!(" Send Bus Message → {to_label} "));
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let topic = MESSAGE_TOPICS[*topic_idx % MESSAGE_TOPICS.len()];
            let on_off = |on: bool| if on { "[x]" } else { "[ ]" };
            let mut lines = Vec::new();
            lines.push(field_header("Message Body:", *field == ComposeField::Body));
            for l in body.text.lines().take(6) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            if body.text.is_empty() {
                lines.push(Line::styled("  (Enter adds a line)", dim()));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                field_span("Topic: ", *field == ComposeField::Topic),
                Span::styled(format!("◀ {topic} ▶"), topic_style(topic)),
            ]));
            lines.push(Line::from(vec![
                field_span(
                    "Urgent (injected mid-turn): ",
                    *field == ComposeField::Urgent,
                ),
                Span::styled(
                    on_off(*urgent),
                    if *urgent {
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                field_span("Wake idle session: ", *field == ComposeField::Wake),
                Span::styled(
                    on_off(*wake),
                    if *wake {
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                ),
            ]));
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("Tab", "field"),
                ("←/→/Space", "change"),
                ("Ctrl+Enter", "send"),
                ("Esc", "cancel"),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }

        Modal::ViewMessage { message, scroll } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .title(format!(" Message {} ", short_id(&message.id)));
            let inner = block.inner(popup);
            f.render_widget(block, popup);

            let to = message
                .to_launch
                .as_deref()
                .map(|r| v.snapshot.member_label(r))
                .unwrap_or_else(|| "* (all)".into());
            let delivery = if message.acked_ns.is_some() {
                "✓ read"
            } else if message.delivered_ns.is_some() {
                "⚡ delivered"
            } else {
                "⏳ unread"
            };
            let header = vec![
                Line::from(vec![
                    Span::styled(format!(" {} ", fmt_clock(message.created_ns)), dim()),
                    Span::styled(
                        v.snapshot.member_label(&message.from_launch),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(" → ", dim()),
                    Span::styled(to, Style::default().fg(Color::White)),
                    Span::raw("  "),
                    Span::styled(format!("[{}]", message.topic), topic_style(&message.topic)),
                    Span::raw("  "),
                    Span::styled(delivery, dim()),
                ]),
                Line::raw(""),
            ];
            let body_rows = (inner.height as usize).saturating_sub(header.len() + 2);
            let body_lines: Vec<Line<'static>> = message
                .body
                .lines()
                .skip(*scroll)
                .take(body_rows.max(1))
                .map(|l| Line::from(format!("  {l}")))
                .collect();
            let mut lines = header;
            lines.extend(body_lines);
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("j/k", "scroll"),
                ("m", "reply"),
                ("Esc", "close"),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

fn draw_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) {
    let popup = centered_rect(60, 7, area);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .title(" Confirm Action ");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let prompt = match confirm {
        Confirm::RelaunchSwarm { coordinator } => {
            format!(
                "A swarm is running under {coordinator}. Launch a new one and replace it as coordinator?"
            )
        }
        Confirm::ReleaseLeases { lane, patterns } => {
            format!("Release {lane}'s leases on {}?", patterns.join(", "))
        }
        Confirm::CancelTask { task_id, title } => {
            format!("Cancel task [{}] \"{title}\"?", short_id(task_id))
        }
        Confirm::StopSession { label, .. } => {
            format!("Stop {label}'s session? Its task stays on the board.")
        }
    };

    let lines = vec![
        Line::raw(""),
        Line::styled(format!("  {prompt}"), Style::default().fg(Color::White)),
        Line::raw(""),
        hints(&[("y", "confirm"), ("n / Esc", "cancel")]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn field_header(label: &str, active: bool) -> Line<'static> {
    Line::from(field_span(label, active))
}

fn field_span(label: &str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            format!("▸ {label}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!("  {label}"), dim())
    }
}

/// A message's time of day in the machine's zone (`HH:MM:SS`): a live feed
/// has no room for the date, and the nanosecond timestamp is the store's.
fn fmt_clock(ns: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns)) {
        Ok(t) => {
            let t = time::UtcOffset::local_offset_at(t)
                .map(|offset| t.to_offset(offset))
                .unwrap_or(t);
            format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
        }
        Err(_) => "--:--:--".into(),
    }
}

/// Cuts `s` to `max` terminal cells (wide glyphs count twice), ending in `…`.
fn truncate_str(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}
