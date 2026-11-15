//! Drawing the Library screen (`crate::app::library`) in the Agents
//! layout, as the Swarm screen is drawn: the tree where the Agents list
//! is (`draw_tree`: headings, items, and the sessions and runs an item
//! launched), and the selected row in the main pane (`draw_pane`): a
//! session's terminal through the Agents screen's own pane
//! (`ui::draw_session_pane`), else the item's card in tabs. The harness
//! picker is drawn over the pane. The global status bar is the only
//! footer.

use super::flow::{dim, head};
use crate::app::library::{
    Drawn, Item, LibraryState, Picker, Row, Section, Verdict, coverage, tab_labels,
};
use crate::app::{App, Mode};
use crate::harness::Harness;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn verdict_style(v: &Verdict) -> Style {
    match v {
        Verdict::Ready => Style::default().fg(Color::Green),
        Verdict::Degraded(_) => Style::default().fg(Color::Yellow),
        Verdict::Blocked(_) => Style::default().fg(Color::Red),
        Verdict::NotInstalled => dim(),
    }
}

fn verdict_glyph(v: &Verdict) -> &'static str {
    match v {
        Verdict::Ready => "✓",
        Verdict::Degraded(_) => "~",
        Verdict::Blocked(_) => "✗",
        Verdict::NotInstalled => "·",
    }
}

fn item_style(item: &Item) -> Style {
    Style::default().fg(match item {
        Item::Flow(_) => Color::Cyan,
        Item::Skill(_) => Color::Yellow,
        Item::Persona(_) => Color::Magenta,
        Item::Loop(_) => Color::Cyan,
    })
}

pub fn item_name(app: &App, item: &Item) -> String {
    match item {
        Item::Flow(i) => app
            .workflow_list
            .get(*i)
            .map(|e| e.name.clone())
            .unwrap_or_default(),
        Item::Skill(i) => app
            .skills
            .get(*i)
            .map(|s| s.name.clone())
            .unwrap_or_default(),
        Item::Persona(n) => n.clone(),
        Item::Loop(i) => app
            .loop_registry
            .loops
            .get(*i)
            .map(|e| e.pattern.clone())
            .unwrap_or_default(),
    }
}

/// One tree row, before it is fitted to the width: the text on the left
/// in two styles (a glyph, then the name) and a label on the right.
struct TreeRow {
    lead: String,
    lead_style: Style,
    name: String,
    name_style: Style,
    right: String,
    right_style: Style,
}

