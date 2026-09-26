//! Drawing the flow builder (`crate::app::flow_builder`): the Steps,
//! What passes and Review screens and their overlays. The layout follows
//! the design canvas "Agent Flow Builder": the flow as a chain on the
//! left, the selected step's fields on the right, plain words over TOML.

use super::{centered, draw_confirm, pane_border, text_area_lines, truncate_chars};
use crate::app::flow_builder::{
    AgentField, AgentForm, AgentPicker, EditTarget, Edits, ExPane, Field, FlowBuilderState, Focus,
    Overlay, Screen, TOOL_NAMES,
};
use crate::workflows::builder::{Does, FieldKind, Items, Role, Runs};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub(super) fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
pub(super) fn head() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}
pub(super) fn key_style() -> Style {
    Style::default().fg(Color::Cyan)
}
pub(super) fn agent_style() -> Style {
    Style::default().fg(Color::Magenta)
}
pub(super) fn sel() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// `[k] what` pairs as one footer line.
pub(super) fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (k, what) in pairs {
        spans.push(Span::styled(k.to_string(), key_style()));
        spans.push(Span::styled(format!(" {what}   "), dim()));
    }
    Line::from(spans)
}

/// How a step runs, in a few characters for the chain.
fn runs_short(st: &FlowBuilderState, i: usize) -> String {
    let d = &st.draft;
    match d.runs(i) {
        Runs::Once => "once".into(),
        Runs::Parallel => match d.items(i) {
            Items::List(v) if !v.is_empty() => format!("⇉ {} in parallel", v.len()),
            _ => "⇉ in parallel".into(),
        },
        Runs::EachThenCheck => match d.check_votes(i) {
            Some((v, _)) => format!("↓ each · {v} votes"),
            None => "↓ each".into(),
        },
        Runs::PickPath => "◆ pick a path".into(),
        Runs::BestOf => format!("★ best of {}", d.attempts(i)),
        Runs::UntilQuiet => "↺ until quiet".into(),
    }
}

