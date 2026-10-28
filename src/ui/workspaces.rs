//! Drawing the Workspaces Explorer screen (`crate::app::workspaces_view`).

use super::flow::{dim, head, hints};
use crate::app::workspaces_view::{AgentItemRef, Pane, WorkspacesView, tilde_path};
use crate::tracing::cli::{fmt_cost, fmt_tokens};
use crate::workspace::SummarySource;
use crate::workspace::activity::format_interval;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub fn draw(f: &mut Frame, area: Rect, v: &WorkspacesView) {
    f.render_widget(Clear, area);

    let root_live = v
        .nodes
        .first()
        .map(|n| n.activity.live_sessions.len())
        .unwrap_or(0);
    let title = format!(
        " Workspaces · {} ({} folder{}{}) ",
        tilde_path(&v.root),
        v.filtered_indices.len(),
        if v.filtered_indices.len() == 1 {
            ""
        } else {
            "s"
        },
        if root_live > 0 {
            format!(", {root_live} ● live")
        } else {
            String::new()
        }
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
    let left_width = (body.width * 38 / 100).clamp(28, 48);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Length(left_width), Constraint::Min(0)]).areas(body);

    draw_tree_pane(f, left_area, v);
    draw_dossier_pane(f, right_area, v);
    draw_footer_bar(f, foot, v);
}