fn tree_row(app: &App, st: &LibraryState, rows: &[Row], at: usize, now: Instant) -> TreeRow {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match &rows[at] {
        Row::Header(section) => {
            let count = rows[at + 1..]
                .iter()
                .take_while(|r| !r.is_header())
                .filter(|r| !r.is_child())
                .count();
            let folded = st.folded.contains(section);
            TreeRow {
                lead: format!("{} ", if folded { "▸" } else { "▾" }),
                lead_style: dim(),
                name: section.label().to_uppercase(),
                name_style: dim().add_modifier(Modifier::BOLD),
                // a folded section's items are not in the rows to count
                right: if folded {
                    String::new()
                } else {
                    count.to_string()
                },
                right_style: dim(),
            }
        }
        row @ (Row::Flow(_) | Row::Skill(_) | Row::Persona(_) | Row::Loop(_)) => {
            let Some(item) = Option::<Item>::from(row) else {
                unreachable!("an item row");
            };
            let live = app.library_live(&item);
            let (right, right_style) = if live > 0 {
                (format!("●{live}"), Style::default().fg(Color::Green))
            } else if let Item::Loop(i) = &item {
                match app.loop_registry.loops.get(*i) {
                    Some(e) if e.paused() => ("⏸".to_string(), dim()),
                    Some(e) => (e.harness.clone(), dim()),
                    None => (String::new(), dim()),
                }
            } else {
                // on how many harnesses it can start
                let (ok, total) = coverage(&app.library_verdicts_in(st, &item));
                (
                    format!("{ok}/{total}"),
                    if ok == 0 {
                        Style::default().fg(Color::Red)
                    } else {
                        dim()
                    },
                )
            };
            let glyph = match &item {
                Item::Skill(i) => app
                    .skills
                    .get(*i)
                    .and_then(|s| s.icon.clone())
                    .unwrap_or_else(|| item.glyph().to_string()),
                _ => item.glyph().to_string(),
            };
            TreeRow {
                lead: format!("  {glyph} "),
                lead_style: item_style(&item),
                name: item_name(app, &item),
                name_style: bold,
                right,
                right_style,
            }
        }
        row @ (Row::Session(_) | Row::Run(_)) => {
            // the tree line stops at the item's last child
            let last = rows.get(at + 1).is_none_or(|r| !r.is_child());
            let lead = format!("    {} ", if last { "└" } else { "├" });
            match row {
                Row::Session(_) => {
                    let s = app
                        .library_session_index(row)
                        .and_then(|i| app.sessions.get(i));
                    let (name, (label, style)) = match s {
                        Some(s) => (
                            format!(
                                "{} · {}",
                                Harness::detect(&s.profile.command)
                                    .map(|h| h.as_str().to_string())
                                    .unwrap_or_else(|| s.profile.name.clone()),
                                super::folder_name(&s.dir)
                            ),
                            super::status_label_style(s.status(now)),
                        ),
                        None => (String::new(), (String::new(), dim())),
                    };
                    TreeRow {
                        lead,
                        lead_style: dim(),
                        name,
                        name_style: Style::default(),
                        right: label,
                        right_style: style,
                    }
                }
                Row::Run(id) => {
                    let run = app.live_workflow_runs.iter().find(|r| &r.run_id == id);
                    TreeRow {
                        lead,
                        lead_style: dim(),
                        name: format!("run {}", id.chars().take(8).collect::<String>()),
                        name_style: Style::default(),
                        right: run
                            .map(|r| format!("{}▶", r.running_sessions()))
                            .unwrap_or_default(),
                        right_style: Style::default().fg(Color::Green),
                    }
                }
                _ => unreachable!("a session or a run"),
            }
        }
    }
}

/// The tree rows as text, fitted to `width` columns: what the drawing
/// shows, without the cursor marker (the tests read this).
pub fn tree_lines(app: &App, st: &LibraryState, width: usize) -> Vec<(String, Row)> {
    let rows = app.library_rows(st);
    let now = Instant::now();
    (0..rows.len())
        .map(|at| {
            let r = tree_row(app, st, &rows, at, now);
            let room = width.saturating_sub(r.lead.width() + r.right.width() + 1);
            let name = super::truncate_chars(&r.name, room);
            let pad = room.saturating_sub(name.width());
            (
                format!("{}{name}{} {}", r.lead, " ".repeat(pad), r.right),
                rows[at].clone(),
            )
        })
        .collect()
}

/// The tree, where the Agents list is: every definition under its
/// section, and under a definition the sessions and runs it launched.
pub fn draw_tree(f: &mut Frame, area: Rect, app: &App, st: &LibraryState, now: Instant) {
    let focused = !st.pane_focused;
    let rows = app.library_rows(st);
    let live = app.library_live_sessions() + app.live_workflow_runs.len();
    // like the Agents list's: the name and where the keyboard is
    let title = if st.filtering {
        format!("Library · /{}▏", st.filter)
    } else if !st.filter.is_empty() {
        format!("Library · /{}", st.filter)
    } else if focused {
        "Library • Keyboard focus".to_string()
    } else if live > 0 {
        format!("Library · {live} live")
    } else {
        "Library".to_string()
    };
    let room = usize::from(area.width.saturating_sub(4));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(Span::styled(
            format!(" {} ", super::truncate_chars(&title, room)),
            if focused {
                head()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let visible = usize::from(area.height.saturating_sub(2));
    let start = super::sidebar_window(st.cursor, rows.len(), visible.saturating_sub(1).max(1));
    st.drawn.set(Drawn {
        tree: area,
        tree_start: start,
        ..st.drawn.get()
    });
    let mut items: Vec<ListItem> = Vec::new();
    for at in (start..rows.len()).take(visible) {
        let is_cursor = at == st.cursor;
        let marker = if is_cursor { ">" } else { " " };
        let r = tree_row(app, st, &rows, at, now);
        let room = inner_w.saturating_sub(1 + r.lead.width() + r.right.width() + 1);
        let name = super::truncate_chars(&r.name, room);
        let pad = room.saturating_sub(name.width());
        let item = ListItem::new(Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(r.lead, r.lead_style),
            Span::styled(name, r.name_style),
            Span::raw(" ".repeat(pad + 1)),
            Span::styled(r.right, r.right_style),
        ]));
        items.push(if is_cursor && focused {
            item.style(Style::default().add_modifier(Modifier::REVERSED))
        } else if is_cursor {
            item.style(Style::default().fg(Color::Cyan))
        } else {
            item
        });
    }
    // what the keys do, under the rows when there is room
    let hints: &[&str] = if rows.is_empty() {
        &[" nothing matches: Esc clears the filter"]
    } else {
        &["", " ↵ launch  m Markdown  / filter  o back"]
    };
    for h in hints {
        if items.len() < visible {
            items.push(ListItem::new(Line::styled(
                super::truncate_chars(h, inner_w),
                dim(),
            )));
        }
    }
    f.render_widget(List::new(items).block(block), area);
}

/// A card line: a yellow label, then its value.
fn field(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<10} "), Style::default().fg(Color::Yellow)),
        Span::raw(v.into()),
    ])
}

