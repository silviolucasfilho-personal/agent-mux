//! Drawing the Agent Swarm Control Room (`crate::app::swarm_view`).

use super::flow::{dim, head, hints};
use crate::app::App;
use crate::app::swarm_view::{
    BoardMode, Confirm, HARNESS_CYCLE, LaunchField, MESSAGE_TOPICS, Modal, PreviewMode, SwarmPane,
    SwarmView, TaskField, WorkerField, format_task_detail_lines,
};
use crate::app::workspaces_view::tilde_path;
use crate::bus::{TaskStatus, short_id};
use crate::tracing::cli::{fmt_cost, fmt_time, fmt_tokens};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

pub fn draw(f: &mut Frame, area: Rect, v: &SwarmView, app: &App) {
    f.render_widget(Clear, area);

    let (running_tasks, open_tasks, done_tasks, failed_tasks) = v.snapshot.task_counts();
    let total_tasks = v.snapshot.tasks.len();
    let coord_tag = match &v.snapshot.coordinator {
        Some(c) => format!("★ Coord: {} ({})", c.provider, c.state),
        None => "★ Coord: none (press r to launch)".to_string(),
    };
    let workers = match v.snapshot.max_workers {
        Some(cap) => format!("{}/{cap} Workers", v.snapshot.workers.len()),
        None if v.snapshot.workers.len() == 1 => "1 Worker".to_string(),
        None => format!("{} Workers", v.snapshot.workers.len()),
    };
    let title = format!(
        " Swarm · {} · {} · {} · Tasks {}/{} done ({} ●, {} ○{}) · {} Lease{} · {} tok ({}) ",
        tilde_path(&v.workspace),
        coord_tag,
        workers,
        done_tasks,
        total_tasks,
        running_tasks,
        open_tasks,
        if failed_tasks > 0 {
            format!(", {failed_tasks} ✗")
        } else {
            String::new()
        },
        v.snapshot.leases.len(),
        if v.snapshot.leases.len() == 1 {
            ""
        } else {
            "s"
        },
        fmt_tokens(Some(v.snapshot.total_tokens)),
        fmt_cost(Some(v.snapshot.total_cost_usd)),
    );

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [body, foot] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let top_h = (body.height * 54 / 100)
        .max(8)
        .min(body.height.saturating_sub(6));
    let [top_row, bot_row] =
        Layout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).areas(body);

    let left_w = (body.width * 42 / 100).clamp(32, 62);
    let [q1_area, q2_area] =
        Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(top_row);
    let [q3_area, q4_area] =
        Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(bot_row);

    draw_agents_pane(f, q1_area, v);
    draw_preview_pane(f, q2_area, v, app);
    draw_board_pane(f, q3_area, v);
    draw_messages_pane(f, q4_area, v);
    draw_footer_bar(f, foot, v);

    if let Some(modal) = &v.modal {
        draw_modal(f, area, modal);
    }
    if let Some(confirm) = &v.confirm {
        draw_confirm(f, area, confirm);
    }
}

