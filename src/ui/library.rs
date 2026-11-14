//! The Library screen's drawing: the flows / skills / personas tree
//! where the Agents list is (the Swarm screen's geometry), the selected
//! row's card in the main pane, and the harness picker over it.

use crate::app::library::{Item, LibraryState, Row, Section, Verdict, coverage};
use crate::app::{App, Mode};
use crate::harness::Harness;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

fn head() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::default().fg(Color::Gray)
}
fn sel() -> Style {
    Style::default()
        .bg(Color::White)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

fn verdict_style(v: &Verdict) -> Style {
    match v {
        Verdict::Ready => Style::default().fg(Color::Green),
        Verdict::Degraded(_) => Style::default().fg(Color::Yellow),
        Verdict::Blocked(_) => Style::default().fg(Color::Red),
        Verdict::NotInstalled => dim(),
    }
}

/// The tree rows as `(text, row)` — the drawing and the tests read this.
pub fn tree_lines(app: &App, st: &LibraryState) -> Vec<(String, Row)> {
    let rows = app.library_rows(st);
    let mut out: Vec<(String, Row)> = Vec::new();
    let mut counts: Vec<(Section, usize)> = Vec::new();
    for r in &rows {
        if !r.is_header() {
            let s = r.section();
            match counts.iter_mut().find(|(c, _)| *c == s) {
                Some((_, n)) => *n += 1,
                None => counts.push((s, 1)),
            }
        }
    }
    for r in rows {
        let text = match &r {
            Row::Header(s) => {
                let n = counts
                    .iter()
                    .find(|(c, _)| c == s)
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                let arrow = if st.folded.contains(s) { "▸" } else { "▾" };
                format!("{arrow} {}  {n}", s.label())
            }
            Row::Flow(i) => {
                let name = app
                    .workflow_list
                    .get(*i)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                let item = Item::Flow(*i);
                let (ok, total) = coverage(&app.library_verdicts(&item));
                let live = app.library_live_session(&item).is_some();
                format!("  {name}  {ok}/{total}{}", if live { " ●" } else { "" })
            }
            Row::Skill(i) => {
                let name = app
                    .skills
                    .get(*i)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let item = Item::Skill(*i);
                let (ok, total) = coverage(&app.library_verdicts(&item));
                let live = app.library_live_session(&item).is_some();
                format!("  {name}  {ok}/{total}{}", if live { " ●" } else { "" })
            }
            Row::Loop(i) => {
                let (label, paused) = app
                    .loop_registry
                    .loops
                    .get(*i)
                    .map(|e| (format!("{} · {}", e.pattern, e.harness), e.paused()))
                    .unwrap_or_default();
                format!("  {label}{}", if paused { " ⏸" } else { "" })
            }
            Row::Persona(name) => {
                let item = Item::Persona(name.clone());
                let (ok, total) = coverage(&app.library_verdicts(&item));
                let live = app.library_live_session(&item).is_some();
                format!("  {name}  {ok}/{total}{}", if live { " ●" } else { "" })
            }
        };
        out.push((text, r));
    }
    out
}

pub fn draw_tree(f: &mut Frame, area: Rect, app: &App, st: &LibraryState) {
    let lines = tree_lines(app, st);
    let mut title = " Library ".to_string();
    if st.filtering || !st.filter.is_empty() {
        title = format!(" Library · /{} ", st.filter);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let height = usize::from(inner.height);
    let start = st
        .cursor
        .saturating_sub(height.saturating_sub(1).max(0))
        .min(lines.len().saturating_sub(height.max(1)));
    for (y, (i, (text, row))) in (inner.y..).zip(lines.iter().enumerate().skip(start).take(height))
    {
        let style = if i == st.cursor {
            sel()
        } else if row.is_header() {
            head()
        } else {
            Style::default()
        };
        let rect = Rect::new(inner.x, y, inner.width, 1);
        f.render_widget(Paragraph::new(Line::styled(text.clone(), style)), rect);
    }
    let hint = "o back · ↑↓ · Enter launch · e edit · n new · / filter";
    let rect = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    f.render_widget(Paragraph::new(Line::styled(hint, dim())), rect);
}

/// The tab labels of the selected item's kind.
pub fn tab_labels(item: &Item) -> Vec<&'static str> {
    match item {
        Item::Flow(_) => vec!["Overview", "Harnesses", "Runs", "Document"],
        Item::Skill(_) => vec!["Details", "Harnesses", "Executions"],
        Item::Persona(_) => vec!["Card", "Harnesses", "Runs"],
        Item::Loop(_) => vec!["Card", "Runs"],
    }
}

/// The main pane's lines for the selected row (the picker excluded).
pub fn pane_lines(app: &App, st: &LibraryState) -> Vec<Line<'static>> {
    let rows = app.library_rows(st);
    let Some(row) = rows.get(st.cursor) else {
        return vec![Line::styled("nothing here", dim())];
    };
    match row {
        Row::Header(section) => section_lines(app, *section),
        item_row => {
            let Some(item) = Option::<Item>::from(item_row) else {
                return Vec::new();
            };
            item_lines(app, &item, st.tab)
        }
    }
}