fn title_line(text: impl Into<String>) -> Line<'static> {
    Line::styled(text.into(), head())
}

fn note(text: impl Into<String>) -> Line<'static> {
    Line::styled(text.into(), dim())
}

fn problem(text: &str) -> Line<'static> {
    Line::styled(format!("✗ {text}"), Style::default().fg(Color::Red))
}

/// A heading's card: what the section holds and where it cannot run.
fn section_lines(app: &App, st: &LibraryState, section: Section) -> Vec<Line<'static>> {
    let items: Vec<Item> = match section {
        Section::Flows => (0..app.workflow_list.len()).map(Item::Flow).collect(),
        Section::Skills => (0..app.skills.len()).map(Item::Skill).collect(),
        Section::Personas => crate::app::library::persona_names(app)
            .into_iter()
            .map(Item::Persona)
            .collect(),
        Section::Scheduled => (0..app.loop_registry.loops.len()).map(Item::Loop).collect(),
    };
    let mut out = vec![Line::raw(""), note(section.about()), Line::raw("")];
    let live: usize = items.iter().map(|i| app.library_live(i)).sum();
    out.push(field("items", items.len().to_string()));
    out.push(field("running", live.to_string()));
    if section == Section::Scheduled {
        out.push(Line::raw(""));
        out.push(note(
            "r runs one now · p pauses it · e edits it · d removes it",
        ));
        return out;
    }
    let mut full = 0;
    let mut blocked: Vec<String> = Vec::new();
    for item in &items {
        let verdicts = app.library_verdicts_in(st, item);
        let (ok, total) = coverage(&verdicts);
        if ok == total {
            full += 1;
        }
        for (h, v) in &verdicts {
            if matches!(v, Verdict::Blocked(_)) {
                blocked.push(format!(
                    "{} on {}: {}",
                    item_name(app, item),
                    h.as_str(),
                    v.notes().join("; ")
                ));
            }
        }
    }
    out.push(field(
        "everywhere",
        format!("{full} start on every harness"),
    ));
    out.push(Line::raw(""));
    if blocked.is_empty() {
        out.push(note("No harness is blocked for any of them."));
    } else {
        out.push(title_line("Blocked"));
        out.extend(blocked.iter().map(|b| problem(b)));
    }
    out
}

fn harness_lines(app: &App, st: &LibraryState, item: &Item) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (h, v) in app.library_verdicts_in(st, item) {
        out.push(Line::from(vec![
            Span::styled(format!("{} ", verdict_glyph(&v)), verdict_style(&v)),
            Span::styled(
                format!("{:<12}", h.as_str()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(v.label().to_string(), verdict_style(&v)),
        ]));
        // why, under the row it explains
        for n in v.notes() {
            out.push(Line::styled(format!("  {n}"), dim()));
        }
    }
    out.push(Line::raw(""));
    out.push(note(
        "Enter picks one of these to launch on; Blocked and Not installed cannot start.",
    ));
    out
}

fn history_lines(st: &LibraryState, what: &str) -> Vec<Line<'static>> {
    match &st.history {
        None => vec![note("Reading…")],
        Some((_, lines)) if lines.is_empty() => vec![
            note(format!("No {what} recorded yet.")),
            Line::raw(""),
            note("Enter launches it; W opens the runs view."),
        ],
        Some((_, lines)) => lines
            .iter()
            .map(|l| Line::raw(l.clone()))
            .chain([
                Line::raw(""),
                note("Newest first. W opens the runs view on every run."),
            ])
            .collect(),
    }
}

