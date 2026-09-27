//! Drawing the agent editor (`crate::app::agent_editor`): the header, the
//! tabs Who · What · When · Limits · Review, the tab's fields on the left
//! and what they mean on the right. A flow's Who, What and Review are the
//! flow builder's own screens (`super::flow`).

use super::flow::{dim, head, hints, key_style, sel};
use super::{
    centered, dir_picker_lines, draw_confirm, pane_border, text_area_lines, truncate_chars,
};
use crate::app::agent_editor::{
    AgentEditorState, Body, EdField, PersonaBody, Tab, TaskField, TaskKind,
};
use crate::app::flow_builder::{AgentField, EFFORTS, FlowBuilderState, TOOL_NAMES};
use crate::app::loops::{LoopDialogState, LoopField};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};

pub fn draw(f: &mut Frame, st: &AgentEditorState) {
    // the last row stays the status bar, where notices show
    let full = f.area();
    let area = Rect::new(full.x, full.y, full.width, full.height.saturating_sub(1));
    f.render_widget(Clear, area);
    let [top, tabs, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    match &st.body {
        Body::Flow(fl) => super::flow::draw_header(f, top, fl),
        _ => draw_header(f, top, st),
    }
    f.render_widget(Paragraph::new(tab_bar(st.tab)), tabs);
    match &st.body {
        Body::Flow(fl) if !matches!(st.tab, Tab::When | Tab::Limits) => {
            super::flow::draw_body(f, body, fl)
        }
        Body::Flow(fl) => {
            let [pane, foot] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(body);
            draw_flow_panel(f, pane, st.tab, fl);
            f.render_widget(Paragraph::new(super::flow::footer(fl)), foot);
            super::flow::draw_overlay(f, fl);
        }
        _ => {
            let [panes, foot] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(body);
            draw_panes(f, panes, st);
            f.render_widget(Paragraph::new(footer(st)), foot);
        }
    }
    if st.confirm_discard {
        draw_confirm(
            f,
            "Leave the editor? Changes since the last save are lost. [y/n]",
        );
    }
}

fn tab_bar(tab: Tab) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, t) in Tab::ALL.iter().enumerate() {
        let text = format!(" {} {} ", i + 1, t.label());
        spans.push(if *t == tab {
            Span::styled(
                text,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(text, dim())
        });
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn draw_header(f: &mut Frame, area: Rect, st: &AgentEditorState) {
    let (about, status) = match &st.body {
        Body::Scheduled(d) => (
            d.pattern().map(|p| p.goal.clone()).unwrap_or_default(),
            match d.validate() {
                Ok(_) => Span::styled("✓ ready", Style::default().fg(Color::Green)),
                Err(e) => Span::styled(format!("! {e}"), Style::default().fg(Color::Red)),
            },
        ),
        Body::Persona(p) => (
            p.form.purpose.text.clone(),
            if crate::agents::is_valid_name(p.form.name.text.trim()) {
                Span::styled("✓ ready", Style::default().fg(Color::Green))
            } else {
                Span::styled(
                    "! a name: a-z, 0-9, - and _",
                    Style::default().fg(Color::Red),
                )
            },
        ),
        Body::Flow(_) => (String::new(), Span::raw("")),
    };
    let right = format!(
        "{}{}  ",
        st.kind(),
        if st.dirty { " · unsaved" } else { "" }
    );
    let left = format!(" ◆ {}  {}", st.name(), about);
    let room = usize::from(area.width)
        .saturating_sub(right.chars().count() + status.width() + 2)
        .max(10);
    let left = truncate_chars(&left, room);
    let pad = usize::from(area.width)
        .saturating_sub(left.chars().count() + right.chars().count() + status.width());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, head()),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, dim()),
            status,
        ])),
        area,
    );
}

