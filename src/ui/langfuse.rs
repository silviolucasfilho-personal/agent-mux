//! Drawing the Langfuse screen (`crate::app::langfuse_view`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::flow::{dim, head, hints};
use crate::app::langfuse_view::{FetchStatus, LangfusePane, LangfuseView};

pub fn draw(f: &mut Frame, area: Rect, v: &LangfuseView) {
    f.render_widget(Clear, area);

    let host = v
        .langfuse
        .as_ref()
        .map(|lf| lf.host.as_str())
        .unwrap_or("no host");

    let proj = v.project_id.as_deref().unwrap_or("auto-detect");

    let status_desc = match &v.status {
        FetchStatus::Idle => format!("{} traces", v.traces.len()),
        FetchStatus::Loading(msg) => format!("⟳ {msg}"),
        FetchStatus::Error(err) => format!("⚠ {err}"),
    };

    let title = format!(" Langfuse · {host} (proj: {proj}) · {status_desc} ");

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

    if v.langfuse.is_none() {
        draw_unconfigured_banner(f, body);
        draw_footer(f, foot, v);
        return;
    }

    if v.zoomed {
        match v.pane {
            LangfusePane::Traces => draw_traces_pane(f, body, v),
            LangfusePane::Tree => draw_tree_pane(f, body, v),
            LangfusePane::Detail => draw_detail_pane(f, body, v),
        }
    } else {
        let left_width = (body.width * 33 / 100).clamp(24, 46);
        let mid_width = (body.width * 33 / 100).clamp(24, 46);
        let [left_area, mid_area, right_area] = Layout::horizontal([
            Constraint::Length(left_width),
            Constraint::Length(mid_width),
            Constraint::Min(24),
        ])
        .areas(body);

        draw_traces_pane(f, left_area, v);
        draw_tree_pane(f, mid_area, v);
        draw_detail_pane(f, right_area, v);
    }

    draw_footer(f, foot, v);
}