fn draw_tree_pane(f: &mut Frame, area: Rect, v: &WorkspacesView) {
    let focused = v.pane == Pane::Tree;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            " Folders ",
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
        Span::styled("(←/→ root · / filter)", dim()),
    ]));

    if v.filtering || !v.filter_text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" / ", Style::default().fg(Color::Cyan)),
            Span::raw(v.filter_text.clone()),
            if v.filtering {
                Span::styled("▏", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
        ]));
    }
    lines.push(Line::raw(""));

    if v.filtered_indices.is_empty() {
        lines.push(Line::styled("  No matching folders.", dim()));
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let room = (inner.height as usize).saturating_sub(lines.len()).max(1);
    let start = super::sidebar_window(v.selected_tree, v.filtered_indices.len(), room);
    let max_w = (inner.width as usize).saturating_sub(1);

    for (vis_idx, &node_idx) in v.filtered_indices.iter().enumerate().skip(start).take(room) {
        let node = &v.nodes[node_idx];
        let is_sel = vis_idx == v.selected_tree;
        let indent = if node.depth == 0 {
            String::new()
        } else {
            "  ".repeat(node.depth)
        };
        let marker = if is_sel { "▸ " } else { "  " };
        let live_count = node.activity.live_sessions.len();
        let badge = node.stack.short_badge();
        let right_tag = if !badge.is_empty() {
            badge
        } else if node.child_project_count > 0 {
            format!("{} proj", node.child_project_count)
        } else if node.activity.stats.subtree_sessions_30d > 0 {
            format!("{} sess", node.activity.stats.subtree_sessions_30d)
        } else {
            String::new()
        };

        let label_name = if node.depth == 0 || node.child_project_count > 0 {
            format!("▾ {}", node.name)
        } else {
            node.name.clone()
        };

        let live_str = if live_count > 0 {
            format!(" ●{live_count}")
        } else {
            String::new()
        };

        let left_part = format!("{marker}{indent}{label_name}{live_str}");
        let left_chars = left_part.chars().count();
        let right_chars = right_tag.chars().count();

        let row_style = if is_sel && focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if node.is_project {
            Style::default()
        } else {
            dim()
        };

        let mut spans = vec![Span::styled(
            truncate_chars(&left_part, max_w.saturating_sub(right_chars + 1)),
            row_style,
        )];
        if !right_tag.is_empty() && left_chars + right_chars + 2 <= max_w {
            let pad = max_w.saturating_sub(left_chars + right_chars);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                right_tag,
                if is_sel && focused {
                    row_style
                } else {
                    Style::default().fg(Color::Yellow)
                },
            ));
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_dossier_pane(f: &mut Frame, area: Rect, v: &WorkspacesView) {
    let Some(node) = v.selected_node() else {
        return;
    };
    let focused = v.pane == Pane::Agents;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. Folder title + path + git branch
    let mut header_spans = vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            node.name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", tilde_path(&node.path)), dim()),
    ];
    if let Some(branch) = &node.git.branch {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            format!("[git: {branch}]"),
            Style::default().fg(Color::Green),
        ));
    }
    lines.push(Line::from(header_spans));

    // 2. Stack & Config lines
    lines.push(Line::from(vec![
        Span::styled("  Stack    ", dim()),
        Span::styled(node.stack.detail_line(), Style::default().fg(Color::Yellow)),
    ]));

    let badges = node.customization.summary_badges();
    if !badges.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Config   ", dim()),
            Span::raw(badges.join(" · ")),
        ]));
    }

    if let Some(err) = &v.error {
        lines.push(Line::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        ));
    } else if let Some(note) = &v.notice {
        lines.push(Line::styled(
            format!("  {note}"),
            Style::default().fg(Color::Cyan),
        ));
    }

    lines.push(Line::raw(""));

    // 3. Project Summary section
    let src_tag = match &node.summary.source {
        SummarySource::CachedLlm {
            harness,
            stale: false,
            ..
        } => format!("LLM summary · {harness}"),
        SummarySource::CachedLlm {
            harness,
            stale: true,
            ..
        } => format!("LLM summary · {harness} · stale, press s to refresh"),
        SummarySource::Manifest => "from manifest".to_string(),
        SummarySource::DocLead { file } => format!("from {file}"),
        SummarySource::Synthesized => "auto-detected".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled("  Summary ", head()),
        Span::styled(format!("({src_tag})"), dim()),
    ]));
    for wrapped in wrap_text(&node.summary.text, (area.width as usize).saturating_sub(4)) {
        lines.push(Line::raw(format!("  {wrapped}")));
    }
    if let Some(arch) = &node.summary.architecture {
        for wrapped in wrap_text(
            &format!("Architecture: {arch}"),
            (area.width as usize).saturating_sub(4),
        ) {
            lines.push(Line::styled(format!("  {wrapped}"), dim()));
        }
    }
    if !node.summary.entry_points.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Entry    ", dim()),
            Span::raw(node.summary.entry_points.join(" · ")),
        ]));
    }

    lines.push(Line::raw(""));

    // 4. Agents & Roll-up Activity section
    let stats = &node.activity.stats;
    let harness_breakdown = if stats.by_harness.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = stats
            .by_harness
            .iter()
            .map(|(h, n)| format!("{h}: {n}"))
            .collect();
        format!("  [{}]", parts.join(" · "))
    };
    lines.push(Line::from(vec![
        Span::styled(
            "  Agents in this folder & sub-folders ",
            if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                head()
            },
        ),
        Span::styled(
            format!(
                "(30d: {} sess · {} tok · {}{})",
                stats.subtree_sessions_30d,
                fmt_tokens(Some(stats.total_tokens_30d)),
                fmt_cost(Some(stats.total_cost_usd_30d)),
                harness_breakdown
            ),
            dim(),
        ),
    ]));

    let all_items = v.agent_items_for_selected();
    if all_items.is_empty() {
        lines.push(Line::styled(
            "  No active or recorded agent sessions in this folder yet. Press Enter or n to start one.",
            dim(),
        ));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        return;
    }

    let max_w = (area.width as usize).saturating_sub(4).max(20);
    let room = (area.height as usize)
        .saturating_sub(lines.len() + 1)
        .max(2);
    let start = super::sidebar_window(v.selected_agent, all_items.len(), room);

    for (idx, item) in all_items.iter().enumerate().skip(start).take(room) {
        let is_sel = idx == v.selected_agent;
        let prefix = if is_sel { "  ▸ " } else { "    " };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        match item {
            AgentItemRef::Live(i) => {
                let l = &node.activity.live_sessions[*i];
                let sub = l
                    .rel_subpath
                    .as_deref()
                    .map(|s| format!(" [in {s}]"))
                    .unwrap_or_default();
                let dot_color = if l.is_running {
                    Color::Green
                } else {
                    Color::Yellow
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, row_style),
                    Span::styled("● live      ", Style::default().fg(dot_color)),
                    Span::styled(
                        truncate_chars(
                            &format!("{:<12} {}{}", l.profile_name, l.status_label, sub),
                            max_w.saturating_sub(14),
                        ),
                        row_style,
                    ),
                ]));
            }
            AgentItemRef::Scheduled(i) => {
                let s = &node.activity.scheduled_agents[*i];
                let sub = s
                    .rel_subpath
                    .as_deref()
                    .map(|p| format!(" [in {p}]"))
                    .unwrap_or_default();
                let status = if s.paused {
                    "‖ paused".to_string()
                } else {
                    s.last_outcome
                        .as_deref()
                        .map(|o| {
                            if let Some(age) = &s.last_run_at {
                                format!("{o} ({age})")
                            } else {
                                o.to_string()
                            }
                        })
                        .unwrap_or_else(|| "scheduled".into())
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, row_style),
                    Span::styled("⟳ loop      ", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        truncate_chars(
                            &format!(
                                "{} (every {}, {}) · {status}{sub}",
                                s.loop_id,
                                format_interval(s.interval_s),
                                s.harness
                            ),
                            max_w.saturating_sub(14),
                        ),
                        row_style,
                    ),
                ]));
            }
            AgentItemRef::Persona(i) => {
                let p = &node.customization.project_personas[*i];
                let task_note = p
                    .task_label
                    .as_deref()
                    .map(|t| format!(" [{t}]"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(prefix, row_style),
                    Span::styled("⚙ persona   ", Style::default().fg(Color::Blue)),
                    Span::styled(
                        truncate_chars(
                            &format!("{}{} — {}", p.name, task_note, p.description),
                            max_w.saturating_sub(14),
                        ),
                        row_style,
                    ),
                ]));
            }
            AgentItemRef::Recent(i) => {
                let r = &node.activity.recent_sessions[*i];
                let sub = r
                    .rel_subpath
                    .as_deref()
                    .map(|p| format!(" [in {p}]"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(prefix, row_style),
                    Span::styled("↺ session   ", dim()),
                    Span::styled(
                        truncate_chars(
                            &format!(
                                "{:>7}  {:<11} {:>2}t  \"{}\"{sub}",
                                r.age_label, r.provider, r.turn_count, r.title
                            ),
                            max_w.saturating_sub(14),
                        ),
                        row_style,
                    ),
                ]));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_footer_bar(f: &mut Frame, area: Rect, v: &WorkspacesView) {
    let pairs: &[(&str, &str)] = if v.filtering {
        &[("Enter", "done"), ("Esc", "clear filter")]
    } else if v.pane == Pane::Agents {
        &[
            ("Enter/r", "open/resume"),
            ("n", "new session"),
            ("e", "edit/docs"),
            ("Tab", "folders"),
            ("s", "LLM summary"),
            ("Ctrl+R", "rescan"),
            ("Esc", "back"),
        ]
    } else {
        &[
            ("Enter", "open agent here"),
            ("r", "resume latest"),
            ("n", "new session"),
            ("Tab", "focus agents"),
            ("e", "open docs"),
            ("s", "LLM summary"),
            ("/", "filter"),
            ("Ctrl+R", "rescan"),
            ("q", "close"),
        ]
    };
    f.render_widget(Paragraph::new(hints(pairs)), area);
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        head.push('…');
        head
    }
}

fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in s.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