fn footer(st: &AgentEditorState) -> Line<'static> {
    use crate::keymap::{Verb, ctrl_label, label, newline_label};
    let editor = ctrl_label('o');
    if st.editing {
        let mut v = vec![("↩", "done"), ("Esc", "done")];
        if matches!(st.body, Body::Persona(_)) {
            v.push((newline_label(), "new line"));
            v.push((&editor, "editor"));
        }
        return hints(&v);
    }
    let mut v: Vec<(&str, &str)> = vec![("1-5", "tabs"), ("↑↓", "fields")];
    match st.current() {
        Some(f) if f.is_text() => v.push(("↩", "type")),
        Some(EdField::Agent(AgentField::Tools)) => {
            v.push(("←→", "tool"));
            v.push(("Space", "on / off"));
        }
        Some(_) => v.push(("←→", "change")),
        None if st.tab == Tab::Review => v.push(("↩", "save")),
        None => {}
    }
    v.push((label(Verb::Save), "save"));
    match &st.body {
        Body::Scheduled(d) => {
            v.push((label(Verb::Run), "save and run now"));
            if d.editing.is_some() {
                v.push((&editor, "agent file"));
            }
            if st.tab == Tab::What {
                v.push((label(Verb::Edit), "edit the task"));
            }
        }
        Body::Persona(p) if p.original.is_some() => v.push((&editor, "file in $EDITOR")),
        _ => {}
    }
    v.push(("Esc", "back"));
    hints(&v)
}

fn draw_panes(f: &mut Frame, area: Rect, st: &AgentEditorState) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Min(0)]).areas(area);
    let title = match st.tab {
        Tab::Who => " Who runs it ",
        Tab::What => " What it does ",
        Tab::When => " When it runs ",
        Tab::Limits => " What it may change ",
        Tab::Review => " Review ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(true))
        .title(Span::styled(title, key_style()));
    let inner = block.inner(left);
    f.render_widget(block, left);
    let width = usize::from(inner.width);
    let lines = match &st.body {
        Body::Scheduled(d) => scheduled_fields(st, d, width, inner.height),
        Body::Persona(p) => persona_fields(st, p, width, inner.height),
        Body::Flow(_) => Vec::new(),
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    let (rtitle, rlines) = match &st.body {
        Body::Scheduled(d) => scheduled_facts(st, d),
        Body::Persona(p) => persona_facts(st, p),
        Body::Flow(_) => (String::new(), Vec::new()),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(Span::styled(format!(" {rtitle} "), head()))
        .padding(Padding::horizontal(1));
    f.render_widget(
        Paragraph::new(rlines)
            .wrap(Wrap { trim: false })
            .block(block),
        right,
    );
    if let Some(e) = &st.error {
        let w = (e.chars().count() as u16 + 4).min(area.width.saturating_sub(4));
        let r = centered(area, w, 3);
        let r = Rect::new(r.x, area.bottom().saturating_sub(3), r.width, 3);
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(Span::styled(e.clone(), Style::default().fg(Color::Red))).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            ),
            r,
        );
    }
}

/// One `label  value` row; the cursor's row stands out.
fn row(label: &str, value: String, on: bool, typing: bool) -> Line<'static> {
    let value = if typing { format!("{value}▏") } else { value };
    Line::from(vec![
        Span::styled(
            format!(" {label:<16}"),
            if on {
                Style::default().fg(Color::Yellow)
            } else {
                dim()
            },
        ),
        Span::styled(
            value,
            if on && !typing {
                sel()
            } else {
                Style::default()
            },
        ),
    ])
}

fn note(text: impl Into<String>) -> Line<'static> {
    Line::styled(format!(" {:<16}{}", "", text.into()), dim())
}

fn or_blank(v: &str, blank: &str) -> String {
    if v.trim().is_empty() {
        blank.to_string()
    } else {
        v.to_string()
    }
}

