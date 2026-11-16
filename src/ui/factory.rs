//! Drawing the Factory screen (`crate::app::factory`): the items of one
//! repository in a tree on the left, the selected one in tabs on the right.

use super::flow::{dim, head, hints};
use crate::app::factory::{FactoryView, Input, Row, Tab, badge, row_name};
use crate::factory::{Item, Source, State, comment};
use crate::workflows::run_view::tokens_short;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use serde_json::Value;

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn state_color(state: State) -> Color {
    match state {
        State::Running => Color::Cyan,
        State::Delivered | State::Answered => Color::Green,
        State::NeedsAnswer | State::Unresolved => Color::Yellow,
        State::Rejected | State::Failed => Color::Red,
        State::Queued | State::Cancelled => Color::DarkGray,
    }
}

pub fn draw(f: &mut Frame, area: Rect, v: &FactoryView) {
    f.render_widget(Clear, area);
    let [body, input, foot] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(if v.text_entry_active() || v.confirmation_active() {
            1
        } else {
            0
        }),
        Constraint::Length(1),
    ])
    .areas(area);
    let left_width = (body.width * 36 / 100).clamp(30, 46).min(body.width);
    let [left, right] =
        Layout::horizontal([Constraint::Length(left_width), Constraint::Min(0)]).areas(body);
    draw_tree(f, left, v);
    draw_pane(f, right, v);
    draw_input(f, input, v);
    draw_foot(f, foot, v);
}

fn draw_tree(f: &mut Frame, area: Rect, v: &FactoryView) {
    let needs = v.needs_you();
    let title = format!(
        " factory · {}{} ",
        v.repo_name,
        if needs > 0 {
            format!(" · {needs} need you")
        } else {
            String::new()
        }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &v.error {
        lines.push(Line::styled(
            format!(" {e}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !v.filter.is_empty() && !v.filtering {
        lines.push(Line::styled(format!(" filter: {}", v.filter), dim()));
    }
    if v.items.is_empty() && v.error.is_none() {
        lines.push(Line::styled(" no work items yet: [n] starts one", dim()));
    }
    let mut cursor_line = 0;
    for (i, row) in v.rows.iter().enumerate() {
        let selected = i == v.cursor;
        if selected {
            cursor_line = lines.len();
        }
        let marker = if selected { "> " } else { "  " };
        match row {
            Row::Heading(group, n) => {
                lines.push(Line::styled(format!(" ▸ {} ({n})", group.label()), head()));
            }
            Row::Item(idx) => {
                let it = &v.items[*idx];
                let process = (it.id == v.detail.item).then_some(v.detail.process);
                let right = badge(it, process);
                let room = width.saturating_sub(right.chars().count() + 4);
                let name = cut(&row_name(it), room);
                let pad = width.saturating_sub(name.chars().count() + right.chars().count() + 3);
                let mut spans = vec![
                    Span::raw(format!("{marker}{name}")),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(right, Style::default().fg(state_color(it.state))),
                    Span::raw(" "),
                ];
                if selected {
                    for s in &mut spans {
                        s.style = s.style.add_modifier(Modifier::REVERSED);
                    }
                }
                lines.push(Line::from(spans));
            }
            Row::Brain => {
                let size = if v.brain.trim().is_empty() {
                    "empty".to_string()
                } else {
                    tokens_short(v.brain.chars().count() as u64)
                };
                let pad = width.saturating_sub(size.chars().count() + 10);
                let style = if selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(
                    format!("{marker}▸ brain{}{size} ", " ".repeat(pad)),
                    style,
                ));
            }
        }
    }
    let visible = usize::from(inner.height);
    let start = super::sidebar_window(cursor_line, lines.len(), visible);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        inner,
    );
}

fn field(name: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {name:<11} "), dim()),
        Span::raw(value.into()),
    ])
}

