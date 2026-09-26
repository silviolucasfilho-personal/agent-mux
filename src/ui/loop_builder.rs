//! Drawing the loop builder (`crate::app::loop_builder`): the patterns on
//! the left, the selected one's cycle and fields on the right, in the
//! flow builder's look.

use super::flow::{dim, head, hints, key_style, labeled, sel};
use super::{centered, draw_confirm, pane_border, truncate_chars};
use crate::app::loop_builder::{Edits, Focus, LField, LoopBuilderState, Overlay};
use crate::loops::builder::{Origin, format_interval};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

fn origin_style(o: Origin) -> Style {
    match o {
        Origin::Builtin => dim(),
        Origin::Edited => Style::default().fg(Color::Yellow),
        Origin::Yours => Style::default().fg(Color::Magenta),
    }
}

pub fn draw(f: &mut Frame, st: &LoopBuilderState) {
    let full = f.area();
    let area = Rect::new(full.x, full.y, full.width, full.height.saturating_sub(1));
    f.render_widget(Clear, area);
    let [top, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, top, st);
    let left_w = (area.width / 3).clamp(36, 52);
    let [left, right] =
        Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(body);
    draw_list(f, left, st);
    draw_fields(f, right, st);
    f.render_widget(Paragraph::new(footer(st)), foot);
    match &st.overlay {
        Some(Overlay::Confirm { question, .. }) => draw_confirm(f, question),
        Some(Overlay::NewName { text, from }) => {
            let r = centered(f.area(), 70, 5);
            f.render_widget(Clear, r);
            let title = if from.is_some() {
                " Copy as a new pattern "
            } else {
                " New loop pattern "
            };
            let b = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(title, key_style()));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines = labeled("Id", 6, text, true, 1, usize::from(inner.width), "");
            lines.push(Line::raw(""));
            lines.push(hints(&[("Enter", "create"), ("Esc", "cancel")]));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Some(Overlay::Prompt { text }) => {
            let r = centered(f.area(), 110, 24);
            f.render_widget(Clear, r);
            let b = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(" The run's opening prompt ", key_style()));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let [help, body, foot] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .areas(inner);
            f.render_widget(
                Paragraph::new(Line::styled(
                    " Replaces [loop] run of prompts.toml for this pattern. It must keep {invocation}, the triage skill as the harness calls it.",
                    dim(),
                ))
                .wrap(Wrap { trim: true }),
                help,
            );
            f.render_widget(
                Paragraph::new(labeled(
                    "",
                    2,
                    text,
                    true,
                    usize::from(body.height),
                    usize::from(body.width),
                    "",
                )),
                body,
            );
            f.render_widget(
                Paragraph::new(hints(&[
                    ("Enter", "done"),
                    ("Alt+Enter", "new line"),
                    ("Ctrl+E", "editor"),
                    ("Ctrl+R", "back to the default"),
                    ("Esc", "cancel"),
                ])),
                foot,
            );
        }
        Some(Overlay::Multi {
            skills,
            options,
            chosen,
            selected,
        }) => {
            let h = (options.len() as u16 * 2 + 5).clamp(8, 32);
            let r = centered(f.area(), 104, h);
            f.render_widget(Clear, r);
            let title = if *skills {
                " Skills the loop runs (the first is its triage) "
            } else {
                " Checkers: sub-agents that review a change before it is proposed "
            };
            let b = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(title, key_style()));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let w = usize::from(inner.width);
            let mut lines = Vec::new();
            for (n, (name, desc)) in options.iter().enumerate() {
                let order = chosen.iter().position(|c| c == name);
                let mark = match order {
                    Some(i) => format!("[{}]", i + 1),
                    None => "[ ]".into(),
                };
                let style = if n == *selected {
                    sel()
                } else if order.is_some() {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(format!(" {mark} {name}"), style));
                lines.push(Line::styled(
                    truncate_chars(&format!("     {desc}"), w),
                    dim(),
                ));
            }
            let visible = usize::from(inner.height).saturating_sub(1);
            let start = (selected * 2).saturating_sub(visible.saturating_sub(2));
            let mut body: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();
            body.push(hints(&[
                ("Space", "tick"),
                ("J K", "order"),
                ("Enter", "done"),
                ("Esc", "cancel"),
            ]));
            f.render_widget(Paragraph::new(body), inner);
        }
        None => {}
    }
}