fn scheduled_fields(
    st: &AgentEditorState,
    d: &LoopDialogState,
    width: usize,
    height: u16,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::raw("")];
    if st.tab == Tab::Review {
        return scheduled_review(d);
    }
    for (i, field) in st.fields().into_iter().enumerate() {
        let on = i == st.field;
        let typing = on && st.editing;
        let EdField::Loop(lf) = field else { continue };
        match lf {
            LoopField::Workspace if typing => {
                let max = usize::from(height).saturating_sub(10).clamp(1, 8);
                out.extend(dir_picker_lines(
                    &format!(" {:<16}", field.label()),
                    &d.workspace,
                    &d.dir_picker,
                    true,
                    max,
                ));
            }
            LoopField::Workspace => {
                out.push(row(field.label(), d.workspace.clone(), on, false));
                out.push(note(
                    "a git repository: an agent that edits works in a worktree",
                ));
            }
            LoopField::Pattern => {
                let p = d.pattern();
                out.push(row(
                    field.label(),
                    p.map(|p| format!("{} — {}", p.id, p.name))
                        .unwrap_or_default(),
                    on,
                    false,
                ));
                if let Some(p) = p {
                    out.push(Line::raw(""));
                    for l in textwrap(&p.goal, width.saturating_sub(18)) {
                        out.push(note(l));
                    }
                    out.push(note(format!(
                        "risk {} · cost {} · ⟳ {}",
                        p.risk,
                        p.token_cost,
                        crate::loops::format_interval(p.default_interval_s)
                    )));
                }
            }
            LoopField::Profile => {
                let v = if d.no_profiles {
                    "no Claude Code or Codex profile in profiles.toml".to_string()
                } else {
                    d.profiles
                        .get(d.profile_idx)
                        .map(|(n, h)| format!("{n} ({})", h.as_str()))
                        .unwrap_or_default()
                };
                out.push(row(field.label(), v, on, false));
                out.push(note("the harness profile the runs launch with"));
            }
            LoopField::Model => {
                out.push(row(
                    field.label(),
                    if typing {
                        d.model.clone()
                    } else {
                        or_blank(&d.model, "the profile's")
                    },
                    on,
                    typing,
                ));
            }
            LoopField::VerifierModel => {
                let verifier = d.pattern().is_some_and(|p| p.verifier);
                out.push(row(
                    field.label(),
                    if typing {
                        d.verifier_model.clone()
                    } else if !verifier {
                        "no verifier for this task".into()
                    } else {
                        format!(
                            "loop-verifier · {}",
                            or_blank(&d.verifier_model, "the run's model")
                        )
                    },
                    on,
                    typing,
                ));
            }
            LoopField::Every => {
                out.push(row(field.label(), d.every.clone(), on, typing));
                out.push(note("15m, 2h, 1d · at least 5m"));
            }
            LoopField::Scaffold => {
                out.push(row(
                    field.label(),
                    if d.scaffold {
                        "[x] write missing skills and contract files".into()
                    } else {
                        "[ ] leave the workspace as it is".into()
                    },
                    on,
                    false,
                ));
                out.push(note("never overwrites a file"));
            }
            LoopField::Level => {
                out.push(row(field.label(), d.level.short().to_string(), on, false));
                let i = usize::from(d.level.edits());
                out.push(match &d.level_notes[i] {
                    Some(n) => Line::styled(
                        format!(" {:<16}✗ {n}", ""),
                        Style::default().fg(Color::Yellow),
                    ),
                    None => note(format!("a run {}", d.level.label())),
                });
            }
            LoopField::MaxRuns => out.push(row(field.label(), d.max_runs.clone(), on, typing)),
            LoopField::MaxTokens => out.push(row(field.label(), d.max_tokens.clone(), on, typing)),
            LoopField::MaxCost => out.push(row(
                field.label(),
                if typing {
                    d.max_cost.clone()
                } else {
                    or_blank(&d.max_cost, "no cap")
                },
                on,
                typing,
            )),
        }
        out.push(Line::raw(""));
    }
    out
}

fn scheduled_review(d: &LoopDialogState) -> Vec<Line<'static>> {
    let mut out = vec![Line::raw("")];
    let p = d.pattern();
    let task = p.map(|p| p.id.clone()).unwrap_or_default();
    let who = d.profile_name();
    out.push(Line::from(vec![
        Span::raw(" Runs "),
        Span::styled(task, head()),
        Span::raw(format!(
            " every {} in {}",
            or_blank(&d.every, "?"),
            or_blank(&d.workspace, "?")
        )),
    ]));
    out.push(Line::raw(format!(
        " on {} · model {}",
        or_blank(&who, "no profile"),
        or_blank(&d.model, "the profile's")
    )));
    out.push(Line::raw(format!(" It {}.", d.level.label())));
    out.push(Line::raw(format!(
        " At most {} runs and {} tokens a day{}.",
        d.max_runs,
        d.max_tokens,
        if d.max_cost.trim().is_empty() {
            String::new()
        } else {
            format!(", ${} a run", d.max_cost.trim())
        }
    )));
    out.push(Line::raw(""));
    out.push(match d.validate() {
        Ok(_) => Line::styled(
            " ✓ ready: ↩ or s saves it",
            Style::default().fg(Color::Green),
        ),
        Err(e) => Line::styled(format!(" ! {e}"), Style::default().fg(Color::Red)),
    });
    out
}