/// What a step hands on, for the chain's connector line.
fn passes(st: &FlowBuilderState, i: usize) -> (String, String) {
    let d = &st.draft;
    match d.step(i).get("result").and_then(|v| v.as_str()) {
        Some(s) => {
            let fields = d.fields(s);
            let list = fields
                .iter()
                .find(|f| matches!(f.kind, FieldKind::ListOf(_) | FieldKind::ListOfText));
            match list {
                Some(f) => {
                    let inner = match &f.kind {
                        FieldKind::ListOf(g) => d
                            .fields(g)
                            .into_iter()
                            .map(|x| x.name)
                            .collect::<Vec<_>>()
                            .join(", "),
                        _ => "text".into(),
                    };
                    (format!("{}[ ]", f.name), inner)
                }
                None => (
                    s.to_string(),
                    fields
                        .into_iter()
                        .map(|x| x.name)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            }
        }
        None => ("text".into(), String::new()),
    }
}

pub fn draw(f: &mut Frame, st: &FlowBuilderState) {
    // the last row stays the status bar, where notices show
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
    match st.screen {
        Screen::Steps => draw_steps(f, body, st),
        Screen::Exchange => draw_exchange(f, body, st),
        Screen::Review => draw_review(f, body, st),
    }
    f.render_widget(Paragraph::new(footer(st)), foot);
    match &st.overlay {
        Some(Overlay::AddStep {
            name,
            choice,
            on_name,
        }) => draw_add_step(f, st, name, *choice, *on_name),
        Some(Overlay::Pick {
            title,
            options,
            selected,
            ..
        }) => draw_pick(f, title, options, *selected),
        Some(Overlay::Agent(p)) => draw_agents(f, st, p),
        Some(Overlay::Prompt { text }) => draw_prompt(f, st, text),
        Some(Overlay::Confirm { question, .. }) => draw_confirm(f, question),
        None => {}
    }
}

fn draw_header(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let title = match &st.origin {
        crate::app::flow_builder::Origin::New => "⚙ New flow",
        crate::app::flow_builder::Origin::Document(_) => match st.builtin {
            Some(true) => "⚙ Your copy of a built-in",
            Some(false) => "⚙ Built-in flow",
            None => "⚙ Flow",
        },
        crate::app::flow_builder::Origin::Plan(_) => "⚙ Planned flow",
    };
    let screen = match st.screen {
        Screen::Steps => String::new(),
        Screen::Exchange => " › What passes between steps".into(),
        Screen::Review => " › Review".into(),
    };
    let problems = st.problems();
    let status = if st.draft.is_empty() {
        Span::styled("no steps yet", dim())
    } else if problems == 0 {
        Span::styled("✓ ready to run", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            format!("! {problems} to fix"),
            Style::default().fg(Color::Red),
        )
    };
    let right = format!(
        "{} step(s) · {}{}  ",
        st.draft.len(),
        if st.estimate.is_empty() {
            "—"
        } else {
            &st.estimate
        },
        if st.dirty { " · unsaved" } else { "" }
    );
    let left_text = format!(
        " {title}  {}{screen}  {}",
        st.draft.name(),
        st.draft.flow_str("description")
    );
    let room = usize::from(area.width)
        .saturating_sub(right.chars().count() + 18)
        .max(10);
    let line = Line::from(vec![
        Span::styled(truncate_chars(&left_text, room), head()),
        Span::raw(
            " ".repeat(
                usize::from(area.width)
                    .saturating_sub(truncate_chars(&left_text, room).chars().count())
                    .saturating_sub(right.chars().count() + 16),
            ),
        ),
        Span::styled(right, dim()),
        status,
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn footer(st: &FlowBuilderState) -> Line<'static> {
    if st.edit.is_some() {
        return hints(&[("Enter", "done"), ("Esc", "cancel"), ("Ctrl+E", "editor")]);
    }
    match st.screen {
        Screen::Steps => match st.focus {
            Focus::List => hints(&[
                ("↑↓", "steps"),
                ("→", "fields"),
                ("a", "add"),
                ("J K", "move"),
                ("d", "delete"),
                ("w", "what passes"),
                ("g", "who runs it"),
                ("v", "review"),
                ("Esc", "close"),
            ]),
            Focus::Fields => {
                let f = st.current_field();
                let what = match f.map(Field::edits) {
                    Some(Edits::Cycle) => ("←→", "change"),
                    Some(Edits::Pick) => ("Enter", "choose"),
                    Some(Edits::Jump) => ("Enter", "edit the shape"),
                    _ => ("Enter", "type"),
                };
                hints(&[
                    ("↑↓", "fields"),
                    what,
                    ("a", "add step"),
                    ("w", "what passes"),
                    ("g", "who runs it"),
                    ("v", "review"),
                    ("Esc", "steps"),
                ])
            }
        },
        Screen::Exchange => match st.ex_pane {
            ExPane::Strip => hints(&[("←→", "steps"), ("Tab", "gives / gets"), ("Esc", "back")]),
            ExPane::Gives => hints(&[
                ("↑↓", "fields"),
                ("+", "add"),
                ("←→", "kind"),
                ("r", "required"),
                ("Enter", "open / values / rename"),
                ("d", "delete"),
                ("[ ]", "step"),
                ("Tab", "gets"),
                ("Esc", "back"),
            ]),
            ExPane::Gets => hints(&[
                ("↑↓", "choose"),
                ("Enter", "read it"),
                ("f", "keep only"),
                ("u", "drop repeats"),
                ("[ ]", "step"),
                ("Tab", "strip"),
                ("Esc", "back"),
            ]),
        },
        Screen::Review if st.builtin == Some(true) => hints(&[
            ("Enter", "save and run"),
            ("s", "save"),
            ("R", "restore the built-in"),
            ("e", "open in $EDITOR"),
            ("Esc", "steps"),
        ]),
        Screen::Review => hints(&[
            ("Enter", "save and run"),
            ("s", "save"),
            ("e", "open in $EDITOR"),
            ("↑↓ PgUp PgDn", "scroll"),
            ("w", "what passes"),
            ("Esc", "steps"),
        ]),
    }
}

// ---- Steps ----------------------------------------------------------------------

fn draw_steps(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let left_w = (area.width * 2 / 5).clamp(34, 64);
    let [left, right] =
        Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(area);
    draw_chain(f, left, st);
    draw_fields(f, right, st);
}

fn draw_chain(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let focused = st.focus == Focus::List;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(" Steps ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0;
    let row_style = |on: bool| {
        if on && focused {
            sel()
        } else if on {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    if st.selected == 0 {
        selected_line = 0;
    }
    lines.push(Line::styled(
        truncate_chars(" ⚙ flow settings", width),
        row_style(st.selected == 0),
    ));
    lines.push(Line::raw(""));
    let output = st.draft.output();
    for i in 0..st.draft.len() {
        let on = st.selected == i + 1;
        if on {
            selected_line = lines.len();
        }
        let id = st.draft.id(i);
        let who = st
            .draft
            .agent(i, Role::Step)
            .or_else(|| st.draft.agent(i, Role::Voters))
            .map(|a| format!("as {a}"))
            .unwrap_or_default();
        let runs = runs_short(st, i);
        let text = format!(
            " {:<2} {:<13} {:<18} ",
            i + 1,
            truncate_chars(&id, 13),
            runs
        );
        let mut spans = vec![Span::styled(text, row_style(on))];
        spans.push(Span::styled(
            who,
            if on && focused { sel() } else { agent_style() },
        ));
        lines.push(Line::from(spans));
        if st.draft.runs(i) == Runs::Parallel
            && let Items::List(v) = st.draft.items(i)
        {
            let n = v.len();
            for (k, item) in v.iter().take(4).enumerate() {
                let branch = if k + 1 == n.min(4) {
                    "└─"
                } else {
                    "├─"
                };
                lines.push(Line::styled(
                    truncate_chars(&format!("      {branch} {item}"), width),
                    dim(),
                ));
            }
            if n > 4 {
                lines.push(Line::styled(format!("         … {} more", n - 4), dim()));
            }
        }
        if output.as_deref() == Some(id.as_str()) {
            lines.push(Line::styled(
                "      → the run's answer",
                Style::default().fg(Color::Green),
            ));
        } else {
            let (what, inner) = passes(st, i);
            lines.push(Line::from(vec![
                Span::styled("    │ passes ", dim()),
                Span::styled(what, Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!(" {}", truncate_chars(&inner, width.saturating_sub(30))),
                    dim(),
                ),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(" + add a step ", key_style()),
        Span::styled(" a", dim()),
    ]));
    // what the flow takes, at the foot
    let args = st.draft.args_text();
    let budget = st
        .draft
        .budget()
        .map(|b| format!("{}k tokens", b / 1000))
        .unwrap_or_else(|| "no budget".into());
    let foot = [
        Line::styled(
            truncate_chars(
                &format!(
                    " Arguments  {}",
                    if args.is_empty() { "none" } else { &args }
                ),
                width,
            ),
            dim(),
        ),
        Line::styled(format!(" Budget     {budget}"), dim()),
    ];
    let visible = usize::from(inner.height).saturating_sub(foot.len() + 1);
    let start = selected_line.saturating_sub(visible.saturating_sub(4));
    let body: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();
    let [list, rest] =
        Layout::vertical([Constraint::Length(visible as u16), Constraint::Min(0)]).areas(inner);
    f.render_widget(Paragraph::new(body), list);
    let mut tail = vec![Line::styled("─".repeat(width), dim())];
    tail.extend(foot);
    f.render_widget(Paragraph::new(tail), rest);
}

fn draw_fields(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let focused = st.focus == Focus::Fields;
    let title = match st.step_index() {
        Some(i) => format!(" Step {} · {} ", i + 1, st.draft.id(i)),
        None => " The flow ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(Span::styled(title, head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    const LABEL: usize = 14;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line = 0;
    for (n, field) in st.fields().into_iter().enumerate() {
        let active = focused && n == st.field;
        if active {
            cursor_line = lines.len();
        }
        let label = Span::styled(
            format!(" {:<w$}", field.label(), w = LABEL - 1),
            Style::default().fg(Color::Yellow),
        );
        // an inline edit of this field
        if let Some(e) = &st.edit
            && e.target == EditTarget::Field(field)
            && n == st.field
        {
            let rows = text_area_lines(field.label(), &e.text, true, 4, width.saturating_sub(1));
            for l in rows {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(l.spans);
                lines.push(Line::from(spans));
            }
            lines.push(Line::raw(""));
            continue;
        }
        if field == Field::Runs {
            let cur = st.step_index().map(|i| st.draft.runs(i));
            let mut spans = vec![label];
            for r in Runs::ALL {
                let on = Some(r) == cur;
                let style = if on && active {
                    sel().fg(Color::Cyan)
                } else if on {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    dim()
                };
                spans.push(Span::styled(format!(" {} ", chip(r)), style));
                spans.push(Span::raw(" "));
            }
            lines.push(Line::from(spans));
            if let Some(r) = cur {
                lines.push(Line::styled(
                    truncate_chars(
                        &format!("{}{}: {}", " ".repeat(LABEL), r.title(), r.blurb()),
                        width,
                    ),
                    dim(),
                ));
            }
            lines.push(Line::raw(""));
            continue;
        }
        let value = st.value(field);
        let value_text = match field.edits() {
            Edits::Cycle if active => format!("‹ {value} ›"),
            Edits::Pick | Edits::Jump => format!("{value}  ▸"),
            _ => value,
        };
        let vstyle = if active {
            sel()
        } else if matches!(field, Field::Who | Field::VotersWho | Field::JudgeWho)
            && !value_text.starts_with("no agent")
        {
            agent_style()
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            label,
            Span::styled(
                truncate_chars(&value_text, width.saturating_sub(LABEL + 1)),
                vstyle,
            ),
        ]));
        // a second line where it helps
        let detail = detail_of(st, field);
        if let Some(d) = detail {
            lines.push(Line::styled(
                truncate_chars(&format!("{}{d}", " ".repeat(LABEL)), width),
                dim(),
            ));
        }
    }
    if st.step_index().is_none() && st.draft.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" Next: ", head()),
            Span::raw("press "),
            Span::styled("a", key_style()),
            Span::raw(
                " to add the first step, then say how it runs, what it does and who runs it.",
            ),
        ]));
    }
    let visible = usize::from(inner.height);
    let start = cursor_line.saturating_sub(visible.saturating_sub(3));
    let body: Vec<Line> = lines.into_iter().skip(start).collect();
    f.render_widget(Paragraph::new(body), inner);
}

/// How a shape reads as a chip on the Runs row.
fn chip(r: Runs) -> &'static str {
    match r {
        Runs::Once => "once",
        Runs::Parallel => "in parallel",
        Runs::EachThenCheck => "each + check",
        Runs::PickPath => "pick a path",
        Runs::BestOf => "best of N",
        Runs::UntilQuiet => "until quiet",
    }
}

/// A text field under a label of `label_w` columns: one line when it is
/// not being typed in (or `placeholder` when empty), else up to `rows`
/// wrapped rows with the cursor.
pub(super) fn labeled(
    label: &str,
    label_w: usize,
    ta: &crate::app::text_area::TextArea,
    active: bool,
    rows: usize,
    width: usize,
    placeholder: &str,
) -> Vec<Line<'static>> {
    let lstyle = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    };
    let head = Span::styled(format!(" {label:<w$}", w = label_w - 1), lstyle);
    let content = width.saturating_sub(label_w).max(8);
    ta.width.set(content);
    if !active {
        let t = ta.preview(content);
        return vec![Line::from(vec![
            head,
            if t.is_empty() {
                Span::styled(placeholder.to_string(), dim())
            } else {
                Span::raw(t)
            },
        ])];
    }
    let visual = ta.rows();
    let (cur_row, cur_col) = ta.cursor_at(&visual);
    let rows = rows.max(1);
    let start = cur_row
        .saturating_sub(rows - 1)
        .min(visual.len().saturating_sub(1));
    let end = (start + rows).min(visual.len());
    let mut out = Vec::new();
    for (n, row) in visual[start..end].iter().enumerate() {
        let i = start + n;
        let text = &ta.text[row.start..row.end];
        let mut spans = vec![if i == start {
            head.clone()
        } else {
            Span::raw(" ".repeat(label_w))
        }];
        if i == cur_row {
            let before: String = text.chars().take(cur_col).collect();
            let at: String = text.chars().skip(cur_col).take(1).collect();
            let after: String = text.chars().skip(cur_col + 1).collect();
            spans.push(Span::raw(before));
            spans.push(Span::styled(
                if at.is_empty() { " ".to_string() } else { at },
                sel(),
            ));
            spans.push(Span::raw(after));
        } else {
            spans.push(Span::raw(text.to_string()));
        }
        out.push(Line::from(spans));
    }
    out
}