fn draw_unconfigured_banner(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::styled(
            "  Langfuse is not configured or credentials are missing",
            head(),
        ),
        Line::raw(""),
        Line::styled(
            "  To browse remote traces and navigate to Langfuse Web, configure credentials",
            dim(),
        ),
        Line::styled("  in profiles.toml or via environment variables:", dim()),
        Line::raw(""),
        Line::styled("    [tracing.langfuse]", Style::default().fg(Color::Yellow)),
        Line::styled(
            "    host = \"https://cloud.langfuse.com\"",
            Style::default().fg(Color::White),
        ),
        Line::styled(
            "    public_key = \"pk-lf-...\"",
            Style::default().fg(Color::White),
        ),
        Line::styled(
            "    secret_key = \"sk-lf-...\"",
            Style::default().fg(Color::White),
        ),
        Line::raw(""),
        Line::styled(
            "  Or set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY in your shell environment.",
            dim(),
        ),
        Line::raw(""),
        Line::styled("  Press Esc or q to return.", dim()),
    ];

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_traces_pane(f: &mut Frame, area: Rect, v: &LangfuseView) {
    let focused = v.pane == LangfusePane::Traces;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            dim()
        })
        .title(format!(" Traces (p. {}/{}) ", v.page, v.total_pages.max(1)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.traces.is_empty() {
        let msg = match &v.status {
            FetchStatus::Loading(m) => format!("  {m}"),
            FetchStatus::Error(e) => format!("  Error: {e}"),
            FetchStatus::Idle => "  No traces found matching filters.".into(),
        };
        let p = Paragraph::new(vec![Line::raw(""), Line::styled(msg, dim())]);
        f.render_widget(p, inner);
        return;
    }

    let mut lines = Vec::new();
    let visible_rows = inner.height as usize / 2;
    let start_idx = v
        .selected_trace
        .saturating_sub(visible_rows.saturating_sub(1));
    let end_idx = (start_idx + visible_rows + 2).min(v.traces.len());

    for (i, trace) in v
        .traces
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(end_idx - start_idx)
    {
        let is_selected = i == v.selected_trace;
        let mut line1_spans = Vec::new();

        let mark = if is_selected { "▸ " } else { "  " };
        line1_spans.push(Span::styled(
            mark,
            if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim()
            },
        ));

        let name = trace.name.as_deref().unwrap_or("trace");
        line1_spans.push(Span::styled(
            format!("{:<18}", truncate(name, 18)),
            if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ));

        if let Some(sess) = &trace.session_id {
            line1_spans.push(Span::raw(" "));
            line1_spans.push(Span::styled(
                truncate(sess, 14),
                Style::default().fg(Color::Yellow),
            ));
        }

        let mut line2_spans = Vec::new();
        line2_spans.push(Span::raw("    "));

        let time_str = if trace.timestamp.len() >= 19 {
            &trace.timestamp[11..19]
        } else {
            &trace.timestamp
        };
        line2_spans.push(Span::styled(format!("{time_str} "), dim()));

        if let Some(lat) = trace.latency {
            line2_spans.push(Span::styled(format!("{lat:.1}s "), dim()));
        }

        if let Some(cost) = trace.total_cost
            && cost > 0.0
        {
            line2_spans.push(Span::styled(
                format!("${cost:.3} "),
                Style::default().fg(Color::Green),
            ));
        }

        for tag in &trace.tags {
            line2_spans.push(Span::styled(
                format!("#{tag} "),
                Style::default().fg(Color::Blue),
            ));
        }

        let row_style = if is_selected {
            Style::default().bg(Color::Rgb(30, 40, 50))
        } else {
            Style::default()
        };

        lines.push(Line::from(line1_spans).style(row_style));
        lines.push(Line::from(line2_spans).style(row_style));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn draw_tree_pane(f: &mut Frame, area: Rect, v: &LangfuseView) {
    let focused = v.pane == LangfusePane::Tree;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            dim()
        })
        .title(format!(" Observations ({}) ", v.tree_nodes.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.tree_nodes.is_empty() {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::styled("  No observations loaded for trace.", dim()),
        ]);
        f.render_widget(p, inner);
        return;
    }

    let mut lines = Vec::new();
    let visible_rows = inner.height as usize;
    let start_idx = v
        .selected_node
        .saturating_sub(visible_rows.saturating_sub(1));
    let end_idx = (start_idx + visible_rows).min(v.tree_nodes.len());

    for (i, node) in v
        .tree_nodes
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(end_idx - start_idx)
    {
        let is_selected = i == v.selected_node;
        let mut spans = Vec::new();

        let mark = if is_selected { "▸ " } else { "  " };
        spans.push(Span::styled(
            mark,
            if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim()
            },
        ));

        // Indentation
        for _ in 0..node.depth {
            spans.push(Span::styled("  ", dim()));
        }

        // Icon based on type
        let (icon, icon_style) = match node.obs_type.as_str() {
            "TRACE" => ("▼ ", Style::default().fg(Color::Cyan)),
            "GENERATION" => ("★ ", Style::default().fg(Color::Yellow)),
            "EVENT" => ("• ", Style::default().fg(Color::Magenta)),
            _ => ("⚙ ", Style::default().fg(Color::Green)),
        };
        spans.push(Span::styled(icon, icon_style));

        // Node name
        spans.push(Span::styled(
            truncate(&node.name, 18),
            if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ));

        // Model badge if generation
        if let Some(m) = &node.model {
            spans.push(Span::styled(
                format!(" [{}]", truncate(m, 12)),
                Style::default().fg(Color::Cyan),
            ));
        }

        // Duration
        if let Some(dur) = node.duration_s {
            spans.push(Span::styled(format!(" {dur:.2}s"), dim()));
        }

        // Error mark
        if node.is_error {
            spans.push(Span::styled(" ✗", Style::default().fg(Color::Red)));
        }

        let row_style = if is_selected {
            Style::default().bg(Color::Rgb(30, 40, 50))
        } else {
            Style::default()
        };

        lines.push(Line::from(spans).style(row_style));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn draw_detail_pane(f: &mut Frame, area: Rect, v: &LangfuseView) {
    let focused = v.pane == LangfusePane::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            dim()
        })
        .title(" Detail & Inspection ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(detail) = &v.current_detail else {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::styled("  Select a trace to inspect details.", dim()),
        ]);
        f.render_widget(p, inner);
        return;
    };

    let mut lines = Vec::new();

    // Check if an observation is selected, else show trace summary
    if let Some(obs) = v.selected_observation() {
        lines.push(Line::styled(
            format!(
                "Observation: {} ({})",
                obs.name.as_deref().unwrap_or(&obs.id),
                obs.observation_type
            ),
            head(),
        ));
        lines.push(Line::styled(format!("ID: {}", obs.id), dim()));

        if let Some(model) = &obs.model {
            lines.push(Line::styled(
                format!("Model: {model}"),
                Style::default().fg(Color::Cyan),
            ));
        }
        if let Some(dur) = obs.latency {
            lines.push(Line::styled(format!("Duration: {dur:.3}s"), dim()));
        }
        if let Some(usage) = &obs.usage {
            lines.push(Line::styled(
                format!(
                    "Tokens: {} in / {} out ({} total)",
                    usage.prompt_tokens.unwrap_or(0),
                    usage.completion_tokens.unwrap_or(0),
                    usage.total_tokens.unwrap_or(0)
                ),
                Style::default().fg(Color::Yellow),
            ));
        }
        if let Some(cost) = obs.calculated_total_cost {
            lines.push(Line::styled(
                format!("Cost: ${cost:.4}"),
                Style::default().fg(Color::Green),
            ));
        }
        if let Some(msg) = &obs.status_message {
            lines.push(Line::styled(
                format!("Status Message: {msg}"),
                Style::default().fg(Color::Red),
            ));
        }

        lines.push(Line::raw(""));
        if let Some(input) = &obs.input {
            lines.push(Line::styled("─── Input ───", dim()));
            for l in format_json_val(input) {
                lines.push(Line::raw(l));
            }
        }
        if let Some(output) = &obs.output {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─── Output ───", dim()));
            for l in format_json_val(output) {
                lines.push(Line::raw(l));
            }
        }
        if let Some(meta) = &obs.metadata {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─── Metadata ───", dim()));
            for l in format_json_val(meta) {
                lines.push(Line::raw(l));
            }
        }
    } else {
        // Trace level summary
        lines.push(Line::styled(
            format!("Trace: {}", detail.name.as_deref().unwrap_or(&detail.id)),
            head(),
        ));
        lines.push(Line::styled(format!("Trace ID: {}", detail.id), dim()));
        if let Some(sess) = &detail.session_id {
            lines.push(Line::styled(
                format!("Session: {sess}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        if let Some(dur) = detail.latency {
            lines.push(Line::styled(format!("Duration: {dur:.2}s"), dim()));
        }
        if let Some(cost) = detail.total_cost {
            lines.push(Line::styled(
                format!("Total Cost: ${cost:.4}"),
                Style::default().fg(Color::Green),
            ));
        }

        if !detail.scores.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─── Scores & Evals ───", dim()));
            for sc in &detail.scores {
                let val_str = sc
                    .value
                    .map(|v| format!("{v:.2}"))
                    .or_else(|| sc.string_value.clone())
                    .unwrap_or_default();
                let comment = sc.comment.as_deref().unwrap_or("");
                lines.push(Line::styled(
                    format!("  ★ {}: {}  {}", sc.name, val_str, comment),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }

        if let Some(input) = &detail.input {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─── Input ───", dim()));
            for l in format_json_val(input) {
                lines.push(Line::raw(l));
            }
        }

        if let Some(output) = &detail.output {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─── Output ───", dim()));
            for l in format_json_val(output) {
                lines.push(Line::raw(l));
            }
        }
    }

    let skip = v.detail_scroll.min(lines.len().saturating_sub(1));
    let display_lines: Vec<Line> = lines.into_iter().skip(skip).collect();

    let p = Paragraph::new(display_lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn format_json_val(val: &serde_json::Value) -> Vec<String> {
    match val {
        serde_json::Value::String(s) => s.lines().map(|l| format!("  {l}")).collect(),
        _ => {
            if let Ok(pretty) = serde_json::to_string_pretty(val) {
                pretty.lines().map(|l| format!("  {l}")).collect()
            } else {
                vec![format!("  {val}")]
            }
        }
    }
}

fn draw_footer(f: &mut Frame, area: Rect, v: &LangfuseView) {
    if let Some(input) = &v.filter_input {
        let line = Line::from(vec![
            Span::styled(
                " Filter: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(input, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Cyan)),
            Span::styled("  [Enter: apply, Esc: cancel]", dim()),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let line = hints(&[
        ("Tab", "Cycle"),
        ("↑/↓", "Move"),
        ("w", "Open in Web"),
        ("/", "Filter"),
        ("n/p", "Page"),
        ("z", "Zoom"),
        ("⌃R", "Refresh"),
        ("q", "Back"),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let mut truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        truncated.push('…');
        truncated
    } else {
        s.to_string()
    }
}