fn scheduled_facts(st: &AgentEditorState, d: &LoopDialogState) -> (String, Vec<Line<'static>>) {
    let l = |s: &str| Line::raw(s.to_string());
    let gap = || Line::raw("");
    match st.tab {
        Tab::Who => (
            "Who".into(),
            vec![
                gap(),
                l(
                    "A scheduled agent runs one task with a harness profile. A blank model keeps the profile's own.",
                ),
                gap(),
                l(
                    "A task that checks its work asks the loop-verifier sub-agent; its model is set here (Claude Code only).",
                ),
                gap(),
                Line::styled(
                    " Antigravity: not supported for loops yet (docs/loops.md)",
                    dim(),
                ),
            ],
        ),
        Tab::What => (
            "The task".into(),
            vec![
                gap(),
                l(
                    "←→ picks another task from the library; e edits the task itself: its goal, prompt and skills.",
                ),
                gap(),
                l(
                    "A flow of several steps runs when you start it: only a one-task agent runs on a schedule for now.",
                ),
            ],
        ),
        Tab::When => (
            "When".into(),
            vec![
                gap(),
                l(
                    "It runs every interval while it is not paused: p in the Agents list pauses it, K pauses them all.",
                ),
                gap(),
                l("It keeps notes of what it saw last time, so a quiet run ends early and cheap."),
            ],
        ),
        Tab::Limits => (
            "Budget and brakes".into(),
            vec![
                gap(),
                l(
                    "Without editing, it reads and reports: it writes its notes and its answer, nothing else.",
                ),
                gap(),
                l(
                    "With editing, it works in its own worktree, and the change waits in the inbox until you apply it.",
                ),
                gap(),
                l(
                    "Nothing pushes, merges or changes your tree without you. The breaker pauses it after repeated failures; gate.yaml lists the paths it never touches.",
                ),
                gap(),
                Line::styled(format!("{} · advice, never a gate", d.audit_note), dim()),
            ],
        ),
        Tab::Review => (
            "Readiness".into(),
            vec![gap(), Line::styled(d.audit_note.clone(), dim())],
        ),
    }
}