fn pane_border_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn state_badge(state: &str) -> (&'static str, Style) {
    match state {
        "working" | "running" => ("● working", Style::default().fg(Color::Green)),
        "attention" => (
            "◆ attention",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        "idle" => ("○ idle", Style::default().fg(Color::Gray)),
        "exited" | "disconnected" => ("◌ exited", Style::default().fg(Color::DarkGray)),
        _ => ("• active", Style::default().fg(Color::Cyan)),
    }
}

fn draw_agents_pane(f: &mut Frame, area: Rect, v: &SwarmView) {
    let focused = v.pane == SwarmPane::Agents;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border_style(focused))
        .title(Span::styled(
            " [1] Swarm Agents (Coordinator & Workers) ",
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(goal) = &v.snapshot.goal {
        let max_g = (inner.width as usize).saturating_sub(8).max(12);
        lines.push(Line::from(vec![
            Span::styled(" Goal: ", Style::default().fg(Color::Yellow)),
            Span::styled(truncate_str(goal, max_g), Style::default().fg(Color::White)),
        ]));
    }

    let members = v.snapshot.all_members();
    if members.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "  No swarm sessions active in this workspace.",
            dim(),
        ));
        lines.push(Line::from(vec![
            Span::raw("  Press "),
            Span::styled(
                "r",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to launch a Coordinator + Swarm, or ", dim()),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to spawn a Worker.", dim()),
        ]));
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let header_lines = lines.len();
    let avail_rows = (inner.height as usize).saturating_sub(header_lines).max(1);
    let rows_per_item = 2usize;
    let visible_items = (avail_rows / rows_per_item).max(1);
    let start = super::sidebar_window(v.selected_agent, members.len(), visible_items);

    for (idx, m) in members.iter().enumerate().skip(start).take(visible_items) {
        let is_sel = idx == v.selected_agent;
        let marker = if is_sel { "▸ " } else { "  " };
        let (st_label, st_style) = state_badge(&m.state);
        let role_style = match m.role {
            crate::bus::swarm::SwarmRole::Coordinator => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            crate::bus::swarm::SwarmRole::Worker => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        };
        let row_bg = if is_sel && focused {
            Style::default().bg(Color::Rgb(24, 38, 52))
        } else {
            Style::default()
        };

        let persona_tag = m
            .agent_persona
            .as_deref()
            .map(|p| format!(" · {p}"))
            .unwrap_or_default();
        let unread_tag = if m.unread_messages > 0 {
            format!(" ✉ {}", m.unread_messages)
        } else {
            String::new()
        };

        lines.push(
            Line::from(vec![
                Span::styled(
                    marker,
                    if is_sel {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                ),
                Span::styled(format!("{:<7}", m.label), role_style),
                Span::raw(" "),
                Span::styled(
                    format!("{}{}", m.provider, persona_tag),
                    Style::default().fg(Color::White),
                ),
                Span::raw("  "),
                Span::styled(st_label, st_style),
                Span::styled(
                    unread_tag,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{}t · {}", m.turns, fmt_tokens(Some(m.tokens))),
                    dim(),
                ),
            ])
            .style(row_bg),
        );

        // Second line: active task / tool / leased files / worktree
        let detail = if let Some(tool) = &m.running_tool {
            format!("    ⚙ {tool}")
        } else if let Some(task) = m.active_tasks.first() {
            format!("    ↳ {task}")
        } else if !m.leased_files.is_empty() {
            format!("    🔒 {}", m.leased_files.join(", "))
        } else if let Some(br) = &m.branch {
            format!("    ⎇ {br}")
        } else {
            format!("    id: {}", short_id(&m.launch_id))
        };
        let max_w = (inner.width as usize).saturating_sub(2).max(10);
        lines.push(Line::from(Span::styled(truncate_str(&detail, max_w), dim())).style(row_bg));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_preview_pane(f: &mut Frame, area: Rect, v: &SwarmView, app: &App) {
    let focused = v.pane == SwarmPane::Preview;
    let member_tag = v
        .selected_member()
        .map(|m| format!("{} ({})", m.label, m.provider))
        .unwrap_or_else(|| "no agent".into());
    let title = format!(
        " [2] Preview · {} · mode: {} (v) ",
        member_tag,
        v.preview_mode.label()
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border_style(focused))
        .title(Span::styled(
            title,
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    match v.preview_mode {
        PreviewMode::Terminal => {
            if let Some(m) = v.selected_member()
                && let Some(idx) = m.session_idx
                && let Some(session) = app.sessions.get(idx)
            {
                let term = PseudoTerminal::new(session.parser.screen());
                f.render_widget(term, inner);
                return;
            }
            // Fallback when peer is external or no session exists
            let mut lines = Vec::new();
            lines.push(Line::raw(""));
            if let Some(m) = v.selected_member() {
                lines.push(Line::styled(
                    format!(
                        "  Peer {} ({}) is connected via traces.db (external process).",
                        m.label,
                        short_id(&m.launch_id)
                    ),
                    dim(),
                ));
                lines.push(Line::styled(
                    "  Press v to view its Task Detail or Git Diff.",
                    dim(),
                ));
            } else {
                lines.push(Line::styled(
                    "  No active agent selected. Press r to start a Swarm.",
                    dim(),
                ));
            }
            f.render_widget(Paragraph::new(lines), inner);
        }
        PreviewMode::TaskDetail => {
            let task = v.selected_task().or_else(|| {
                let lid = v.selected_member()?.launch_id.as_str();
                v.snapshot
                    .tasks
                    .iter()
                    .find(|t| t.assigned_to.as_deref() == Some(lid))
            });
            let Some(task) = task else {
                f.render_widget(
                    Paragraph::new(vec![
                        Line::raw(""),
                        Line::styled("  No task selected on the Task Board.", dim()),
                    ]),
                    inner,
                );
                return;
            };
            let raw_lines = format_task_detail_lines(task, &v.snapshot);
            let lines: Vec<Line<'static>> = raw_lines
                .into_iter()
                .skip(v.preview_scroll)
                .take(inner.height as usize)
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
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        PreviewMode::GitDiff => {
            let lines: Vec<Line<'static>> = v
                .diff_lines
                .iter()
                .skip(v.preview_scroll)
                .take(inner.height as usize)
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
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

fn task_status_span(status: TaskStatus) -> Span<'static> {
    match status {
        TaskStatus::Open => Span::styled("○ open   ", Style::default().fg(Color::Gray)),
        TaskStatus::Claimed => Span::styled("◐ claimed", Style::default().fg(Color::Cyan)),
        TaskStatus::Running => Span::styled(
            "● running",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Done => Span::styled("✓ done   ", Style::default().fg(Color::Green)),
        TaskStatus::Failed => Span::styled(
            "✗ failed ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Cancelled => Span::styled("⊘ cancel ", Style::default().fg(Color::DarkGray)),
    }
}

fn draw_board_pane(f: &mut Frame, area: Rect, v: &SwarmView) {
    let focused = v.pane == SwarmPane::Board;
    let tab_label = match v.board_mode {
        BoardMode::Tasks => format!(
            " [3] Task Board ({}) · File Leases ({}) (l) ",
            v.snapshot.tasks.len(),
            v.snapshot.leases.len()
        ),
        BoardMode::Leases => format!(
            " [3] File Leases ({}) · Task Board ({}) (l) ",
            v.snapshot.leases.len(),
            v.snapshot.tasks.len()
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border_style(focused))
        .title(Span::styled(
            tab_label,
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let room = (inner.height as usize).max(1);
    let max_w = (inner.width as usize).saturating_sub(2).max(12);

    match v.board_mode {
        BoardMode::Tasks => {
            if v.snapshot.tasks.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled("  No tasks on the shared board yet.", dim()));
                lines.push(Line::styled(
                    "  Press n to add a task, or r to launch a Coordinator.",
                    dim(),
                ));
                f.render_widget(Paragraph::new(lines), inner);
                return;
            }
            let start = super::sidebar_window(v.selected_task, v.snapshot.tasks.len(), room);
            for (idx, t) in v.snapshot.tasks.iter().enumerate().skip(start).take(room) {
                let is_sel = idx == v.selected_task;
                let marker = if is_sel { "▸ " } else { "  " };
                let owner = t
                    .assigned_to
                    .as_deref()
                    .map(|a| v.snapshot.member_label(a))
                    .unwrap_or_else(|| "unclaimed".into());
                let iso_badge = match t.isolation {
                    crate::bus::TaskIsolation::Worktree => " [wt]",
                    crate::bus::TaskIsolation::Shared => "",
                };
                let files_hint = if t.files.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", t.files.join(","))
                };
                let title_budget = max_w.saturating_sub(30);
                let row_bg = if is_sel && focused {
                    Style::default().bg(Color::Rgb(24, 38, 52))
                } else {
                    Style::default()
                };

                lines.push(
                    Line::from(vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                dim()
                            },
                        ),
                        task_status_span(t.status),
                        Span::raw(" "),
                        Span::styled(
                            format!("[{}] ", short_id(&t.id)),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            truncate_str(
                                &format!("{}{iso_badge}{files_hint}", t.title),
                                title_budget,
                            ),
                            Style::default().fg(Color::White),
                        ),
                        Span::raw(" → "),
                        Span::styled(owner, Style::default().fg(Color::Cyan)),
                    ])
                    .style(row_bg),
                );
            }
        }
        BoardMode::Leases => {
            if v.snapshot.leases.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled("  No active cooperative file leases.", dim()));
                f.render_widget(Paragraph::new(lines), inner);
                return;
            }
            let start = super::sidebar_window(v.selected_lease, v.snapshot.leases.len(), room);
            for (idx, l) in v.snapshot.leases.iter().enumerate().skip(start).take(room) {
                let is_sel = idx == v.selected_lease;
                let marker = if is_sel { "▸ " } else { "  " };
                let owner = v.snapshot.member_label(&l.owner_launch);
                let task_tag = l
                    .task_id
                    .as_deref()
                    .map(|tid| format!(" task:{}", short_id(tid)))
                    .unwrap_or_default();
                let row_bg = if is_sel && focused {
                    Style::default().bg(Color::Rgb(24, 38, 52))
                } else {
                    Style::default()
                };

                lines.push(
                    Line::from(vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                dim()
                            },
                        ),
                        Span::styled("🔒 ", Style::default().fg(Color::Yellow)),
                        Span::styled(
                            l.pattern.clone(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  ← "),
                        Span::styled(owner, Style::default().fg(Color::Cyan)),
                        Span::styled(task_tag, dim()),
                    ])
                    .style(row_bg),
                );
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
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

fn draw_messages_pane(f: &mut Frame, area: Rect, v: &SwarmView) {
    let focused = v.pane == SwarmPane::Messages;
    let filtered = v.filtered_messages();
    let title = format!(
        " [4] Inter-Agent Message Feed ({}) · filter: {} (f) ",
        filtered.len(),
        v.message_filter.label()
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border_style(focused))
        .title(Span::styled(
            title,
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if filtered.is_empty() {
        let lines = vec![
            Line::raw(""),
            Line::styled("  No inter-agent messages matching filter.", dim()),
            Line::styled(
                "  Press m to message the selected agent, or b to broadcast to all.",
                dim(),
            ),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let room = (inner.height as usize).max(1);
    let start = super::sidebar_window(v.selected_message, filtered.len(), room);
    let max_w = (inner.width as usize).saturating_sub(2).max(16);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(room);
    for (idx, msg) in filtered.iter().enumerate().skip(start).take(room) {
        let is_sel = idx == v.selected_message;
        let marker = if is_sel { "▸ " } else { "  " };
        let from_lbl = v.snapshot.member_label(&msg.from_launch);
        let to_lbl = msg
            .to_launch
            .as_deref()
            .map(|r| v.snapshot.member_label(r))
            .unwrap_or_else(|| "* (all)".into());
        let time_str = fmt_time(msg.created_ns / 1_000_000_000);
        let single_line_body = msg.body.lines().next().unwrap_or("");
        let prefix_len = 22 + from_lbl.len() + to_lbl.len() + msg.topic.len();
        let body_budget = max_w.saturating_sub(prefix_len).max(12);
        let row_bg = if is_sel && focused {
            Style::default().bg(Color::Rgb(24, 38, 52))
        } else {
            Style::default()
        };

        lines.push(
            Line::from(vec![
                Span::styled(
                    marker,
                    if is_sel {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                ),
                Span::styled(format!("{time_str} "), dim()),
                Span::styled(from_lbl, Style::default().fg(Color::Cyan)),
                Span::styled(" → ", dim()),
                Span::styled(to_lbl, Style::default().fg(Color::White)),
                Span::raw(" "),
                Span::styled(format!("[{}]", msg.topic), topic_style(&msg.topic)),
                Span::raw(" "),
                Span::styled(
                    truncate_str(single_line_body, body_budget),
                    Style::default().fg(Color::Gray),
                ),
            ])
            .style(row_bg),
        );
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer_bar(f: &mut Frame, area: Rect, v: &SwarmView) {
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

    let pairs: &[(&str, &str)] = match v.pane {
        SwarmPane::Agents => &[
            ("1-4/Tab", "pane"),
            ("r", "launch swarm"),
            ("n", "new worker"),
            ("Enter", "attach"),
            ("c", "set coord"),
            ("w", "wake"),
            ("m", "message"),
            ("b", "broadcast"),
            ("x", "stop"),
            ("Esc", "back"),
        ],
        SwarmPane::Preview => &[
            ("1-4/Tab", "pane"),
            ("v/←/→", "mode (Term/Task/Diff)"),
            ("j/k", "scroll"),
            ("Enter", "attach"),
            ("m", "message"),
            ("Esc", "back"),
        ],
        SwarmPane::Board => &[
            ("1-4/Tab", "pane"),
            ("l/←/→", "Tasks/Leases"),
            ("n", "new task"),
            ("e", "edit"),
            ("r", "delegate task"),
            ("d/x", "cancel/unlease"),
            ("Enter", "inspect"),
            ("Esc", "back"),
        ],
        SwarmPane::Messages => &[
            ("1-4/Tab", "pane"),
            ("f/←/→", "filter"),
            ("Enter/m", "reply"),
            ("b/n", "broadcast"),
            ("j/k", "scroll"),
            ("Esc", "back"),
        ],
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

fn draw_modal(f: &mut Frame, area: Rect, modal: &Modal) {
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

        Modal::NewTask {
            title,
            instructions,
            files,
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
                .title(" New Shared Board Task ");
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
            for l in instructions.text.lines().take(4) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::from(vec![
                field_span("File Globs (comma): ", *field == TaskField::Files),
                Span::styled(files.text.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                field_span("Isolation: ", *field == TaskField::Isolation),
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
                ("Ctrl+S", "create task"),
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
            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                Span::styled("  Topic (Tab): ", dim()),
                Span::styled(format!("[{topic}]"), topic_style(topic)),
                Span::raw("   "),
                Span::styled(
                    format!("Urgent (Ctrl+U): {}", if *urgent { "YES" } else { "no" }),
                    if *urgent {
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                ),
                Span::raw("   "),
                Span::styled(
                    format!("Wake PTY (Ctrl+W): {}", if *wake { "YES" } else { "no" }),
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
            lines.push(field_header("Message Body:", true));
            for l in body.text.lines().take(6) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l.to_string(), Style::default().fg(Color::White)),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(hints(&[
                ("Tab", "cycle topic"),
                ("Ctrl+U", "urgent"),
                ("Ctrl+W", "wake"),
                ("Ctrl+S", "send"),
                ("Esc", "cancel"),
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
        Confirm::ReleaseLease { pattern } => {
            format!("Force-release cooperative file lease `{pattern}`?")
        }
        Confirm::CancelTask { task_id, title } => {
            format!("Cancel task [{}] \"{title}\"?", short_id(task_id))
        }
        Confirm::StopSession { label, .. } => {
            format!("Stop swarm session {label}?")
        }
    };

    let lines = vec![
        Line::raw(""),
        Line::styled(format!("  {prompt}"), Style::default().fg(Color::White)),
        Line::raw(""),
        hints(&[("y / Enter", "confirm"), ("n / Esc", "cancel")]),
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

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    } else {
        s.to_string()
    }
}