/// What is going now, at the top of an item's first tab.
fn running_line(app: &App, item: &Item) -> Option<Line<'static>> {
    let live = app.library_live(item);
    (live > 0).then(|| {
        Line::from(vec![
            Span::styled("running    ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("● {live}"), Style::default().fg(Color::Green)),
            Span::styled("  → goes to its row, i to the first", dim()),
        ])
    })
}

/// The lines of an item's tab.
fn item_lines(app: &App, st: &LibraryState, item: &Item, tab: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match (item, tab) {
        (_, "Harnesses") => out = harness_lines(app, st, item),
        (Item::Flow(i), "Overview") => {
            if let Some(e) = app.workflow_list.get(*i) {
                if let Some(doc) = &e.doc {
                    out.push(note(doc.description.clone()));
                    out.push(Line::raw(""));
                    out.extend(running_line(app, item));
                    out.push(field("steps", doc.steps.len().to_string()));
                    out.push(Line::raw(""));
                    for s in &doc.steps {
                        let actor = match &s.actor {
                            Some(crate::workflows::document::Actor::Skill(id)) => {
                                format!("skill {id}")
                            }
                            Some(crate::workflows::document::Actor::Prompt(_)) => "prompt".into(),
                            None => "—".into(),
                        };
                        out.push(Line::from(vec![
                            Span::styled(
                                format!("{:<18} ", s.id),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(format!("{actor:<24} ")),
                            Span::styled(
                                format!(
                                    "{} · {}",
                                    s.agent.as_deref().unwrap_or("no agent"),
                                    s.harness.as_deref().unwrap_or("run's harness")
                                ),
                                dim(),
                            ),
                        ]));
                    }
                }
                out.extend(e.problems.iter().map(|p| problem(p)));
            }
        }
        (Item::Flow(i), "Document") => {
            if let Some(e) = app.workflow_list.get(*i) {
                out.extend(e.text.lines().map(|l| Line::raw(l.to_string())));
            }
        }
        (Item::Flow(_), "Runs") => out = history_lines(st, "runs"),
        (Item::Skill(i), "Details") => {
            if let Some(def) = app.skills.get(*i) {
                out.push(note(def.description.clone()));
                out.push(Line::raw(""));
                out.extend(running_line(app, item));
                out.push(field("id", def.id.clone()));
                out.push(field(
                    "invoke",
                    crate::skill::render::invocation(&def.id, def.default_harness),
                ));
                out.push(field("default", def.default_harness.as_str()));
                out.push(field(
                    "source",
                    def.dir
                        .as_ref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_else(|| "built-in".into()),
                ));
                out.push(Line::raw(""));
                out.push(title_line("Installed"));
                let home = app.skill_home_for_library();
                for h in &def.harnesses {
                    let status = crate::skill::install::status(def, *h, &home);
                    let (label, style) = if status.current {
                        ("installed", Style::default().fg(Color::Green))
                    } else if status.installed {
                        (
                            "stale: the next launch updates it",
                            Style::default().fg(Color::Yellow),
                        )
                    } else {
                        ("not yet: the first launch installs it", dim())
                    };
                    out.push(Line::from(vec![
                        Span::raw(format!("{:<12} ", h.as_str())),
                        Span::styled(label, style),
                    ]));
                }
                for w in &def.warnings {
                    out.push(Line::styled(
                        format!("! {w}"),
                        Style::default().fg(Color::Yellow),
                    ));
                }
            }
        }
        (Item::Skill(_), "Executions") => out = history_lines(st, "executions"),
        (Item::Persona(name), "Card") => {
            let catalog = app.library_catalog();
            match catalog.entry(name) {
                Some(e) => {
                    if let Some(spec) = &e.spec {
                        out.push(note(spec.description.clone()));
                        out.push(Line::raw(""));
                        out.extend(running_line(app, item));
                        out.push(field("tools", spec.tools_label()));
                        for (h, b) in &spec.backends {
                            out.push(field(
                                h,
                                format!(
                                    "model {} · effort {}",
                                    b.model.as_deref().unwrap_or("the harness's"),
                                    b.effort.as_deref().unwrap_or("the harness's")
                                ),
                            ));
                        }
                        if let Some(t) = &spec.task {
                            out.push(field("task", t.label()));
                        }
                        if let Some(s) = &spec.schedule {
                            out.push(field(
                                "when",
                                format!(
                                    "every {} · {}",
                                    s.every,
                                    s.harness.as_deref().unwrap_or("(profile)")
                                ),
                            ));
                        }
                        out.push(Line::raw(""));
                        out.push(title_line("Instructions"));
                        out.extend(
                            spec.instructions
                                .lines()
                                .map(|l| Line::raw(format!("  {l}"))),
                        );
                    }
                    out.extend(e.problems.iter().map(|p| problem(p)));
                }
                None => out.push(note("The persona is not in the catalog.")),
            }
        }
        (Item::Persona(_), "Runs") => out = history_lines(st, "runs"),
        (Item::Loop(i), "Card") => {
            if let Some(e) = app.loop_registry.loops.get(*i) {
                out.push(Line::raw(""));
                out.push(field("workspace", e.workspace.display().to_string()));
                out.push(field(
                    "runs on",
                    format!(
                        "{} · profile {}",
                        e.harness,
                        if e.profile.is_empty() {
                            "(first)"
                        } else {
                            &e.profile
                        }
                    ),
                ));
                out.push(field("every", format!("{}s", e.interval_s)));
                out.push(field("level", e.level.as_str()));
                out.push(field(
                    "state",
                    if e.paused() { "paused" } else { "scheduled" },
                ));
                if let Some(next) = &e.next_run_at {
                    out.push(field("next", next.to_string()));
                }
                if let Some(last) = &e.last_run_id {
                    out.push(field("last run", last.clone()));
                }
                out.push(Line::raw(""));
                out.push(note(
                    "r runs it now · p pauses or resumes · e edits · d removes",
                ));
            }
        }
        (Item::Loop(_), "Runs") => out = history_lines(st, "runs"),
        _ => {}
    }
    out
}

/// The card's title, its tab labels (none for a heading) and its lines:
/// what the pane shows for a row that is not a session.
pub fn card(app: &App, st: &LibraryState) -> (String, Vec<&'static str>, Vec<Line<'static>>) {
    match app.library_row(st) {
        None => (
            "Library".into(),
            Vec::new(),
            vec![Line::raw(""), note("Nothing matches the filter.")],
        ),
        Some(Row::Header(section)) => (
            section.label().to_string(),
            Vec::new(),
            section_lines(app, st, section),
        ),
        Some(Row::Run(id)) => (
            format!("run {id}"),
            Vec::new(),
            vec![
                Line::raw(""),
                note("A run of this flow is going."),
                Line::raw(""),
                note("Enter opens it on the Run screen: its steps, sessions and result."),
            ],
        ),
        Some(Row::Session(_)) => (
            "session".into(),
            Vec::new(),
            vec![Line::raw(""), note("The session is gone.")],
        ),
        Some(row) => {
            let Some(item) = Option::<Item>::from(&row) else {
                return (String::new(), Vec::new(), Vec::new());
            };
            let tabs = tab_labels(&item);
            let tab = tabs[st.tab.min(tabs.len() - 1)];
            (
                format!("{} · {}", item_name(app, &item), item.word()),
                tabs.to_vec(),
                item_lines(app, st, &item, tab),
            )
        }
    }
}

/// The card's text, for tests: its tab's lines without styles.
pub fn pane_text(app: &App, st: &LibraryState) -> String {
    card(app, st)
        .2
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The main pane: the selected session's terminal, else the row's card,
/// and the harness picker over either.
pub fn draw_pane(f: &mut Frame, area: Rect, app: &App, st: &LibraryState, now: Instant) {
    st.drawn.set(Drawn {
        pane: area,
        ..st.drawn.get()
    });
    let session = app
        .library_row(st)
        .and_then(|r| app.library_session_index(&r));
    if let Some(idx) = session {
        // the terminal itself, as the Agents list draws it
        st.max_scroll.set(0);
        super::draw_session_pane(f, area, app, idx, now);
    } else {
        draw_card(f, area, app, st);
    }
    if let Some(p) = &st.picker {
        draw_picker(f, area, app, p);
    }
}

fn draw_card(f: &mut Frame, area: Rect, app: &App, st: &LibraryState) {
    let (title, tabs, lines) = card(app, st);
    let room = usize::from(area.width.saturating_sub(4));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(false))
        .title(Span::styled(
            format!(" {} ", super::truncate_chars(&title, room)),
            Style::default().fg(Color::Gray),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let body = if tabs.is_empty() {
        inner
    } else {
        // the tab bar the Run and session screens draw
        let [bar, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
        let current = st.tab.min(tabs.len() - 1);
        let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
        for (n, t) in tabs.iter().enumerate() {
            let label = format!(" {} {t} ", n + 1);
            spans.push(if n == current {
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(label, dim())
            });
            spans.push(Span::raw(" "));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), bar);
        body
    };
    // a margin on both sides, so a wrapped line keeps its indent
    let body = Rect::new(
        body.x + 2.min(body.width),
        body.y,
        body.width.saturating_sub(3),
        body.height,
    );
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max = p
        .line_count(body.width)
        .saturating_sub(usize::from(body.height));
    st.max_scroll.set(max);
    let off = u16::try_from(st.scroll.min(max)).unwrap_or(u16::MAX);
    f.render_widget(p.scroll((off, 0)), body);
}

/// The harness picker: every harness with its verdict, over the pane.
fn draw_picker(f: &mut Frame, area: Rect, app: &App, p: &Picker) {
    let w = area.width.min(72);
    let h = (p.rows.len() as u16 + 5).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(true))
        .title(Span::styled(
            format!(
                " Launch {} on ",
                super::truncate_chars(&item_name(app, &p.item), usize::from(w.saturating_sub(16)))
            ),
            head(),
        ));
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    let width = usize::from(inner.width);
    let mut items: Vec<ListItem> = Vec::new();
    for (i, (h, v)) in p.rows.iter().enumerate() {
        let on = i == p.cursor;
        let left = format!(
            "{} {} {:<12} {:<14}",
            if on { ">" } else { " " },
            verdict_glyph(v),
            h.as_str(),
            v.label()
        );
        let why = super::truncate_chars(
            &v.notes().join("; "),
            width.saturating_sub(left.width() + 1),
        );
        let item = ListItem::new(Line::from(vec![
            Span::styled(
                left,
                if v.launchable() {
                    verdict_style(v)
                } else {
                    dim()
                },
            ),
            Span::raw(" "),
            Span::styled(why, dim()),
        ]));
        items.push(if on {
            item.style(Style::default().add_modifier(Modifier::REVERSED))
        } else {
            item
        });
    }
    items.push(ListItem::new(Line::raw("")));
    // why the row under the cursor cannot start, in full
    let hint = match p.rows.get(p.cursor) {
        Some((_, v)) if !v.launchable() => format!(" {}: pick another", v.label()),
        _ => " ↑↓ pick · ↵ launch · Esc back".to_string(),
    };
    items.push(ListItem::new(Line::styled(
        super::truncate_chars(&hint, width),
        dim(),
    )));
    f.render_widget(List::new(items), inner);
}

/// Draws the whole screen body (tree + pane); the caller adds the
/// status bar, as for the Swarm screen.
pub fn draw_screen(f: &mut Frame, body: Rect, app: &App, now: Instant) {
    let Mode::Library(st) = &app.mode else { return };
    let [side, main] =
        Layout::horizontal([Constraint::Length(super::SIDEBAR_WIDTH), Constraint::Min(0)])
            .areas(body);
    draw_tree(f, side, app, st, now);
    draw_pane(f, main, app, st, now);
}