fn persona_fields(
    st: &AgentEditorState,
    p: &PersonaBody,
    width: usize,
    height: u16,
) -> Vec<Line<'static>> {
    let form = &p.form;
    let mut out = vec![Line::raw("")];
    if st.tab == Tab::Review {
        match p.render() {
            Ok(text) => out.extend(text.lines().map(|l| Line::raw(format!(" {l}")))),
            Err(e) => out.push(Line::styled(
                format!(" ! {e}"),
                Style::default().fg(Color::Red),
            )),
        }
        return out;
    }
    if p.original.is_some() && st.tab == Tab::Who {
        out.push(row("Name", form.name.text.clone(), false, false));
        out.push(note("a saved agent keeps its name"));
        out.push(Line::raw(""));
    }
    for (i, field) in st.fields().into_iter().enumerate() {
        let on = i == st.field;
        let typing = on && st.editing;
        if let EdField::Task(tf) = field {
            out.extend(task_field(p, tf, field, on, typing, width));
            out.push(Line::raw(""));
            continue;
        }
        let EdField::Agent(af) = field else { continue };
        match af {
            AgentField::Instructions => {
                let rows = usize::from(height).saturating_sub(out.len() + 3).max(3);
                out.extend(text_area_lines(
                    "",
                    &form.instructions,
                    typing,
                    rows,
                    width.saturating_sub(1),
                ));
                if !typing {
                    if let Some(first) = out.get_mut(1) {
                        *first = Line::from(
                            std::iter::once(Span::styled(
                                " ",
                                if on { sel() } else { Style::default() },
                            ))
                            .chain(first.spans.clone())
                            .collect::<Vec<_>>(),
                        );
                    }
                    if form.instructions.is_empty() {
                        out.push(note("↩ types what it should do and how"));
                    }
                }
            }
            AgentField::Tools => {
                let mut spans = vec![Span::styled(
                    format!(" {:<16}", field.label()),
                    if on {
                        Style::default().fg(Color::Yellow)
                    } else {
                        dim()
                    },
                )];
                for (t, name) in TOOL_NAMES.iter().enumerate() {
                    let text = format!("[{}] {name}", if form.tools[t] { "x" } else { " " });
                    let style = if on && t == form.tool_cursor {
                        sel()
                    } else if form.tools[t] {
                        Style::default()
                    } else {
                        dim()
                    };
                    spans.push(Span::styled(text, style));
                    spans.push(Span::raw("  "));
                }
                out.push(Line::from(spans));
            }
            AgentField::EffortCodex | AgentField::EffortAgy => {
                let k = usize::from(af == AgentField::EffortAgy);
                let e = EFFORTS[form.efforts[k]];
                out.push(row(
                    field.label(),
                    or_blank(e, "the harness's default"),
                    on,
                    false,
                ));
            }
            _ => {
                let text = match af {
                    AgentField::Name => &form.name,
                    AgentField::Purpose => &form.purpose,
                    AgentField::Mcp => &form.mcp,
                    AgentField::ModelClaude => &form.models[0],
                    AgentField::ModelCodex => &form.models[1],
                    _ => &form.models[2],
                };
                let v = if typing {
                    text.text.clone()
                } else {
                    match af {
                        AgentField::Mcp => or_blank(&text.text, "none"),
                        AgentField::Name | AgentField::Purpose => text.text.clone(),
                        _ => or_blank(&text.text, "the harness's default"),
                    }
                };
                out.push(row(field.label(), v, on, typing));
            }
        }
        if af != AgentField::Instructions {
            out.push(Line::raw(""));
        }
    }
    out
}

/// A task or schedule field of the persona form.
fn task_field(
    p: &PersonaBody,
    tf: TaskField,
    field: EdField,
    on: bool,
    typing: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match tf {
        TaskField::Kind => {
            out.push(row(field.label(), p.task.label().to_string(), on, false));
            out.push(note(
                "←→ a prompt, a skill, or nothing: a persona steps use",
            ));
        }
        TaskField::Text if p.task == TaskKind::Prompt => {
            out.push(Line::styled(
                format!(" {}", "What it does, in words"),
                if on {
                    Style::default().fg(Color::Yellow)
                } else {
                    dim()
                },
            ));
            out.extend(text_area_lines(
                "",
                &p.task_text,
                typing,
                6,
                width.saturating_sub(1),
            ));
        }
        TaskField::Text => {
            out.push(row(
                "Skill",
                if typing {
                    p.task_text.text.clone()
                } else {
                    or_blank(&p.task_text.text, "↩ names the skill, e.g. heimdall")
                },
                on,
                typing,
            ));
        }
        TaskField::Runs => {
            out.push(row(
                field.label(),
                if p.scheduled {
                    "on a schedule".into()
                } else {
                    "when I start it".into()
                },
                on,
                false,
            ));
            out.push(note(match (p.task, p.scheduled) {
                (TaskKind::None, _) => "a persona runs when a flow step or a run uses it",
                (_, false) => "Enter on its row in the Agents list starts it in a session",
                (_, true) => "every interval, in a workspace, like any scheduled agent",
            }));
        }
        TaskField::Every => {
            out.push(row(field.label(), p.every.text.clone(), on, typing));
            out.push(note("15m, 2h, 1d · at least 5m"));
        }
        TaskField::Workspace => {
            out.push(row(field.label(), p.workspace.text.clone(), on, typing));
            out.push(note(
                "the folder it runs in; with edit files, in a worktree of it",
            ));
        }
        TaskField::Profile => {
            out.push(row(
                field.label(),
                p.profiles
                    .get(p.profile_idx)
                    .map(|(n, h)| format!("{n} ({h})"))
                    .unwrap_or_else(|| "no Claude Code or Codex profile".into()),
                on,
                false,
            ));
        }
    }
    out
}