fn section_lines(app: &App, section: Section) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::styled(format!("{} ", section.label()), head()),
        Line::raw(""),
    ];
    let items: Vec<Item> = match section {
        Section::Flows => (0..app.workflow_list.len()).map(Item::Flow).collect(),
        Section::Skills => (0..app.skills.len()).map(Item::Skill).collect(),
        Section::Personas => crate::app::library::persona_names(app)
            .into_iter()
            .map(Item::Persona)
            .collect(),
        Section::Scheduled => (0..app.loop_registry.loops.len()).map(Item::Loop).collect(),
    };
    let mut full = 0;
    let mut blocked: Vec<String> = Vec::new();
    for item in &items {
        let verdicts = app.library_verdicts(item);
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
    out.push(Line::raw(format!(
        "{} items · {full} on every harness",
        items.len()
    )));
    if blocked.is_empty() {
        out.push(Line::styled("no blocked harness", dim()));
    } else {
        out.push(Line::raw(""));
        out.push(Line::styled("blocked:", head()));
        for b in blocked {
            out.push(Line::styled(
                format!("  {b}"),
                verdict_style(&Verdict::Blocked(b.clone())),
            ));
        }
    }
    out
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

fn item_lines(app: &App, item: &Item, tab: usize) -> Vec<Line<'static>> {
    let tabs = tab_labels(item);
    let tab = tab.min(tabs.len().saturating_sub(1));
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut bar = String::new();
    for (i, t) in tabs.iter().enumerate() {
        bar.push_str(&format!(
            " {} {}:{} ",
            i + 1,
            if i == tab { "▸" } else { " " },
            t
        ));
    }
    out.push(Line::styled(bar, head()));
    out.push(Line::raw(""));
    match (item, tabs[tab]) {
        (_, "Harnesses") => {
            for (h, v) in app.library_verdicts(item) {
                out.push(Line::from(vec![
                    Span::styled(format!("{:<10}", h.as_str()), head()),
                    Span::styled(format!("{:<14}", v.label()), verdict_style(&v)),
                    Span::raw(v.notes().join("; ")),
                ]));
            }
        }
        (Item::Flow(i), "Overview") => {
            if let Some(e) = app.workflow_list.get(*i) {
                out.push(Line::styled(e.name.clone(), head()));
                if let Some(doc) = &e.doc {
                    out.push(Line::raw(doc.description.clone()));
                    out.push(Line::raw(""));
                    for s in &doc.steps {
                        let actor = match &s.actor {
                            Some(crate::workflows::document::Actor::Skill(id)) => {
                                format!("skill {id}")
                            }
                            Some(crate::workflows::document::Actor::Prompt(_)) => "prompt".into(),
                            None => "—".into(),
                        };
                        out.push(Line::raw(format!(
                            "  {:<16} {:<22} agent {:<14} harness {}",
                            s.id,
                            actor,
                            s.agent.as_deref().unwrap_or("—"),
                            s.harness.as_deref().unwrap_or("(run profile)")
                        )));
                    }
                }
                for p in &e.problems {
                    out.push(Line::styled(
                        format!("✗ {p}"),
                        verdict_style(&Verdict::Blocked(p.clone())),
                    ));
                }
            }
        }
        (Item::Flow(i), "Document") => {
            if let Some(e) = app.workflow_list.get(*i) {
                for l in e.text.lines() {
                    out.push(Line::raw(l.to_string()));
                }
            }
        }
        (Item::Flow(_), "Runs") => {
            out.push(Line::raw(
                "this flow's runs are in the runs view (W); Enter in the Agents list opens a run",
            ));
        }
        (Item::Skill(i), "Details") => {
            if let Some(def) = app.skills.get(*i) {
                out.push(Line::styled(def.name.clone(), head()));
                out.push(Line::raw(def.description.clone()));
                out.push(Line::raw(""));
                out.push(Line::raw(format!("id          {}", def.id)));
                out.push(Line::raw(format!(
                    "harnesses   {}",
                    def.harnesses
                        .iter()
                        .map(|h| h.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
                out.push(Line::raw(format!(
                    "default     {}",
                    def.default_harness.as_str()
                )));
                out.push(Line::raw(format!(
                    "source      {}",
                    def.dir
                        .as_ref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_else(|| "built-in".into())
                )));
                for h in Harness::ALL {
                    let status =
                        crate::skill::install::status(def, h, &app.skill_home_for_library());
                    out.push(Line::raw(format!(
                        "  {:<10} {}",
                        h.as_str(),
                        status_label(status.current, status.installed)
                    )));
                }
                for w in &def.warnings {
                    out.push(Line::styled(format!("! {w}"), dim()));
                }
            }
        }
        (Item::Skill(_), "Executions") => {
            out.push(Line::raw(
                "this skill's executions are in the Skills view (S), Executions tab",
            ));
        }
        (Item::Persona(name), "Card") => {
            let catalog = app.library_catalog();
            match catalog.entry(name) {
                Some(e) => {
                    out.push(Line::styled(e.name.clone(), head()));
                    if let Some(spec) = &e.spec {
                        out.push(Line::raw(spec.description.clone()));
                        out.push(Line::raw(""));
                        out.push(Line::raw(format!("tools   {}", spec.tools_label())));
                        for (h, b) in &spec.backends {
                            out.push(Line::raw(format!(
                                "  {h:<10} model {} effort {}",
                                b.model.as_deref().unwrap_or("—"),
                                b.effort.as_deref().unwrap_or("—")
                            )));
                        }
                        if let Some(t) = &spec.task {
                            out.push(Line::raw(format!("task    {}", t.label())));
                        }
                        if let Some(s) = &spec.schedule {
                            out.push(Line::raw(format!(
                                "when    every {} · {}",
                                s.every,
                                s.harness.as_deref().unwrap_or("(profile)")
                            )));
                        }
                        out.push(Line::raw(""));
                        for l in spec.instructions.lines().take(20) {
                            out.push(Line::styled(format!("  {l}"), dim()));
                        }
                    }
                    for p in &e.problems {
                        out.push(Line::styled(
                            format!("✗ {p}"),
                            verdict_style(&Verdict::Blocked(p.clone())),
                        ));
                    }
                }
                None => out.push(Line::raw("the persona is not in the catalog")),
            }
        }
        (Item::Loop(i), "Card") => {
            if let Some(e) = app.loop_registry.loops.get(*i) {
                out.push(Line::styled(e.pattern.clone(), head()));
                out.push(Line::raw(""));
                out.push(Line::raw(format!("workspace  {}", e.workspace.display())));
                out.push(Line::raw(format!(
                    "runs on    {} · profile {}",
                    e.harness,
                    if e.profile.is_empty() {
                        "(first)"
                    } else {
                        &e.profile
                    }
                )));
                out.push(Line::raw(format!("every      {}s", e.interval_s)));
                out.push(Line::raw(format!(
                    "level      {} · {}",
                    e.level.as_str(),
                    if e.paused() { "paused" } else { "scheduled" }
                )));
                if let Some(next) = &e.next_run_at {
                    out.push(Line::raw(format!("next       {next}")));
                }
                if let Some(last) = &e.last_run_id {
                    out.push(Line::raw(format!("last run   {last}")));
                }
                out.push(Line::raw(""));
                out.push(Line::styled(
                    "r run now · p pause/resume · e edit · d remove".to_string(),
                    dim(),
                ));
            }
        }
        (Item::Loop(_), "Runs") => {
            out.push(Line::raw(
                "this loop's runs are in the runs view (W); its sessions sit in the Agents list",
            ));
        }
        (Item::Persona(_), "Runs") => {
            out.push(Line::raw(
                "this persona's runs are in the runs view (W), under its name",
            ));
        }
        _ => {}
    }
    out
}

fn status_label(current: bool, installed: bool) -> &'static str {
    if current {
        "installed ✓"
    } else if installed {
        "stale"
    } else {
        "not installed"
    }
}

pub fn draw_pane(f: &mut Frame, area: Rect, app: &App, st: &LibraryState) {
    let item = app.library_item(st);
    let title = match &item {
        Some(it) => format!(" {} ", item_name(app, it)),
        None => " Library ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, head()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = pane_lines(app, st);
    let height = usize::from(inner.height);
    let start = st.scroll.min(lines.len().saturating_sub(height.max(1)));
    let shown: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    f.render_widget(Paragraph::new(shown).wrap(Wrap { trim: false }), inner);
    if let Some(p) = &st.picker {
        draw_picker(f, area, p);
    }
}

fn draw_picker(f: &mut Frame, area: Rect, p: &crate::app::library::Picker) {
    let w = area.width.min(64);
    let h = (p.rows.len() as u16 + 4).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" launch on ".to_string(), head()));
    let inner = block.inner(rect);
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(block, rect);
    for (y, (i, (h, v))) in (inner.y..).zip(p.rows.iter().enumerate()) {
        if y >= inner.y + inner.height {
            break;
        }
        let style = if i == p.cursor {
            sel()
        } else {
            Style::default()
        };
        let line = Line::from(vec![
            Span::styled(format!(" {:<10} {:<14}", h.as_str(), v.label()), style),
            Span::styled(v.notes().join("; "), verdict_style(v)),
        ]);
        f.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
    }
    let hint = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    f.render_widget(
        Paragraph::new(Line::styled("↑↓ pick · Enter launch · Esc back", dim())),
        hint,
    );
}

/// Draws the whole screen body (tree + pane); the caller adds the
/// status bar, as for the Swarm screen.
pub fn draw_screen(f: &mut Frame, body: Rect, app: &App) {
    let Mode::Library(st) = &app.mode else { return };
    let [side, main] = ratatui::layout::Layout::horizontal([
        ratatui::layout::Constraint::Length(crate::ui::SIDEBAR_WIDTH),
        ratatui::layout::Constraint::Min(0),
    ])
    .areas(body);
    draw_tree(f, side, app, st);
    draw_pane(f, main, app, st);
}