fn item_lines(it: &Item, v: &FactoryView) -> Vec<Line<'static>> {
    let mut out = vec![
        field(
            "state",
            match (it.state, v.detail.process) {
                (State::Running, false) => {
                    "running, but its process is gone: [r] takes it over".to_string()
                }
                (s, _) => s.as_str().to_string(),
            },
        ),
        field(
            "source",
            match it.source {
                Source::Issue => format!("issue #{} of {}", it.source_ref, it.repo),
                Source::Ci => format!("failing checks on pull request #{}", it.source_ref),
                Source::Manual => "typed by the operator".to_string(),
            },
        ),
    ];
    if let Some(r) = &it.requester {
        out.push(field("requester", format!("@{r}")));
    }
    out.push(field(
        "started",
        if it.attended {
            "by the operator"
        } else {
            "by the label poll (unattended)"
        },
    ));
    let facts: Vec<String> = [&it.kind, &it.priority, &it.complexity]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    if !facts.is_empty() {
        out.push(field("triage", facts.join(" · ")));
    }
    if it.cycles > 0 {
        out.push(field("revisions", it.cycles.to_string()));
    }
    for (name, value) in [
        ("branch", &it.branch),
        ("pull", &it.pr_url),
        ("worktree", &it.worktree),
        ("run", &it.run_id),
    ] {
        if let Some(value) = value {
            out.push(field(name, value.clone()));
        }
    }
    out.push(field("id", it.id.clone()));
    if let Some(e) = &it.error {
        out.push(Line::raw(""));
        out.push(Line::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    if !it.body.trim().is_empty() {
        out.push(Line::raw(""));
        for l in it.body.trim().lines() {
            out.push(Line::raw(format!("  {l}")));
        }
    }
    out
}

fn line_lines(it: &Item, v: &FactoryView) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let said = comment::station_lines(&v.detail.journal);
    for (entry, line) in v.detail.journal.iter().zip(said) {
        let failed = entry.kind == "null";
        let text = line.trim_start_matches("- ").to_string();
        let (name, rest) = text.split_once(": ").unwrap_or((text.as_str(), ""));
        let mut spans = vec![
            Span::styled(
                if failed { "  ✗ " } else { "  ✓ " },
                Style::default().fg(if failed { Color::Red } else { Color::Green }),
            ),
            Span::raw(format!("{name:<28} ")),
            Span::raw(rest.to_string()),
        ];
        if entry.tokens > 0 {
            spans.push(Span::styled(
                format!("   {}", tokens_short(entry.tokens)),
                dim(),
            ));
        }
        if let Some(agent) = &entry.agent {
            spans.push(Span::styled(format!("   {agent}"), dim()));
        }
        out.push(Line::from(spans));
    }
    match it.state {
        State::Running if v.detail.process => out.push(Line::styled(
            "  ◷ the next station is working",
            Style::default().fg(Color::Cyan),
        )),
        State::Running => out.push(Line::styled(
            "  the process that ran this item is gone: [r] starts it again",
            Style::default().fg(Color::Yellow),
        )),
        State::Queued => out.push(Line::styled("  not started yet", dim())),
        _ if out.is_empty() => out.push(Line::styled("  no station answered", dim())),
        _ => {}
    }
    if it.run_id.is_some() {
        out.push(Line::raw(""));
        out.push(Line::styled(
            "  [Enter] opens the run: every session, its output and its trace",
            dim(),
        ));
    }
    out
}

fn thread_lines(v: &FactoryView) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for e in &v.detail.events {
        let when = time::OffsetDateTime::from_unix_timestamp_nanos(e.ts_ns as i128)
            .map(crate::loops::format_timestamp)
            .unwrap_or_default();
        let detail = match &e.detail {
            Value::Null => String::new(),
            Value::String(s) => s.lines().next().unwrap_or("").to_string(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" · "),
            other => other.to_string(),
        };
        out.push(Line::from(vec![
            Span::styled(format!("  {when}  "), dim()),
            Span::raw(format!("{:<22} ", e.kind)),
            Span::styled(detail, dim()),
        ]));
    }
    if out.is_empty() {
        out.push(Line::styled("  nothing recorded yet", dim()));
    }
    out
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn result_lines(it: &Item, v: &FactoryView) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let classify = v
        .detail
        .journal
        .iter()
        .rev()
        .find(|e| e.step == "classify")
        .map(|e| e.result.clone())
        .unwrap_or(Value::Null);
    if it.state == State::NeedsAnswer {
        out.push(Line::styled(
            "  Triage needs an answer before anything is built:",
            head(),
        ));
        for (i, q) in strings(&classify, "questions").iter().enumerate() {
            out.push(Line::raw(format!("  {}. {q}", i + 1)));
        }
        out.push(Line::raw(""));
        out.push(Line::styled("  [a] answers", dim()));
        return out;
    }
    if it.state == State::Answered {
        let answer = v
            .detail
            .events
            .iter()
            .rev()
            .find(|e| e.kind == "state.answered")
            .and_then(|e| e.detail.as_str().map(str::to_string))
            .unwrap_or_default();
        for l in answer.lines() {
            out.push(Line::raw(format!("  {l}")));
        }
        return out;
    }
    let review = v
        .detail
        .result
        .get("review")
        .cloned()
        .unwrap_or(Value::Null);
    if review.is_null() {
        out.push(Line::styled(
            match it.state {
                State::Running | State::Queued => "  no review yet",
                _ => "  the run left no review",
            },
            dim(),
        ));
        return out;
    }
    let verdict = review
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .replace('_', " ");
    out.push(Line::styled(format!("  Review: {verdict}"), head()));
    if let Some(s) = review.get("summary").and_then(Value::as_str) {
        out.push(Line::raw(format!("  {s}")));
    }
    out.push(Line::raw(""));
    for c in review
        .get("criteria_results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let pass = c.get("pass").and_then(Value::as_bool) == Some(true);
        let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        out.push(Line::from(vec![
            Span::styled(
                if pass { "  ✓ " } else { "  ✗ " },
                Style::default().fg(if pass { Color::Green } else { Color::Red }),
            ),
            Span::raw(text("criterion")),
        ]));
        out.push(Line::styled(format!("      {}", text("evidence")), dim()));
    }
    for (title, key) in [
        ("Blocking findings", "blocking_findings"),
        ("Suggestions", "suggestions"),
    ] {
        let items = strings(&review, key);
        if items.is_empty() {
            continue;
        }
        out.push(Line::raw(""));
        out.push(Line::styled(format!("  {title}"), head()));
        for i in items {
            out.push(Line::raw(format!("  - {i}")));
        }
    }
    out
}