fn persona_facts(st: &AgentEditorState, p: &PersonaBody) -> (String, Vec<Line<'static>>) {
    let l = |s: &str| Line::raw(s.to_string());
    let gap = || Line::raw("");
    let edits = p.form.tools[1];
    match st.tab {
        Tab::Who => (
            "Who".into(),
            vec![
                gap(),
                l(
                    "A persona: instructions, tools and a model that flow steps and runs use, on any harness.",
                ),
                gap(),
                l("A model per harness; blank keeps the harness's own."),
            ],
        ),
        Tab::What => (
            "What".into(),
            vec![
                gap(),
                l(
                    "The instructions become the system prompt (Claude Code, Antigravity) or the developer instructions (Codex).",
                ),
                gap(),
                l(&format!(
                    "{} types a new line; ↩ ends typing.",
                    crate::keymap::newline_label()
                )),
            ],
        ),
        Tab::When => (
            "When".into(),
            vec![
                gap(),
                l(
                    "On demand: Enter on its row starts a session with its task as the first message; r does the same for a persona.",
                ),
                gap(),
                l(
                    "On a schedule: it runs every interval in its workspace like any scheduled agent, and its runs are in the runs view (E).",
                ),
            ],
        ),
        Tab::Limits => (
            "What it may change".into(),
            vec![
                gap(),
                Line::styled(
                    if edits {
                        "● With \"edit files\": it changes files where it runs; a scheduled run of it works in a worktree."
                    } else {
                        "● Without \"edit files\": it reads and reports; its answer is all it writes."
                    },
                    Style::default().fg(Color::Cyan),
                ),
                gap(),
                l("MCP servers: names, comma-separated."),
            ],
        ),
        Tab::Review => (
            "Saved as".into(),
            vec![
                gap(),
                l(&format!(
                    "{}.toml in the library; a built-in one is shadowed by your copy.",
                    p.form.name.text.trim()
                )),
            ],
        ),
    }
}

fn draw_flow_panel(f: &mut Frame, area: Rect, tab: Tab, fl: &FlowBuilderState) {
    let l = |s: String| Line::raw(format!(" {s}"));
    let lines = match tab {
        Tab::When => vec![
            Line::raw(""),
            l("It runs when you start it: Enter on it in the Agents list,".into()),
            l(format!("or agent-mux workflow run {}.", fl.draft.name())),
            Line::raw(""),
            Line::styled(
                " Only a one-task agent can run on a schedule for now.",
                dim(),
            ),
        ],
        _ => {
            let budget = fl.draft.flow_str("budget");
            let editors: Vec<String> = (0..fl.draft.len())
                .filter(|i| {
                    fl.skills.iter().any(|(id, _, w)| {
                        *w && fl.draft.does(*i)
                            == crate::workflows::builder::Does::Skill(id.clone())
                    })
                })
                .map(|i| fl.draft.id(i))
                .collect();
            vec![
                Line::raw(""),
                l(format!(
                    "Budget: {}",
                    if budget.is_empty() {
                        "none set (tab 1, Budget)".into()
                    } else {
                        budget
                    }
                )),
                Line::raw(""),
                l(if editors.is_empty() {
                    "No step edits files: the flow reads and reports.".into()
                } else {
                    format!("Steps that edit files: {}.", editors.join(", "))
                }),
                l("A step that edits works in an isolated worktree when its".into()),
                l("isolation says so (tab 2, the step's Isolation).".into()),
            ]
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(true))
        .title(Span::styled(
            if tab == Tab::When {
                " When it runs "
            } else {
                " Limits "
            },
            key_style(),
        ));
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn textwrap(text: &str, width: usize) -> Vec<String> {
    crate::app::text_area::wrap(text, width.max(10))
        .iter()
        .map(|r| text[r.start..r.end].to_string())
        .collect()
}