/// The dim line under a field: what the value means.
fn detail_of(st: &FlowBuilderState, field: Field) -> Option<String> {
    let i = st.step_index();
    let d = &st.draft;
    match (field, i) {
        (Field::Does, Some(i)) => match d.does(i) {
            Does::Skill(k) => st
                .skills
                .iter()
                .find(|s| s.0 == k)
                .map(|s| truncate_chars(&s.1, 110)),
            Does::Nothing if d.runs(i) == Runs::EachThenCheck => {
                Some("optional here: the votes alone can decide".into())
            }
            _ => None,
        },
        (Field::Who, Some(i)) => d.agent(i, Role::Step).map(|a| agent_line(st, &a)),
        (Field::VotersWho, Some(i)) => d
            .agent(i, Role::Voters)
            .map(|a| agent_line(st, &a))
            .or(Some("voters do not inherit the step's agent".into())),
        (Field::JudgeWho, Some(i)) => d.agent(i, Role::Judge).map(|a| agent_line(st, &a)),
        (Field::Gets, Some(_)) => Some("handed to the session as its inputs".into()),
        (Field::Items, Some(_)) => Some("one session per item".into()),
        (Field::Filter, Some(_)) => Some("e.g. severity in [high, medium]; empty keeps all".into()),
        (Field::Paths, Some(_)) => Some("label: step step; label: step".into()),
        (Field::RepeatBy, Some(_)) => Some("fields that make two items the same".into()),
        (Field::FlowArgs, None) => Some("name=default, other*  (* is required)".into()),
        (Field::FlowOutput, None) => Some("the step whose answer the run returns".into()),
        _ => None,
    }
}