fn draw_header(f: &mut Frame, area: Rect, st: &LoopBuilderState) {
    let mut spans = vec![Span::styled(" ⟳ Loop patterns", head())];
    if let Some(it) = st.current() {
        spans.push(Span::styled(format!("  {}", it.pattern.id), head()));
        spans.push(Span::styled(
            format!(" · {}", it.origin.label()),
            origin_style(it.origin),
        ));
        let uses = st.uses(&it.pattern.id);
        spans.push(Span::styled(
            format!(
                "   {} registered loop(s) use it{}   ",
                uses,
                if it.dirty() { " · unsaved" } else { "" }
            ),
            dim(),
        ));
        let n = st.problems().len();
        spans.push(if n == 0 {
            Span::styled("✓ valid", Style::default().fg(Color::Green))
        } else {
            Span::styled(format!("! {n} to fix"), Style::default().fg(Color::Red))
        });
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn footer(st: &LoopBuilderState) -> Line<'static> {
    if st.edit.is_some() {
        return hints(&[("Enter", "done"), ("Esc", "cancel")]);
    }
    match st.focus {
        Focus::List => hints(&[
            ("↑↓", "patterns"),
            ("→", "fields"),
            ("s", "save"),
            ("n", "new"),
            ("c", "copy"),
            ("R", "restore built-in"),
            ("d", "delete yours"),
            ("Esc", "close"),
        ]),
        Focus::Fields => {
            let what = match st.current_field().map(LField::edits) {
                Some(Edits::Cycle) => ("←→", "change"),
                Some(Edits::Pick) => ("Enter", "choose"),
                _ => ("Enter", "type"),
            };
            hints(&[
                ("↑↓", "fields"),
                what,
                ("s", "save"),
                ("R", "restore built-in"),
                ("Esc", "patterns"),
            ])
        }
    }
}

fn draw_list(f: &mut Frame, area: Rect, st: &LoopBuilderState) {
    let focused = st.focus == Focus::List;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(" Patterns ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    for (i, it) in st.items.iter().enumerate() {
        let on = i == st.selected;
        let style = if on && focused {
            sel()
        } else if on {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mark = if it.dirty() { "*" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                truncate_chars(
                    &format!(
                        "{mark}{:<22} {:>4} ",
                        truncate_chars(&it.pattern.id, 22),
                        format_interval(it.pattern.default_interval_s)
                    ),
                    w.saturating_sub(9),
                ),
                style,
            ),
            Span::styled(it.origin.label(), origin_style(it.origin)),
        ]));
    }
    lines.push(Line::raw(""));
    let new_on = st.selected >= st.items.len();
    lines.push(Line::styled(
        " + new pattern",
        if new_on && focused {
            sel()
        } else {
            key_style()
        },
    ));
    if let Some(e) = &st.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" ! {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(" * unsaved", dim()));
    lines.push(Line::styled(
        " built-in · edited (your copy) · yours",
        dim(),
    ));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_fields(f: &mut Frame, area: Rect, st: &LoopBuilderState) {
    let focused = st.focus == Focus::Fields;
    let title = st
        .current()
        .map(|i| format!(" {} ", i.pattern.name))
        .unwrap_or_else(|| " New pattern ".into());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(Span::styled(title, head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = usize::from(inner.width);
    let Some(it) = st.current() else {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(" A loop pattern is what a registered loop runs on a schedule:"),
                Line::styled(
                    " its goal, its skills, its checkers, its gates and its budget.",
                    dim(),
                ),
                Line::raw(""),
                Line::from(vec![
                    Span::raw(" Enter or "),
                    Span::styled("n", key_style()),
                    Span::raw(" starts one; "),
                    Span::styled("c", key_style()),
                    Span::raw(" on a pattern copies it."),
                ]),
            ]),
            inner,
        );
        return;
    };
    let p = &it.pattern;
    // the cycle, drawn
    let arrow = || Span::styled(" ─▶ ", Style::default().fg(Color::Yellow));
    let mut cycle = vec![
        Span::styled(" every ", dim()),
        Span::raw(format_interval(p.default_interval_s)),
    ];
    for s in &p.skills {
        cycle.push(arrow());
        cycle.push(Span::styled(
            s.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    let checkers = p.effective_agents();
    let mut second = vec![Span::raw("   ")];
    if !checkers.is_empty() {
        second.push(Span::styled("checked by ", dim()));
        second.push(Span::styled(
            checkers.join(" + "),
            Style::default().fg(Color::Magenta),
        ));
        second.push(arrow());
    }
    second.push(Span::styled("writes ", dim()));
    second.push(Span::raw(p.state_file.clone()));
    second.push(Span::styled(
        format!(
            " · starts at {} · {} run(s), {}k tokens a day",
            p.week_one_level.as_str(),
            p.max_runs_per_day,
            p.max_tokens_per_day / 1000
        ),
        dim(),
    ));
    let mut lines = vec![Line::from(cycle), Line::from(second), Line::raw("")];

    const LW: usize = 15;
    let mut group = "";
    let mut cursor_line = 0;
    for (n, field) in LField::ALL.iter().copied().enumerate() {
        if field.group() != group {
            group = field.group();
            lines.push(Line::styled(
                format!(" {group}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let active = focused && n == st.field;
        if active {
            cursor_line = lines.len();
        }
        if let Some((ef, t)) = &st.edit
            && *ef == field
        {
            lines.extend(labeled(field.label(), LW, t, true, 3, w, ""));
            continue;
        }
        let value = st.value(field);
        let shown = match field.edits() {
            Edits::Cycle if active => format!("‹ {value} ›"),
            Edits::Pick => format!("{value}  ▸"),
            _ => value,
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<w$}", field.label(), w = LW - 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                truncate_chars(&shown, w.saturating_sub(LW + 1)),
                if active { sel() } else { Style::default() },
            ),
        ]));
        if active && let Some(d) = detail(st, field) {
            lines.push(Line::styled(
                truncate_chars(&format!("{}{d}", " ".repeat(LW)), w),
                dim(),
            ));
        }
    }
    let problems = st.problems();
    if !problems.is_empty() {
        lines.push(Line::raw(""));
        for pr in problems {
            lines.push(Line::styled(
                format!(" ✗ {pr}"),
                Style::default().fg(Color::Red),
            ));
        }
    }
    let visible = usize::from(inner.height);
    let start = cursor_line.saturating_sub(visible.saturating_sub(4));
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        inner,
    );
}

fn detail(st: &LoopBuilderState, f: LField) -> Option<String> {
    let p = &st.current()?.pattern;
    Some(match f {
        LField::Every => "e.g. 15m, 2h, 1d; at least 5m".into(),
        LField::Level => "what a new loop of this pattern may do in its first week".into(),
        LField::Skills => st
            .skills
            .iter()
            .find(|s| Some(&s.0) == p.skills.first())
            .map(|s| format!("triage: {}", s.1))
            .unwrap_or_else(|| "the first skill is the triage; loop-rules must be listed".into()),
        LField::Checkers => "empty: loop-verifier alone when Verifier is yes".into(),
        LField::Verifier => "changes go to loop-verifier before they are proposed".into(),
        LField::Breaker => "keeps loop-ledger.json and stops a loop that keeps failing".into(),
        LField::Gates => "comma-separated: what always goes to a human".into(),
        LField::Model | LField::CheckerModel => {
            "a loop copies it when registered; empty leaves the profile's".into()
        }
        LField::StateFile => "the file the loop keeps in the workspace (guard-permitted)".into(),
        LField::EarlyExit => "an empty watch list must end under 5k tokens".into(),
        LField::Priority => "lower runs first when several loops are due".into(),
        _ => return None,
    })
}