fn draw_pane(f: &mut Frame, area: Rect, v: &FactoryView) {
    let (title, lines): (String, Vec<Line>) = match v.selected_row() {
        Some(Row::Item(i)) => {
            let it = &v.items[*i];
            let tabs: Vec<String> = Tab::ALL
                .iter()
                .enumerate()
                .map(|(n, t)| {
                    if *t == v.tab {
                        format!("[{} {}]", n + 1, t.label())
                    } else {
                        format!(" {} {} ", n + 1, t.label())
                    }
                })
                .collect();
            let mut lines = vec![Line::styled(format!(" {}", tabs.join(" ")), head())];
            lines.push(Line::raw(""));
            lines.extend(match v.tab {
                Tab::Item => item_lines(it, v),
                Tab::Line => line_lines(it, v),
                Tab::Thread => thread_lines(v),
                Tab::Result => result_lines(it, v),
            });
            (
                format!(" {} · {} ", cut(&row_name(it), 60), it.state.as_str()),
                lines,
            )
        }
        Some(Row::Brain) => {
            let mut lines = vec![
                Line::styled(
                    "  What every station of this repository's runs reads first.",
                    dim(),
                ),
                Line::styled(
                    "  [e] edits it. Stations only suggest notes; they land in the inbox.",
                    dim(),
                ),
                Line::raw(""),
            ];
            if v.brain.trim().is_empty() {
                lines.push(Line::styled("  empty", dim()));
            }
            lines.extend(v.brain.lines().map(|l| Line::raw(format!("  {l}"))));
            (format!(" brain · {} ", v.repo_name), lines)
        }
        _ => (" factory ".to_string(), Vec::new()),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((v.scroll, 0)),
        area,
    );
}

fn draw_input(f: &mut Frame, area: Rect, v: &FactoryView) {
    if area.height == 0 {
        return;
    }
    let line = if let Some(id) = &v.confirm_stop {
        Line::styled(
            format!(" stop and cancel {id}? [y] yes   any other key keeps it"),
            Style::default().fg(Color::Yellow),
        )
    } else if let Some(input) = &v.input {
        let label = match input {
            Input::New(_) => "what is wanted, or #<issue>",
            Input::Answer { .. } => "your answer",
        };
        Line::from(vec![
            Span::styled(format!(" {label}: "), head()),
            Span::raw(format!("{}▏", input.text())),
        ])
    } else {
        Line::from(vec![
            Span::styled(" filter: ", head()),
            Span::raw(format!("{}▏", v.filter)),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_foot(f: &mut Frame, area: Rect, v: &FactoryView) {
    let line = if v.text_entry_active() {
        hints(&[("Enter", "done"), ("Esc", "cancel")])
    } else {
        hints(&[
            ("n", "new"),
            ("a", "answer"),
            ("r", "retry"),
            ("x", "stop"),
            ("Enter", "run"),
            ("o", "pull request"),
            ("1-4", "tabs"),
            ("/", "filter"),
            ("Esc", "close"),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}