fn agent_line(st: &FlowBuilderState, name: &str) -> String {
    match st.catalog.get(name) {
        Some(a) => format!("{} · tools {}", a.description, a.tools_label()),
        None => "not found: g creates it".into(),
    }
}

// ---- What passes ------------------------------------------------------------------

fn draw_exchange(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let [strip, bottom] = Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(area);
    draw_strip(f, strip, st);
    let right_w = (area.width * 2 / 5).clamp(36, 70);
    let [gives, gets] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).areas(bottom);
    draw_gives(f, gives, st);
    draw_gets(f, gets, st);
}

fn draw_strip(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(st.ex_pane == ExPane::Strip))
        .title(Span::styled(" The flow ", head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let n = st.draft.len();
    if n == 0 {
        f.render_widget(Paragraph::new(Line::styled(" no steps yet", dim())), inner);
        return;
    }
    // as many cards as fit, keeping the selected one in view
    let card_w: u16 = 30;
    let arrow_w: u16 = 6;
    let fit = (((inner.width + arrow_w) / (card_w + arrow_w)) as usize).max(1);
    let sel_i = st.selected.saturating_sub(1);
    let start = sel_i.saturating_sub(fit - 1).min(n.saturating_sub(fit));
    let mut x = inner.x;
    for i in start..(start + fit).min(n) {
        let on = i == sel_i;
        let r = Rect::new(
            x,
            inner.y,
            card_w.min(inner.x + inner.width - x),
            inner.height,
        );
        let cb = Block::default().borders(Borders::ALL).border_style(if on {
            Style::default().fg(Color::Cyan)
        } else {
            dim()
        });
        let ci = cb.inner(r);
        f.render_widget(cb, r);
        let gets = st
            .draft
            .step(i)
            .get("input")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "—".into());
        let (gives, _) = passes(st, i);
        let who = st
            .draft
            .agent(i, Role::Step)
            .map(|a| format!(" · as {a}"))
            .unwrap_or_default();
        let w = usize::from(ci.width);
        let lines = vec![
            Line::styled(
                truncate_chars(&format!("{} {}", i + 1, st.draft.id(i)), w),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                truncate_chars(&format!("{}{who}", runs_short(st, i)), w),
                dim(),
            ),
            Line::from(vec![
                Span::styled("gets  ", dim()),
                Span::raw(truncate_chars(&gets, w.saturating_sub(6))),
            ]),
            Line::from(vec![
                Span::styled("gives ", dim()),
                Span::styled(
                    truncate_chars(&gives, w.saturating_sub(6)),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];
        f.render_widget(Paragraph::new(lines), ci);
        x += card_w;
        if i + 1 < (start + fit).min(n) && x + arrow_w <= inner.x + inner.width {
            let a = Rect::new(x, inner.y + inner.height / 2 - 1, arrow_w, 1);
            f.render_widget(
                Paragraph::new(Line::styled(" ──▶ ", Style::default().fg(Color::Yellow))),
                a,
            );
            x += arrow_w;
        }
    }
}

fn draw_gives(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let focused = st.ex_pane == ExPane::Gives;
    let id = st.step_index().map(|i| st.draft.id(i)).unwrap_or_default();
    let path = std::iter::once(id.clone())
        .chain(st.ex_groups.iter().cloned())
        .collect::<Vec<_>>()
        .join(" › ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(Span::styled(format!(" {path} gives "), head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    match st.ex_schema() {
        None => {
            lines.push(Line::styled(
                " This step answers in free text.",
                Style::default(),
            ));
            lines.push(Line::styled(
                " + adds a field and gives the answer a shape the next steps can read field by field.",
                dim(),
            ));
        }
        Some(schema) => {
            lines.push(Line::styled(
                if st.ex_groups.is_empty() {
                    " Each session ends its answer with this shape; agent-mux checks it and retries once."
                } else {
                    " Each item of the list has these fields."
                },
                dim(),
            ));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(
                    " {:<18}{:<18}{:<10}{}",
                    "field", "kind", "required", "values"
                ),
                dim(),
            ));
            let fields = st.draft.fields(&schema);
            for (n, fv) in fields.iter().enumerate() {
                let on = focused && n == st.ex_field;
                let values = match &fv.kind {
                    FieldKind::OneOf(v) => v.join(" · "),
                    FieldKind::ListOf(g) => format!("Enter opens {g}"),
                    _ => "—".into(),
                };
                let text = format!(
                    " {:<18}{:<18}{:<10}{}",
                    truncate_chars(&fv.name, 17),
                    fv.kind.label(),
                    if fv.required { "yes" } else { "no" },
                    values
                );
                lines.push(Line::styled(
                    truncate_chars(&text, width),
                    if on { sel() } else { Style::default() },
                ));
            }
            if fields.is_empty() {
                lines.push(Line::styled(" no fields yet", dim()));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(" + field", key_style()));
        }
    }
    if let Some(e) = &st.edit {
        let label = match &e.target {
            EditTarget::NewField(_) => Some("New field"),
            EditTarget::RenameField(..) => Some("Rename"),
            EditTarget::Values(..) => Some("Values"),
            _ => None,
        };
        if let Some(label) = label {
            lines.push(Line::raw(""));
            lines.extend(text_area_lines(
                label,
                &e.text,
                true,
                2,
                width.saturating_sub(1),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " A kind is text, whole number, number, yes/no, one of, a list of text or a list of items.",
        dim(),
    ));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_gets(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let focused = st.ex_pane == ExPane::Gets;
    let Some(i) = st.step_index() else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(focused))
        .title(Span::styled(format!(" {} gets ", st.draft.id(i)), head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let cur = st
        .draft
        .step(i)
        .get("input")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut lines = vec![
        Line::styled(" What this step reads from earlier steps.", dim()),
        Line::raw(""),
    ];
    let mut options: Vec<(Option<String>, String)> =
        vec![(None, "nothing from earlier steps".into())];
    options.extend(
        st.draft
            .sources(i)
            .into_iter()
            .map(|s| (Some(s.path), s.words)),
    );
    for (n, (path, words)) in options.iter().enumerate() {
        let chosen = *path == cur;
        let on = focused && n == st.ex_gets;
        let mark = if chosen { "(•)" } else { "( )" };
        lines.push(Line::styled(
            truncate_chars(&format!(" {mark} {words}"), width),
            if on {
                sel()
            } else if chosen {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
    }
    if st.draft.runs(i).has_items() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " Runs for each of",
            Style::default().fg(Color::Yellow),
        ));
        lines.push(Line::raw(truncate_chars(
            &format!(" {}", st.value(Field::Items)),
            width,
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(" Filters", Style::default().fg(Color::Yellow)));
    let keep = st.draft.step_str(i, "keep");
    let dedupe = st.draft.list_text(i, "dedupe_by");
    lines.push(Line::from(vec![
        Span::styled(" f ", key_style()),
        Span::raw("keep only "),
        Span::styled(
            if keep.is_empty() { "all".into() } else { keep },
            Style::default(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" u ", key_style()),
        Span::raw("drop repeats by "),
        Span::raw(if dedupe.is_empty() {
            "—".into()
        } else {
            dedupe
        }),
    ]));
    if let Some(e) = &st.edit
        && matches!(e.target, EditTarget::Filter | EditTarget::Dedupe)
    {
        lines.push(Line::raw(""));
        let label = if e.target == EditTarget::Filter {
            "Keep only"
        } else {
            "Same when"
        };
        lines.extend(text_area_lines(
            label,
            &e.text,
            true,
            2,
            width.saturating_sub(1),
        ));
    }
    if let Some(p) = &cur {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            truncate_chars(&format!(" written as input = \"{p}\""), width),
            dim(),
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- Review -------------------------------------------------------------------------

fn draw_review(f: &mut Frame, area: Rect, st: &FlowBuilderState) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Min(0)]).areas(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(true))
        .title(Span::styled(" In words ", head()));
    let inner = block.inner(left);
    f.render_widget(block, left);
    let mut lines: Vec<Line> = Vec::new();
    for (n, s) in st.draft.sentences().into_iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", n + 1), dim()),
            Span::raw(s),
        ]));
    }
    if st.draft.is_empty() {
        lines.push(Line::styled(" no steps yet", dim()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(" Checks", Style::default().fg(Color::Yellow)));
    for c in &st.checks {
        let (mark, style) = if c.ok {
            ("✓", Style::default().fg(Color::Green))
        } else if c.warning {
            ("!", Style::default().fg(Color::Yellow))
        } else {
            ("✗", Style::default().fg(Color::Red))
        };
        lines.push(Line::styled(format!(" {mark} {}", c.text), style));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(" Cost", Style::default().fg(Color::Yellow)));
    lines.push(Line::raw(format!(
        " {}",
        if st.estimate.is_empty() {
            "—"
        } else {
            &st.estimate
        }
    )));
    lines.push(Line::styled(
        format!(
            " {}",
            st.draft
                .budget()
                .map(|b| format!("budget {}k tokens · stops early when spent", b / 1000))
                .unwrap_or_else(|| "no budget: set one in the flow settings".into())
        ),
        dim(),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            " Enter  save and run ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw("   "),
        Span::styled("s", key_style()),
        Span::raw(" save to the library   "),
        Span::styled("e", key_style()),
        Span::raw(" open in $EDITOR"),
    ]));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    let doc = st.draft.to_toml();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(Span::styled(format!(" {}.toml ", st.draft.name()), head()));
    let inner = block.inner(right);
    f.render_widget(block, right);
    let total = doc.lines().count();
    let visible = usize::from(inner.height);
    let start = st.review_scroll.min(total.saturating_sub(visible));
    let lines: Vec<Line> = doc
        .lines()
        .skip(start)
        .take(visible)
        .map(toml_line)
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// A TOML line with its headers and strings coloured.
fn toml_line(line: &str) -> Line<'static> {
    let t = line.trim_start();
    if t.starts_with('[') {
        return Line::styled(line.to_string(), Style::default().fg(Color::Yellow));
    }
    if let Some((k, v)) = line.split_once(" = ") {
        let vstyle = if v.starts_with('"') {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        return Line::from(vec![
            Span::raw(format!("{k} = ")),
            Span::styled(v.to_string(), vstyle),
        ]);
    }
    Line::styled(line.to_string(), Style::default().fg(Color::Green))
}

// ---- overlays -----------------------------------------------------------------------

fn draw_add_step(
    f: &mut Frame,
    st: &FlowBuilderState,
    name: &crate::app::text_area::TextArea,
    choice: usize,
    on_name: bool,
) {
    let area = centered(f.area(), 104, 30);
    f.render_widget(Clear, area);
    let after = st
        .step_index()
        .map(|i| format!(" Add a step after {} ", st.draft.id(i)))
        .unwrap_or_else(|| " Add a step ".into());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(after, key_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [name_row, _, label, grid, _, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(21),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    let name_line = text_area_lines("Name", name, on_name, 1, usize::from(name_row.width));
    f.render_widget(Paragraph::new(name_line), name_row);
    f.render_widget(
        Paragraph::new(Line::styled(
            " How does it run?",
            Style::default().fg(Color::Yellow),
        )),
        label,
    );
    let rows = Layout::vertical([Constraint::Length(7); 3]).split(grid);
    for (n, runs) in Runs::ALL.iter().enumerate() {
        let row = rows[n / 2];
        let [a, b] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(row);
        let cell = if n % 2 == 0 { a } else { b };
        let on = n == choice;
        let cb = Block::default()
            .borders(Borders::ALL)
            .border_style(if on && !on_name {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if on {
                Style::default().fg(Color::Cyan)
            } else {
                dim()
            });
        let ci = cb.inner(cell);
        f.render_widget(cb, cell);
        let [pic, words] =
            Layout::horizontal([Constraint::Length(10), Constraint::Min(0)]).areas(ci);
        let dstyle = if on {
            Style::default().fg(Color::Cyan)
        } else {
            dim()
        };
        f.render_widget(
            Paragraph::new(
                runs.diagram()
                    .iter()
                    .map(|l| Line::styled(l.to_string(), dstyle))
                    .collect::<Vec<_>>(),
            ),
            pic,
        );
        f.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    runs.title().to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Line::styled(runs.blurb().to_string(), dim()),
            ])
            .wrap(Wrap { trim: true }),
            words,
        );
    }
    f.render_widget(
        Paragraph::new(hints(&[
            ("←↑↓→", "choose"),
            ("Enter", "add and edit"),
            ("Tab", "name"),
            ("Esc", "cancel"),
        ])),
        foot,
    );
}

fn draw_pick(
    f: &mut Frame,
    title: &str,
    options: &[crate::app::flow_builder::PickOption],
    selected: usize,
) {
    let h = (options.len() as u16 * 2 + 4).clamp(8, 30);
    let area = centered(f.area(), 96, h);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(format!(" {title} "), key_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = usize::from(inner.width);
    let mut lines = Vec::new();
    let mut sel_line = 0;
    for (n, o) in options.iter().enumerate() {
        if n == selected {
            sel_line = lines.len();
        }
        lines.push(Line::styled(
            truncate_chars(&format!(" {}", o.label), width),
            if n == selected {
                sel()
            } else {
                Style::default()
            },
        ));
        lines.push(Line::styled(
            truncate_chars(&format!("   {}", o.detail), width),
            dim(),
        ));
    }
    let visible = usize::from(inner.height).saturating_sub(1);
    let start = sel_line.saturating_sub(visible.saturating_sub(2));
    let mut body: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();
    body.push(hints(&[
        ("↑↓", "choose"),
        ("Enter", "use it"),
        ("Esc", "cancel"),
    ]));
    f.render_widget(Paragraph::new(body), inner);
}

fn draw_prompt(f: &mut Frame, st: &FlowBuilderState, text: &crate::app::text_area::TextArea) {
    let area = centered(f.area(), 100, 18);
    f.render_widget(Clear, area);
    let id = st.step_index().map(|i| st.draft.id(i)).unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(format!(" What {id} does "), key_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [help, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(Line::styled(
            " Say what the session does. {item}, {args.name} and {step} fill in; agent-mux adds how to read the context and answer.",
            dim(),
        ))
        .wrap(Wrap { trim: true }),
        help,
    );
    let lines = text_area_lines(
        "Prompt",
        text,
        true,
        usize::from(body.height),
        usize::from(body.width),
    );
    f.render_widget(Paragraph::new(lines), body);
    f.render_widget(
        Paragraph::new(hints(&[
            ("Enter", "done"),
            ("Alt+Enter", "new line"),
            ("Ctrl+E", "editor"),
            ("Esc", "cancel"),
        ])),
        foot,
    );
}

fn draw_agents(f: &mut Frame, st: &FlowBuilderState, p: &AgentPicker) {
    let area = centered(f.area(), 130, 34);
    f.render_widget(Clear, area);
    let id = st.step_index().map(|i| st.draft.id(i)).unwrap_or_default();
    let whose = match p.role {
        Role::Step => format!(" Who runs {id} "),
        Role::Voters => format!(" Who votes on each item of {id} "),
        Role::Judge => format!(" Who judges {id} "),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(whose, key_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [body, foot] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let [list, detail] =
        Layout::horizontal([Constraint::Length(40), Constraint::Min(0)]).areas(body);

    // the list
    let lb = Block::default()
        .borders(Borders::RIGHT)
        .border_style(dim())
        .title(Span::styled(" Agents ", head()));
    let li = lb.inner(list);
    f.render_widget(lb, list);
    let w = usize::from(li.width);
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0;
    for (n, o) in p.options.iter().enumerate() {
        let on = n == p.selected && p.form.is_none();
        if n == p.selected {
            sel_line = lines.len();
        }
        let (name, desc, src) = match o {
            None => (
                "no agent".to_string(),
                "the harness as it is".to_string(),
                String::new(),
            ),
            Some(a) => {
                let e = st.catalog.entry(a);
                (
                    a.clone(),
                    e.and_then(|e| e.spec.as_ref())
                        .map(|s| s.description.clone())
                        .unwrap_or_default(),
                    e.map(|e| format!(" · {}", e.source.label()))
                        .unwrap_or_default(),
                )
            }
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {name}"),
                if on {
                    sel()
                } else if o.is_some() {
                    agent_style()
                } else {
                    Style::default()
                },
            ),
            Span::styled(src, dim()),
        ]));
        lines.push(Line::styled(
            truncate_chars(&format!("   {desc}"), w),
            dim(),
        ));
    }
    let new_on = p.selected == p.new_index();
    if new_on {
        sel_line = lines.len();
    }
    lines.push(Line::styled(
        " + new agent",
        if new_on && p.form.is_none() {
            sel()
        } else {
            key_style()
        },
    ));
    lines.push(Line::styled("   write one for this role", dim()));
    if p.role != Role::Step {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            truncate_chars(" Voters and judges do not inherit the step's agent.", w),
            dim(),
        ));
    }
    let visible = usize::from(li.height);
    let start = sel_line.saturating_sub(visible.saturating_sub(3));
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        li,
    );

    // the detail: the selected agent, or the new-agent form
    let di = Rect::new(
        detail.x + 1,
        detail.y,
        detail.width.saturating_sub(1),
        detail.height,
    );
    match &p.form {
        Some(form) => draw_agent_form(f, di, st, p.role, form),
        None => {
            let mut d: Vec<Line> = Vec::new();
            match p.options.get(p.selected).cloned().flatten() {
                Some(a) => match st.catalog.get(&a) {
                    Some(spec) => {
                        d.push(Line::styled(
                            format!(" {}", spec.name),
                            agent_style().add_modifier(Modifier::BOLD),
                        ));
                        d.push(Line::raw(format!(" {}", spec.description)));
                        d.push(Line::raw(""));
                        d.push(Line::from(vec![
                            Span::styled(" tools  ", Style::default().fg(Color::Yellow)),
                            Span::raw(spec.tools_label()),
                        ]));
                        for h in crate::agents::HARNESSES {
                            if let Some(m) = spec.model_for(h) {
                                d.push(Line::from(vec![
                                    Span::styled(
                                        format!(" {h:<7}"),
                                        Style::default().fg(Color::Yellow),
                                    ),
                                    Span::raw(m),
                                ]));
                            }
                        }
                        d.push(Line::raw(""));
                        for l in spec.instructions.lines().take(14) {
                            d.push(Line::styled(format!(" {l}"), dim()));
                        }
                    }
                    None => d.push(Line::styled(
                        " does not load",
                        Style::default().fg(Color::Red),
                    )),
                },
                None if p.selected == p.new_index() => {
                    d.push(Line::raw(" Write a new agent for this role:"));
                    d.push(Line::styled(
                        " a name, what it is for, how it works, what it may use and its model.",
                        dim(),
                    ));
                }
                None => {
                    d.push(Line::raw(" No agent: the session is the harness as it is,"));
                    d.push(Line::styled(
                        " with the step's skill or prompt as its only guide.",
                        dim(),
                    ));
                }
            }
            f.render_widget(Paragraph::new(d).wrap(Wrap { trim: false }), di);
        }
    }
    let foot_line = if p.form.is_some() {
        hints(&[
            ("Tab ↑↓", "fields"),
            ("Space ←→", "toggle"),
            ("Alt+Enter", "new line"),
            ("Ctrl+E", "editor"),
            ("Ctrl+S", "save and use"),
            ("Esc", "back to the list"),
        ])
    } else {
        hints(&[
            ("↑↓", "choose"),
            ("Enter", "use it"),
            ("e", "edit it"),
            ("n", "new agent"),
            ("Esc", "cancel"),
        ])
    };
    f.render_widget(Paragraph::new(foot_line), foot);
}

fn draw_agent_form(f: &mut Frame, area: Rect, st: &FlowBuilderState, role: Role, form: &AgentForm) {
    let w = usize::from(area.width);
    let label = |t: &str, on: bool| {
        Span::styled(
            format!(" {t:<14}"),
            if on {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            },
        )
    };
    let mut lines: Vec<Line> = vec![Line::styled(
        " New agent",
        key_style().add_modifier(Modifier::BOLD),
    )];
    const LW: usize = 15;
    let text_row = |lines: &mut Vec<Line>,
                    name: &str,
                    field: AgentField,
                    ta: &crate::app::text_area::TextArea,
                    rows: usize,
                    placeholder: &str| {
        lines.extend(labeled(
            name,
            LW,
            ta,
            form.field == field,
            rows,
            w,
            placeholder,
        ));
    };
    text_row(
        &mut lines,
        "Name",
        AgentField::Name,
        &form.name,
        1,
        "a-kebab-name",
    );
    text_row(
        &mut lines,
        "For",
        AgentField::Purpose,
        &form.purpose,
        2,
        "one line: what it is for",
    );
    let instr_rows = if form.field == AgentField::Instructions {
        8
    } else {
        1
    };
    text_row(
        &mut lines,
        "Instructions",
        AgentField::Instructions,
        &form.instructions,
        instr_rows,
        "who it is and how it works: its system prompt",
    );
    // tools
    let on = form.field == AgentField::Tools;
    let mut spans = vec![label("May use", on)];
    for (n, t) in TOOL_NAMES.iter().enumerate() {
        let mark = if form.tools[n] { "[x]" } else { "[ ]" };
        let style = if on && n == form.tool_cursor {
            sel()
        } else if form.tools[n] {
            Style::default().fg(Color::Green)
        } else {
            dim()
        };
        spans.push(Span::styled(format!("{mark} {t}"), style));
        spans.push(Span::raw("  "));
    }
    lines.push(Line::from(spans));
    text_row(
        &mut lines,
        "MCP servers",
        AgentField::Mcp,
        &form.mcp,
        1,
        "none",
    );
    text_row(
        &mut lines,
        "Model claude",
        AgentField::ModelClaude,
        &form.models[0],
        1,
        "the profile's",
    );
    text_row(
        &mut lines,
        "Model codex",
        AgentField::ModelCodex,
        &form.models[1],
        1,
        "the profile's",
    );
    text_row(
        &mut lines,
        "Model agy",
        AgentField::ModelAgy,
        &form.models[2],
        1,
        "the profile's",
    );
    for (k, (name, field)) in [
        ("Effort codex", AgentField::EffortCodex),
        ("Effort agy", AgentField::EffortAgy),
    ]
    .into_iter()
    .enumerate()
    {
        let on = form.field == field;
        let v = form.effort(k);
        let v = if v.is_empty() { "—" } else { v };
        lines.push(Line::from(vec![
            label(name, on),
            Span::styled(
                if on {
                    format!("‹ {v} ›")
                } else {
                    v.to_string()
                },
                if on { sel() } else { Style::default() },
            ),
        ]));
    }
    let on = form.field == AgentField::SaveTo;
    let (a, b) = if form.to_workspace {
        ("(•)", "( )")
    } else {
        ("( )", "(•)")
    };
    lines.push(Line::from(vec![
        label("Save to", on),
        Span::styled(
            format!("{a} this workspace"),
            if on && form.to_workspace {
                sel()
            } else {
                Style::default()
            },
        ),
        Span::raw("   "),
        Span::styled(
            format!("{b} my library"),
            if on && !form.to_workspace {
                sel()
            } else {
                Style::default()
            },
        ),
    ]));
    lines.push(Line::styled(
        format!(
            "{}{}",
            " ".repeat(15),
            if form.to_workspace {
                ".agent-mux/agents in the flow's workspace, committed with the repository"
            } else {
                "~/.agent-mux/agents, for every workspace"
            }
        ),
        dim(),
    ));
    // does it fit the role?
    lines.push(Line::raw(""));
    let (read, edit) = (form.tools[0], form.tools[1]);
    let needs_edit = role == Role::Step
        && st.step_index().is_some_and(|i| {
            st.draft.step_str(i, "isolation") == "worktree"
                || matches!(st.draft.does(i), Does::Skill(k) if st.skills.iter().any(|s| s.0 == k && s.2))
        });
    if read {
        lines.push(Line::styled(
            " ✓ can read the context file",
            Style::default().fg(Color::Green),
        ));
    } else {
        lines.push(Line::styled(
            " ✗ needs read files: every session reads its context file",
            Style::default().fg(Color::Red),
        ));
    }
    if needs_edit && !edit {
        lines.push(Line::styled(
            " ✗ this step edits files: tick edit files",
            Style::default().fg(Color::Red),
        ));
    } else if needs_edit {
        lines.push(Line::styled(
            " ✓ can edit, as this step needs",
            Style::default().fg(Color::Green),
        ));
    }
    lines.push(Line::styled(
        " ! on agy the tool list is not enforced; codex has no per-tool list",
        Style::default().fg(Color::Yellow),
    ));
    if let Some(e) = &form.error {
        lines.push(Line::styled(
            format!(" ✗ {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}
