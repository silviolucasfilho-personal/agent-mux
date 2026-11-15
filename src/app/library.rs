//! The Library screen (`Mode::Library`, spec
//! `docs/superpowers/specs/2026-11-14-library-screen-flows-skills-personas-design.md`):
//! every definition — flows, skills, personas, scheduled agents — in a
//! left tree, in the Swarm screen's layout and with its conventions.
//!
//! - **Tree:** a section heading, its items, and under an item the
//!   sessions and runs it launched (`Row::Session`, `Row::Run`). Those
//!   sessions are seen here only, as a swarm's are on the Swarm screen:
//!   the Agents list leaves them out (`App::session_listed`) unless
//!   `[agents] show_library = true`.
//! - **Main pane:** the selected row. A session is drawn through the
//!   Agents screen's pane (`ui::draw_session_pane`) and `Enter` types
//!   into it (`pane_focused`; the Focus Agents chord returns to the
//!   tree); an item is its card in tabs.
//! - **Launch:** one harness picker for every item, then the dialog its
//!   kind already has. The session it starts is revealed here
//!   (`App::reveal_session`), and a dialog or editor that closes comes
//!   back here (`App::library_return`, `App::library_resume`).
//!
//! The coverage reader in this module is the one source of truth for
//! "can this item run on this harness": the tree's `n/7`, the Harnesses
//! tab and the picker all read it.

use super::{App, Mode, Notice};
use crate::agents::AgentSpec;
use crate::harness::Harness;
use crate::keymap::{Verb, verb};
use crate::skill::SkillDefinition;
use crate::status::Status;
use crate::workflows::document::Workflow;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// Whether an item can launch on one harness, and what is lost if it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ready,
    /// Launches, with named capabilities lost (warnings, shown verbatim).
    Degraded(Vec<String>),
    /// Cannot launch; the reason is the message.
    Blocked(String),
    /// The harness binary is not on this machine.
    NotInstalled,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Ready => "Ready",
            Verdict::Degraded(_) => "Degraded",
            Verdict::Blocked(_) => "Blocked",
            Verdict::NotInstalled => "Not installed",
        }
    }

    /// Ready and Degraded both start a session.
    pub fn launchable(&self) -> bool {
        matches!(self, Verdict::Ready | Verdict::Degraded(_))
    }

    pub fn notes(&self) -> Vec<String> {
        match self {
            Verdict::Degraded(n) => n.clone(),
            Verdict::Blocked(r) => vec![r.clone()],
            Verdict::NotInstalled => vec!["the harness is not installed on this machine".into()],
            Verdict::Ready => Vec::new(),
        }
    }
}

/// Whether `h`'s binary resolves here (`PATH` or its installer's
/// fallback path, the rule `session::command_available` already uses).
pub fn available(h: Harness) -> bool {
    crate::session::command_available(h.as_str())
}

/// Every harness, in `Harness::ALL` order, with its availability.
pub fn availability() -> Vec<(Harness, bool)> {
    Harness::ALL
        .into_iter()
        .map(|h| (h, available(h)))
        .collect()
}

pub fn skill_verdict(def: &SkillDefinition, h: Harness) -> Verdict {
    if !available(h) {
        return Verdict::NotInstalled;
    }
    if !def.harnesses.contains(&h) {
        return Verdict::Blocked(format!(
            "the package restricts itself to {} (skill.toml `harnesses`)",
            def.harnesses
                .iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Verdict::Ready
}

pub fn persona_verdict(spec: Option<&AgentSpec>, problems: &[String], h: Harness) -> Verdict {
    if !available(h) {
        return Verdict::NotInstalled;
    }
    let Some(spec) = spec else {
        return Verdict::Blocked(format!(
            "the persona does not load: {}",
            problems.join("; ")
        ));
    };
    let notes: Vec<String> = crate::agents::launch::tool_notes(spec, h)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    if notes.is_empty() {
        Verdict::Ready
    } else {
        Verdict::Degraded(notes)
    }
}

pub fn flow_verdict(
    doc: Option<&Workflow>,
    problems: &[String],
    catalog: &crate::agents::Catalog,
    skills: &[crate::workflows::document::SkillInfo],
    h: Harness,
) -> Verdict {
    if !available(h) {
        return Verdict::NotInstalled;
    }
    let Some(doc) = doc else {
        return Verdict::Blocked(format!("the flow does not parse: {}", problems.join("; ")));
    };
    let diags = crate::agents::fit::check_on(doc, catalog, skills, h);
    let errors: Vec<String> = diags
        .iter()
        .filter(|d| d.level == crate::agents::launch::Level::Error)
        .map(|d| d.message.clone())
        .collect();
    if !errors.is_empty() {
        return Verdict::Blocked(errors.join("; "));
    }
    let warnings: Vec<String> = diags.into_iter().map(|d| d.message).collect();
    if warnings.is_empty() {
        Verdict::Ready
    } else {
        Verdict::Degraded(warnings)
    }
}

/// `(launchable, total)` over every harness in `Harness::ALL`.
pub fn coverage(verdicts: &[(Harness, Verdict)]) -> (usize, usize) {
    (
        verdicts.iter().filter(|(_, v)| v.launchable()).count(),
        Harness::ALL.len(),
    )
}

/// The verdicts for every harness, in `Harness::ALL` order.
pub fn all_verdicts<V: Fn(Harness) -> Verdict>(f: V) -> Vec<(Harness, Verdict)> {
    Harness::ALL.into_iter().map(|h| (h, f(h))).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Flows,
    Skills,
    Personas,
    Scheduled,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Flows => "flows",
            Section::Skills => "skills",
            Section::Personas => "personas",
            Section::Scheduled => "scheduled",
        }
    }

    /// What the section holds, in a line (its heading's card).
    pub fn about(self) -> &'static str {
        match self {
            Section::Flows => "A flow is a document of steps; each step runs a headless session.",
            Section::Skills => "A skill is a package of instructions a harness loads on demand.",
            Section::Personas => "A persona is who a session is: instructions, tools and a model.",
            Section::Scheduled => "A scheduled agent runs its task in one workspace on a timer.",
        }
    }

    pub const ALL: [Section; 4] = [
        Section::Flows,
        Section::Skills,
        Section::Personas,
        Section::Scheduled,
    ];
}

/// One row of the Library tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Header(Section),
    /// Index into `App::workflow_list`.
    Flow(usize),
    /// Index into `App::skills`.
    Skill(usize),
    /// Persona name (catalog entry).
    Persona(String),
    /// Index into `loop_registry.loops` (a scheduled loop).
    Loop(usize),
    /// A session the item above it launched, by the session's id (not
    /// its index: the row stays the same row when another session ends).
    Session(usize),
    /// A run in flight of the flow above it, by its run id.
    Run(String),
}

impl Row {
    pub fn is_header(&self) -> bool {
        matches!(self, Row::Header(_))
    }

    /// A session or a run: launched work, listed under its definition.
    pub fn is_child(&self) -> bool {
        matches!(self, Row::Session(_) | Row::Run(_))
    }
}

/// The section row `at` sits in: the nearest heading above it.
pub fn section_at(rows: &[Row], at: usize) -> Option<Section> {
    rows.get(..=at)?.iter().rev().find_map(|r| match r {
        Row::Header(s) => Some(*s),
        _ => None,
    })
}

/// The item a launch or tab set belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Flow(usize),
    Skill(usize),
    Persona(String),
    /// A scheduled loop (index into `loop_registry.loops`).
    Loop(usize),
}

impl Item {
    pub fn section(&self) -> Section {
        match self {
            Item::Flow(_) => Section::Flows,
            Item::Skill(_) => Section::Skills,
            Item::Persona(_) => Section::Personas,
            Item::Loop(_) => Section::Scheduled,
        }
    }

    /// The marker the Agents list and the trees use for the kind.
    pub fn glyph(&self) -> &'static str {
        match self {
            Item::Flow(_) => "⚙",
            Item::Skill(_) => "⚡",
            Item::Persona(_) => "◇",
            Item::Loop(_) => "⟳",
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            Item::Flow(_) => "flow",
            Item::Skill(_) => "skill",
            Item::Persona(_) => "persona",
            Item::Loop(_) => "scheduled",
        }
    }
}

impl From<&Row> for Option<Item> {
    fn from(r: &Row) -> Self {
        match r {
            Row::Header(_) | Row::Session(_) | Row::Run(_) => None,
            Row::Flow(i) => Some(Item::Flow(*i)),
            Row::Skill(i) => Some(Item::Skill(*i)),
            Row::Persona(n) => Some(Item::Persona(n.clone())),
            Row::Loop(i) => Some(Item::Loop(*i)),
        }
    }
}

/// The tab labels of an item's kind.
pub fn tab_labels(item: &Item) -> &'static [&'static str] {
    match item {
        Item::Flow(_) => &["Overview", "Harnesses", "Runs", "Document"],
        Item::Skill(_) => &["Details", "Harnesses", "Executions"],
        Item::Persona(_) => &["Card", "Harnesses", "Runs"],
        Item::Loop(_) => &["Card", "Runs"],
    }
}

/// The harness picker (`Enter` on an item): every available harness
/// with its verdict; Blocked and Not installed rows show but do not pick.
#[derive(Debug, Clone)]
pub struct Picker {
    pub item: Item,
    pub rows: Vec<(Harness, Verdict)>,
    pub cursor: usize,
}

/// Where the last frame put the tree and the pane, for the mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drawn {
    pub tree: Rect,
    /// The first row the tree drew.
    pub tree_start: usize,
    pub pane: Rect,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryState {
    pub cursor: usize,
    /// The cursor and the row under it when the rows were last read:
    /// while the cursor has not moved, a session that starts or ends
    /// elsewhere keeps it on that row.
    pub anchor: Option<(usize, Row)>,
    /// The selected item's tab (0 = its first tab).
    pub tab: usize,
    pub filter: String,
    pub filtering: bool,
    pub folded: std::collections::HashSet<Section>,
    pub picker: Option<Picker>,
    /// The selected session's terminal has the keyboard (as on the Swarm
    /// screen: `Enter` gives it, the Focus Agents chord takes it back).
    pub pane_focused: bool,
    /// The card's scroll, in lines, and how far the last frame let it go.
    pub scroll: usize,
    pub max_scroll: Cell<usize>,
    pub drawn: Cell<Drawn>,
    /// Each item's verdicts, read once while the screen is open (they
    /// probe the machine and the catalog): reloaded with `Ctrl+R`.
    pub verdicts: RefCell<std::collections::HashMap<String, Vec<(Harness, Verdict)>>>,
    /// The Runs / Executions tab's lines, and the item they are of.
    pub history: Option<(String, Vec<String>)>,
}

impl LibraryState {
    pub fn text_entry_active(&self) -> bool {
        self.filtering || self.pane_focused
    }
    /// The harness picker takes every key while it is open.
    pub fn confirmation_active(&self) -> bool {
        self.picker.is_some()
    }

    fn set_cursor(&mut self, at: usize) {
        if at != self.cursor {
            self.cursor = at;
            self.anchor = None;
            self.tab = 0;
            self.scroll = 0;
            self.picker = None;
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll.get());
    }
}

/// Persona names in the Library: the Agents list's personas and its
/// on-demand task agents, deduplicated, in list order.
pub fn persona_names(app: &App) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in app.task_agents.iter().chain(app.personas.iter()) {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

impl App {
    /// The persona catalog, loaded for the Library (entries by name).
    pub fn library_catalog(&self) -> crate::agents::Catalog {
        crate::agents::Catalog::load(&self.library_root(), None)
    }

    /// A stable name for an item (its verdict cache, its history).
    pub fn library_item_key(&self, item: &Item) -> String {
        match item {
            Item::Flow(i) => format!(
                "flow:{}",
                self.workflow_list.get(*i).map_or("", |e| e.name.as_str())
            ),
            Item::Skill(i) => format!(
                "skill:{}",
                self.skills.get(*i).map_or("", |s| s.id.as_str())
            ),
            Item::Persona(n) => format!("persona:{n}"),
            Item::Loop(i) => format!(
                "loop:{}",
                self.loop_registry
                    .loops
                    .get(*i)
                    .map_or("", |e| e.id.as_str())
            ),
        }
    }

    /// The Library item session `idx` was launched from: a persona or a
    /// listed skill. A session of a swarm, a loop, a flow run or a
    /// delegation belongs to that group, and a plain session to nothing.
    pub fn library_item_of(&self, idx: usize) -> Option<Item> {
        let s = self.sessions.get(idx)?;
        if s.group.is_some() {
            return None;
        }
        if let Some(name) = &s.agent
            && (self.task_agents.contains(name) || self.personas.contains(name))
        {
            return Some(Item::Persona(name.clone()));
        }
        let id = s.skill_id.as_ref()?;
        self.skills
            .iter()
            .position(|k| &k.id == id)
            .map(Item::Skill)
    }

    /// The sessions `item` launched, as indexes into `sessions`.
    pub fn library_sessions(&self, item: &Item) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|i| self.library_item_of(*i).as_ref() == Some(item))
            .collect()
    }

    /// The runs in flight of a flow, by run id.
    pub fn library_runs(&self, item: &Item) -> Vec<String> {
        let Item::Flow(i) = item else {
            return Vec::new();
        };
        let Some(name) = self.workflow_list.get(*i).map(|e| &e.name) else {
            return Vec::new();
        };
        self.live_workflow_runs
            .iter()
            .filter(|r| &r.name == name)
            .map(|r| r.run_id.clone())
            .collect()
    }

    /// How many of an item's sessions and runs are going now.
    pub fn library_live(&self, item: &Item) -> usize {
        let now = Instant::now();
        self.library_sessions(item)
            .into_iter()
            .filter(|i| !matches!(self.sessions[*i].status(now), Status::Exited(_)))
            .count()
            + self.library_runs(item).len()
    }

    /// The Library's sessions that are going, across every item: what the
    /// Agents list says it is not listing.
    pub fn library_live_sessions(&self) -> usize {
        let now = Instant::now();
        (0..self.sessions.len())
            .filter(|i| self.library_item_of(*i).is_some())
            .filter(|i| !matches!(self.sessions[*i].status(now), Status::Exited(_)))
            .count()
    }

    /// The Library tree's rows: the sections in order, each item with the
    /// sessions and runs it launched under it; folded sections and the
    /// filter applied.
    pub fn library_rows(&self, state: &LibraryState) -> Vec<Row> {
        let filter = state.filter.trim().to_lowercase();
        let matches = |name: &str, desc: &str| {
            filter.is_empty()
                || name.to_lowercase().contains(&filter)
                || desc.to_lowercase().contains(&filter)
        };
        let mut out = Vec::new();
        for section in Section::ALL {
            let items: Vec<Row> = match section {
                Section::Flows => self
                    .workflow_list
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        matches(
                            &e.name,
                            e.doc.as_ref().map(|d| d.description.as_str()).unwrap_or(""),
                        )
                    })
                    .map(|(i, _)| Row::Flow(i))
                    .collect(),
                Section::Skills => self
                    .skills
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| matches(&s.name, &s.description) || matches(&s.id, ""))
                    .map(|(i, _)| Row::Skill(i))
                    .collect(),
                Section::Personas => persona_names(self)
                    .into_iter()
                    .filter(|n| matches(n, ""))
                    .map(Row::Persona)
                    .collect(),
                Section::Scheduled => self
                    .loop_registry
                    .loops
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches(&e.pattern, "") || matches(&e.id, ""))
                    .map(|(i, _)| Row::Loop(i))
                    .collect(),
            };
            if !filter.is_empty() && items.is_empty() {
                continue;
            }
            out.push(Row::Header(section));
            if state.folded.contains(&section) {
                continue;
            }
            for row in items {
                let item = Option::<Item>::from(&row);
                out.push(row);
                let Some(item) = item else { continue };
                out.extend(self.library_runs(&item).into_iter().map(Row::Run));
                out.extend(
                    self.library_sessions(&item)
                        .into_iter()
                        .map(|i| Row::Session(self.sessions[i].id)),
                );
            }
        }
        out
    }

    /// The row under the cursor.
    pub fn library_row(&self, state: &LibraryState) -> Option<Row> {
        self.library_rows(state).get(state.cursor).cloned()
    }

    /// The item under the cursor; on a session or a run, the item that
    /// launched it.
    pub fn library_item(&self, state: &LibraryState) -> Option<Item> {
        let rows = self.library_rows(state);
        rows.get(..=state.cursor)?
            .iter()
            .rev()
            .take_while(|r| !r.is_header())
            .find_map(Option::<Item>::from)
    }

    /// The index in `sessions` of the session a row stands for.
    pub fn library_session_index(&self, row: &Row) -> Option<usize> {
        match row {
            Row::Session(id) => self.sessions.iter().position(|s| s.id == *id),
            _ => None,
        }
    }

    /// The verdicts behind an item's `n/7`, its Harnesses tab and its
    /// picker — one reader, so the three never disagree.
    pub fn library_verdicts(&self, item: &Item) -> Vec<(Harness, Verdict)> {
        match item {
            Item::Skill(i) => match self.skills.get(*i) {
                Some(def) => all_verdicts(|h| skill_verdict(def, h)),
                None => Vec::new(),
            },
            Item::Persona(name) => {
                let catalog = self.library_catalog();
                let entry = catalog.entry(name);
                let (spec, problems) = match entry {
                    Some(e) => (e.spec.as_ref(), e.problems.as_slice()),
                    None => (None, &[][..]),
                };
                all_verdicts(|h| persona_verdict(spec, problems, h))
            }
            Item::Loop(i) => {
                let Some(entry) = self.loop_registry.loops.get(*i) else {
                    return Vec::new();
                };
                let own = crate::harness::Harness::detect(&entry.harness);
                all_verdicts(|h| {
                    if Some(h) == own {
                        if available(h) {
                            Verdict::Ready
                        } else {
                            Verdict::NotInstalled
                        }
                    } else {
                        Verdict::Blocked(
                            "a scheduled loop runs on its own profile's harness".into(),
                        )
                    }
                })
            }
            Item::Flow(i) => {
                let Some(entry) = self.workflow_list.get(*i) else {
                    return Vec::new();
                };
                let catalog = self.library_catalog();
                let mut packages: Vec<&SkillDefinition> = self
                    .skills
                    .iter()
                    .chain(self.hidden_skills.iter())
                    .collect();
                packages.dedup_by(|a, b| a.id == b.id);
                let defs: Vec<SkillDefinition> = packages.into_iter().cloned().collect();
                let infos = crate::workflows::library::skill_infos(&defs);
                all_verdicts(|h| {
                    flow_verdict(entry.doc.as_ref(), &entry.problems, &catalog, &infos, h)
                })
            }
        }
    }

    /// `library_verdicts`, read once per item while the screen is open:
    /// the drawing asks for every row on every frame.
    pub fn library_verdicts_in(&self, st: &LibraryState, item: &Item) -> Vec<(Harness, Verdict)> {
        let key = self.library_item_key(item);
        if let Some(v) = st.verdicts.borrow().get(&key) {
            return v.clone();
        }
        let v = self.library_verdicts(item);
        st.verdicts.borrow_mut().insert(key, v.clone());
        v
    }

    /// The Runs / Executions tab of an item: its runs, newest first.
    fn library_history(&self, item: &Item) -> Vec<String> {
        let now = crate::tracing::store::now_ns();
        if let Item::Skill(i) = item {
            let Some(def) = self.skills.get(*i) else {
                return Vec::new();
            };
            let Some(conn) = self
                .trace_db_path
                .as_deref()
                .and_then(|p| crate::tracing::store::open_ro(p).ok())
            else {
                return vec!["Tracing is off: no executions are recorded.".into()];
            };
            return crate::tracing::store::query::skill_launches(&conn, &def.id, "", None, 30)
                .unwrap_or_default()
                .into_iter()
                .map(|l| {
                    let state = match (l.live, l.exit_code) {
                        (true, _) => "running".to_string(),
                        (false, Some(0)) | (false, None) => "ended".to_string(),
                        (false, Some(code)) => format!("exit {code}"),
                    };
                    let folder = std::path::Path::new(&l.cwd)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(l.cwd.clone());
                    format!(
                        "{} {:<10} {:<10} {:<9} {:>3} turn(s)  {folder}",
                        if l.live { "●" } else { "·" },
                        l.provider,
                        super::runs_view::ago(l.started_ns, now),
                        state,
                        l.turns,
                    )
                })
                .collect();
        }
        let name = match item {
            Item::Flow(i) => self.workflow_list.get(*i).map(|e| e.name.clone()),
            Item::Persona(n) => Some(n.clone()),
            Item::Loop(i) => self.loop_registry.loops.get(*i).map(|e| e.pattern.clone()),
            Item::Skill(_) => None,
        };
        let Some(name) = name else {
            return Vec::new();
        };
        let (items, _) = self.runs_view_items();
        items
            .into_iter()
            .filter(|r| r.agent == name)
            .take(30)
            .map(|r| format!("{} {:<14} {}", r.glyph, r.when, r.word))
            .collect()
    }

    /// What the native Markdown viewer should open for the Library item
    /// under the cursor: a skill's `SKILL.md`, a persona's file (or its
    /// instructions staged as Markdown), or a flow's document staged as
    /// Markdown. Sessions, runs and headings have no document.
    pub fn library_viewer_target(&self) -> Option<super::ViewerTarget> {
        let Mode::Library(st) = &self.mode else {
            return None;
        };
        let item = self.library_item(st)?;
        match item {
            Item::Skill(i) => {
                let def = self.skills.get(i)?;
                if let Some(dir) = &def.dir {
                    let path = dir.join("SKILL.md");
                    if path.is_file() {
                        return Some(super::ViewerTarget::File(path));
                    }
                }
                if def.body.trim().is_empty() {
                    return None;
                }
                Some(super::ViewerTarget::Text {
                    stem: format!("skill-{}", def.id),
                    text: format!("# {}\n\n{}", def.name, def.body),
                })
            }
            Item::Persona(name) => {
                let catalog = self.library_catalog();
                let entry = catalog.entry(&name)?;
                let path = entry.source.path().to_path_buf();
                if crate::markdown::is_markdown_path(&path) && path.is_file() {
                    return Some(super::ViewerTarget::File(path));
                }
                let spec = entry.spec.as_ref()?;
                if spec.instructions.trim().is_empty() {
                    return None;
                }
                Some(super::ViewerTarget::Text {
                    stem: format!("persona-{name}"),
                    text: format!("# {name}\n\n{}", spec.instructions),
                })
            }
            Item::Flow(i) => {
                let entry = self.workflow_list.get(i)?;
                if entry.text.trim().is_empty() {
                    return None;
                }
                Some(super::ViewerTarget::Text {
                    stem: format!("flow-{}", entry.name),
                    text: format!("# {}\n\n```toml\n{}\n```\n", entry.name, entry.text),
                })
            }
            Item::Loop(_) => None,
        }
    }

    /// Opens the Library on `section`: on its first item, else its heading.
    pub fn open_library(&mut self, section: Section) {
        self.reload_skills();
        let mut st = LibraryState::default();
        let rows = self.library_rows(&st);
        let head = rows
            .iter()
            .position(|r| *r == Row::Header(section))
            .unwrap_or(0);
        st.cursor = match rows.get(head + 1) {
            Some(r) if !r.is_header() => head + 1,
            _ => head,
        };
        self.library_return = None;
        self.mode = Mode::Library(Box::new(st));
        self.library_sync();
    }

    /// The Library on the row of session `idx`, its terminal with the
    /// keyboard: where a skill's or a persona's session is shown, as a
    /// swarm's is on the Swarm screen.
    pub fn open_library_on_session(&mut self, idx: usize) {
        let Some(id) = self.sessions.get(idx).map(|s| s.id) else {
            return;
        };
        let item = self.library_item_of(idx);
        // the screen as it was left (a launch goes through a dialog)
        let mut st = match std::mem::replace(&mut self.mode, Mode::Main) {
            Mode::Library(st) => st,
            _ => self.library_return.take().unwrap_or_default(),
        };
        self.library_return = None;
        st.picker = None;
        st.filtering = false;
        if let Some(item) = &item {
            st.folded.remove(&item.section());
        }
        let mut rows = self.library_rows(&st);
        if !rows.contains(&Row::Session(id)) {
            st.filter.clear();
            rows = self.library_rows(&st);
        }
        if let Some(at) = rows.iter().position(|r| *r == Row::Session(id)) {
            st.cursor = at;
            st.anchor = None;
            st.pane_focused = true;
        }
        st.tab = 0;
        st.scroll = 0;
        self.selected = idx;
        self.selection = None;
        if let Some(s) = self.sessions.get_mut(idx) {
            s.tracker.on_attach();
        }
        self.mode = Mode::Library(st);
        self.library_sync();
    }

    /// Puts session `idx` on screen with the keyboard, where its row is:
    /// the Library for a session a skill or a persona launched (unless
    /// `[agents] show_library` lists it with the others), else Agents.
    pub(crate) fn reveal_session(&mut self, idx: usize) {
        if !self.session_listed(idx) && self.library_item_of(idx).is_some() {
            self.open_library_on_session(idx);
            return;
        }
        self.library_return = None;
        self.selected = idx;
        self.sidebar_section = super::SidebarSection::Active;
        self.mode = Mode::Main;
        self.focus_pane();
    }

    /// `Esc`: back to the Agents list, on a session it lists.
    fn library_leave(&mut self) {
        self.library_return = None;
        self.mode = Mode::Main;
        if !self.session_listed(self.selected)
            && let Some(i) = (0..self.sessions.len()).find(|i| self.session_listed(*i))
        {
            self.selected = i;
            self.selection = None;
        }
        self.focus_list();
    }

    /// Keeps the open screen in step with the sessions: the cursor stays
    /// on its row, the pane's keys go to the session it shows, and the
    /// Runs tab is read when it opens. Run after a key and on the tick.
    pub fn library_sync(&mut self) {
        let Mode::Library(st) = &self.mode else {
            return;
        };
        let rows = self.library_rows(st);
        let mut cursor = st.cursor.min(rows.len().saturating_sub(1));
        // a session started or ended above: follow the row, not the index
        let mut lost = false;
        if let Some((at, anchor)) = &st.anchor
            && *at == st.cursor
            && rows.get(cursor) != Some(anchor)
        {
            match rows.iter().position(|r| r == anchor) {
                Some(at) => cursor = at,
                None => lost = true,
            }
        }
        let row = rows.get(cursor).cloned();
        let session = row.as_ref().and_then(|r| self.library_session_index(r));
        // the Runs / Executions tab's lines, read when the tab opens
        let item = row.as_ref().and_then(Option::<Item>::from);
        let wanted = item.as_ref().and_then(|item| {
            let tabs = tab_labels(item);
            let label = tabs[st.tab.min(tabs.len() - 1)];
            matches!(label, "Runs" | "Executions").then(|| self.library_item_key(item))
        });
        let history = match (&wanted, &item) {
            (Some(key), Some(item)) if st.history.as_ref().map(|(k, _)| k) != Some(key) => {
                Some((key.clone(), self.library_history(item)))
            }
            _ => None,
        };
        if let Some(i) = session
            && self.selected != i
        {
            self.selected = i;
            self.selection = None;
        }
        if let Mode::Library(st) = &mut self.mode {
            st.cursor = cursor;
            st.anchor = row.map(|r| (cursor, r));
            if session.is_none() || lost {
                st.pane_focused = false;
            }
            if wanted.is_none() {
                st.history = None;
            } else if history.is_some() {
                st.history = history;
            }
        }
    }

    /// A dialog or an editor the Library opened has closed: the Library
    /// comes back as it was left. Run after a key and on the tick.
    pub fn library_resume(&mut self) {
        if self.library_return.is_none() {
            return;
        }
        match &self.mode {
            Mode::Main => {
                let Some(mut st) = self.library_return.take() else {
                    return;
                };
                st.picker = None;
                st.pane_focused = false;
                st.anchor = None;
                st.history = None;
                // an editor may have changed what the rows say
                st.verdicts.borrow_mut().clear();
                self.reload_skills();
                self.mode = Mode::Library(st);
                self.library_sync();
            }
            Mode::Library(_) => self.library_return = None,
            // what the Library opens, and what those open
            Mode::SkillLauncher(_)
            | Mode::NewSession(_)
            | Mode::WorkflowDialog(_)
            | Mode::AgentEditor(_)
            | Mode::NewAgent(_)
            | Mode::ConfirmRemoveLoop
            | Mode::ConfirmKill
            | Mode::Run(_)
            | Mode::SessionRun(_) => {}
            _ => self.library_return = None,
        }
    }

    fn library_move(&mut self, delta: isize) {
        let (cursor, len) = match &self.mode {
            Mode::Library(st) => (st.cursor, self.library_rows(st).len()),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let next = cursor.saturating_add_signed(delta).min(len - 1);
        if let Mode::Library(st) = &mut self.mode {
            st.set_cursor(next);
        }
    }

    fn library_open_picker(&mut self, item: Item) {
        let Mode::Library(st) = &self.mode else {
            return;
        };
        let rows = self.library_verdicts_in(st, &item);
        // the item's own default first (a skill's `default_harness`, a
        // persona's first `[backends]` entry), else the first Ready
        let preferred = match &item {
            Item::Skill(i) => self.skills.get(*i).map(|d| d.default_harness),
            Item::Persona(name) => self
                .library_catalog()
                .entry(name)
                .and_then(|e| e.spec.as_ref())
                .and_then(|s| s.backends.keys().next())
                .and_then(|h| Harness::detect(h)),
            Item::Flow(_) | Item::Loop(_) => None,
        };
        let cursor = preferred
            .and_then(|p| rows.iter().position(|(h, v)| *h == p && v.launchable()))
            .or_else(|| rows.iter().position(|(_, v)| matches!(v, Verdict::Ready)))
            .or_else(|| rows.iter().position(|(_, v)| v.launchable()))
            .unwrap_or(0);
        if let Mode::Library(st) = &mut self.mode {
            st.picker = Some(Picker { item, rows, cursor });
        }
    }

    /// Starts `item` on `h` through the dialog its kind already has. The
    /// session it starts is shown here (`reveal_session`), and a dialog
    /// that is cancelled comes back here (`library_resume`).
    fn library_launch(&mut self, item: &Item, h: Harness) {
        match item {
            Item::Loop(_) => {} // loops launch from their row's verbs, not the picker
            Item::Skill(i) => self.open_skill_launcher_on(*i, h),
            Item::Persona(name) => {
                let name = name.clone();
                self.mode = Mode::Main;
                self.open_agent_session_on(&name, Some(h));
            }
            Item::Flow(i) => {
                let i = *i;
                self.mode = Mode::Main;
                self.open_workflow_run_on(i, Some(h));
            }
        }
    }

    /// `Enter` and `r` on an item: the harness picker; a scheduled loop
    /// runs now on its own harness.
    fn library_run(&mut self, item: Item) {
        if let Item::Loop(i) = item {
            self.selected_loop = i;
            self.run_selected_loop_now();
        } else {
            self.library_open_picker(item);
        }
    }

    /// `Enter` on a session's row: its terminal takes the keyboard.
    fn library_focus_pane(&mut self, idx: usize) {
        let ended = self
            .sessions
            .get(idx)
            .is_none_or(|s| matches!(s.status(Instant::now()), Status::Exited(_)));
        if ended {
            self.notice = Some(Notice::info(
                "the session has ended: d removes it, r starts another",
            ));
            return;
        }
        self.selected = idx;
        if let Some(s) = self.sessions.get_mut(idx) {
            s.tracker.on_attach();
        }
        if let Mode::Library(st) = &mut self.mode {
            st.pane_focused = true;
        }
        self.notice = Some(Notice::info(
            "keys go to the session · ⌘L (Ctrl+Shift+L) returns to the library tree",
        ));
    }

    fn library_edit(&mut self, item: Item) {
        match item {
            Item::Loop(i) => {
                self.selected_loop = i;
                let entry = self.loop_registry.loops.get(i).cloned();
                let own_task = entry.as_ref().and_then(|e| e.agent.clone()).filter(|name| {
                    self.library_catalog()
                        .entry(name)
                        .and_then(|x| x.spec.as_ref())
                        .and_then(|s| s.task.as_ref())
                        .is_some_and(|t| !matches!(t.body(), crate::agents::TaskBody::Pattern(_)))
                });
                match (own_task, entry) {
                    (Some(name), _) => self.open_persona_editor(&name),
                    (None, Some(e)) => self.open_scheduled_editor(Some(e.id.clone()), None),
                    (None, None) => {}
                }
            }
            Item::Flow(i) => {
                self.mode = Mode::Main;
                self.open_workflow_run_editor(i);
            }
            Item::Persona(name) => self.open_persona_editor(&name),
            Item::Skill(i) => {
                if let Some(def) = self.skills.get(i).cloned()
                    && let Some(dir) = &def.dir
                {
                    self.editor_request = Some(super::EditorRequest {
                        path: dir.join("SKILL.md"),
                        asset_id: format!("skill:{}", def.id),
                        command: crate::assets::editor_command(self.editor.as_deref()),
                    });
                } else {
                    self.notice = Some(Notice::info(
                        "a built-in skill is edited from the Configuration view (C)",
                    ));
                }
            }
        }
    }

    /// A key on the Library screen. A dialog or an editor it opens keeps
    /// the screen as it was, to come back to (`library_resume`).
    pub fn handle_library_key(&mut self, key: &KeyEvent) {
        let Mode::Library(st) = &self.mode else {
            return;
        };
        let before = st.clone();
        let left = self.library_key(key);
        if !left && !matches!(self.mode, Mode::Library(_)) {
            self.library_return = Some(before);
        }
        self.library_sync();
    }

    /// Returns true when the key left the screen for the Agents list.
    fn library_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // the filter's text entry takes the keys first
        if let Mode::Library(st) = &mut self.mode
            && st.filtering
        {
            match key.code {
                KeyCode::Enter => st.filtering = false,
                KeyCode::Esc => {
                    st.filtering = false;
                    st.filter.clear();
                    st.set_cursor(0);
                }
                KeyCode::Backspace => {
                    st.filter.pop();
                    st.set_cursor(0);
                }
                KeyCode::Char(c) if !ctrl => {
                    st.filter.push(c);
                    st.set_cursor(0);
                }
                _ => {}
            }
            return false;
        }
        // then the picker
        if let Mode::Library(st) = &mut self.mode
            && let Some(p) = &mut st.picker
        {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => st.picker = None,
                KeyCode::Up | KeyCode::Char('k') => p.cursor = p.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    p.cursor = (p.cursor + 1).min(p.rows.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    let item = p.item.clone();
                    let picked = p.rows.get(p.cursor).cloned();
                    match picked {
                        Some((h, v)) if v.launchable() => {
                            st.picker = None;
                            self.library_launch(&item, h);
                        }
                        Some((h, v)) => {
                            self.notice = Some(Notice::warn(format!(
                                "{}: {} — {}",
                                h.as_str(),
                                v.label(),
                                v.notes().join("; ")
                            )));
                        }
                        None => {}
                    }
                }
                _ => {}
            }
            return false;
        }
        let Mode::Library(st) = &self.mode else {
            return false;
        };
        let rows = self.library_rows(st);
        let at = st.cursor.min(rows.len().saturating_sub(1));
        let row = rows.get(at).cloned();
        let item = self.library_item(st);
        let session = row.as_ref().and_then(|r| self.library_session_index(r));
        let filtered = !st.filter.is_empty();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.library_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.library_move(1),
            KeyCode::Left | KeyCode::Right => {
                let unfold = key.code == KeyCode::Right;
                match &row {
                    Some(Row::Header(section)) => {
                        if let Mode::Library(st) = &mut self.mode {
                            if unfold {
                                st.folded.remove(section);
                            } else {
                                st.folded.insert(*section);
                            }
                        }
                    }
                    // as in the Agents list: ← goes to the row's parent
                    Some(r) if !unfold => {
                        let parent = if r.is_child() {
                            rows[..at].iter().rposition(|r| !r.is_child())
                        } else {
                            rows[..at].iter().rposition(|r| r.is_header())
                        };
                        if let (Some(p), Mode::Library(st)) = (parent, &mut self.mode) {
                            st.set_cursor(p);
                        }
                    }
                    // and → onto its first session or run
                    Some(r) if !r.is_child() => {
                        if rows.get(at + 1).is_some_and(|r| r.is_child())
                            && let Mode::Library(st) = &mut self.mode
                        {
                            st.set_cursor(at + 1);
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Char('/') => {
                if let Mode::Library(st) = &mut self.mode {
                    st.filtering = true;
                }
            }
            KeyCode::Char(c @ '1'..='4') if row.as_ref().is_some_and(|r| !r.is_child()) => {
                let n = (c as usize) - ('1' as usize);
                if let Some(item) = &item
                    && n < tab_labels(item).len()
                    && let Mode::Library(st) = &mut self.mode
                {
                    st.tab = n;
                    st.scroll = 0;
                }
            }
            KeyCode::Enter => match (&row, session) {
                (Some(Row::Header(section)), _) => {
                    if let Mode::Library(st) = &mut self.mode
                        && !st.folded.remove(section)
                    {
                        st.folded.insert(*section);
                    }
                }
                (Some(Row::Session(_)), Some(idx)) => self.library_focus_pane(idx),
                (Some(Row::Run(id)), _) => {
                    self.open_run_screen(id, super::run_screen::Origin::Agents);
                }
                _ => {
                    if let Some(item) = item {
                        self.library_run(item);
                    }
                }
            },
            // Run: another session of the item, also from one of its rows
            KeyCode::Char('r') if !ctrl => {
                if let Some(item) = item {
                    self.library_run(item);
                }
            }
            KeyCode::Char('p') => {
                if let Some(Item::Loop(i)) = item {
                    self.selected_loop = i;
                    self.toggle_selected_loop_pause();
                }
            }
            // Stop: the session under the cursor
            KeyCode::Char('x') => match (&row, session) {
                (Some(Row::Session(_)), Some(idx)) => {
                    if matches!(self.sessions[idx].status(Instant::now()), Status::Exited(_)) {
                        self.notice = Some(Notice::info("the session has ended: d removes it"));
                    } else {
                        self.selected = idx;
                        self.mode = Mode::ConfirmKill;
                    }
                }
                (Some(Row::Run(_)), _) => {
                    self.notice = Some(Notice::info("Enter opens the run; x there stops it"));
                }
                _ => self.notice = Some(Notice::info("x stops a session: select its row")),
            },
            // Delete: an ended session's row, a scheduled agent
            KeyCode::Char('d') if !ctrl => match (&row, session) {
                (Some(Row::Session(_)), Some(idx)) => {
                    self.selected = idx;
                    if matches!(self.sessions[idx].status(Instant::now()), Status::Exited(_)) {
                        self.apply(super::Action::RemoveSelected, key, Instant::now());
                    } else {
                        self.mode = Mode::ConfirmKill;
                    }
                }
                (Some(Row::Loop(i)), _) => {
                    self.selected_loop = *i;
                    self.mode = Mode::ConfirmRemoveLoop;
                }
                (Some(Row::Flow(_) | Row::Skill(_) | Row::Persona(_)), _) => {
                    self.notice = Some(Notice::info(
                        "a definition is removed from the Configuration view (C)",
                    ));
                }
                _ => {}
            },
            // a session's run screen, as in the Agents list; on an item,
            // its first session
            KeyCode::Char('i') if !ctrl => match (session, &item) {
                (Some(idx), _) => {
                    self.selected = idx;
                    self.open_session_screen();
                }
                (None, Some(item)) => {
                    let first = self
                        .library_sessions(item)
                        .first()
                        .map(|i| Row::Session(self.sessions[*i].id));
                    match first.and_then(|r| rows.iter().position(|x| *x == r)) {
                        Some(p) => {
                            if let Mode::Library(st) = &mut self.mode {
                                st.set_cursor(p);
                            }
                        }
                        None => {
                            self.notice = Some(Notice::info("no session of this item is open"));
                        }
                    }
                }
                _ => {}
            },
            KeyCode::Char('e') if !ctrl => {
                if let Some(item) = item {
                    self.library_edit(item);
                }
            }
            KeyCode::Char('m') if !ctrl => {
                self.open_library_in_markdown();
            }
            KeyCode::Char('n') if !ctrl => match section_at(&rows, at) {
                Some(Section::Flows) => self.open_workflow_plan(),
                Some(Section::Scheduled) => self.open_scheduled_editor(None, None),
                Some(Section::Personas) => self.open_new_agent(),
                Some(Section::Skills) => {
                    self.notice = Some(Notice::info(
                        "create a skill with `agent-mux skill new <id>`",
                    ));
                }
                None => {}
            },
            // the key that opened the screen closes it
            KeyCode::Char('o') if !ctrl => {
                self.library_leave();
                return true;
            }
            _ => match verb(key) {
                Some(Verb::Back) if filtered => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.filter.clear();
                        st.set_cursor(0);
                    }
                }
                Some(Verb::Back) => {
                    self.library_leave();
                    return true;
                }
                Some(Verb::Top) => self.library_move(isize::MIN),
                Some(Verb::Bottom) => self.library_move(isize::MAX),
                Some(Verb::PageDown) => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.scroll_by(10);
                    }
                }
                Some(Verb::PageUp) => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.scroll_by(-10);
                    }
                }
                Some(Verb::Reload) => {
                    self.reload_skills();
                    if let Mode::Library(st) = &mut self.mode {
                        st.verdicts.borrow_mut().clear();
                        st.history = None;
                    }
                    self.notice = Some(Notice::info("library reloaded"));
                }
                _ => {}
            },
        }
        false
    }

    /// The mouse on the Library screen: a click selects a row or gives a
    /// session's terminal the keyboard; the wheel scrolls what it is over.
    pub fn library_mouse(&mut self, ev: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let Mode::Library(st) = &self.mode else {
            return;
        };
        if st.picker.is_some() {
            return;
        }
        let drawn = st.drawn.get();
        let at = ratatui::layout::Position::new(ev.column, ev.row);
        let rows = self.library_rows(st);
        let session = rows
            .get(st.cursor)
            .and_then(|r| self.library_session_index(r));
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) if drawn.tree.contains(at) => {
                let line = usize::from(ev.row.saturating_sub(drawn.tree.y + 1));
                let target = drawn.tree_start + line;
                if ev.row > drawn.tree.y
                    && target < rows.len()
                    && let Mode::Library(st) = &mut self.mode
                {
                    st.pane_focused = false;
                    st.filtering = false;
                    st.set_cursor(target);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if drawn.pane.contains(at) => {
                if let Some(idx) = session {
                    self.library_focus_pane(idx);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                if drawn.pane.contains(at) && session.is_some() {
                    self.scroll_selected(if up { 3 } else { -3 });
                } else if drawn.pane.contains(at) {
                    if let Mode::Library(st) = &mut self.mode {
                        st.scroll_by(if up { -3 } else { 3 });
                    }
                } else if drawn.tree.contains(at) {
                    self.library_move(if up { -1 } else { 1 });
                }
            }
            _ => {}
        }
        self.library_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentSpec;

    fn spec(tools: &str) -> AgentSpec {
        AgentSpec::parse(&format!(
            "name = \"reviewer\"\ndescription = \"d\"\ninstructions = \"i\"\n{tools}\n"
        ))
        .unwrap()
    }

    fn skill(harnesses: Vec<Harness>) -> SkillDefinition {
        SkillDefinition {
            id: "demo".into(),
            name: "Demo".into(),
            description: "d".into(),
            icon: None,
            harnesses,
            default_harness: Harness::Claude,
            capabilities: Vec::new(),
            startup_prompt: None,
            hydrate: Vec::new(),
            mcp: crate::skill::McpMode::Auto,
            warnings: Vec::new(),
            body: String::new(),
            files: Vec::new(),
            source_hash: String::new(),
            dir: None,
            is_builtin: false,
            hidden: false,
            writes: false,
            auto_approve: false,
        }
    }

    #[test]
    fn persona_verdicts_follow_tool_notes() {
        // every harness in this test is one the test machine may not
        // have: availability is read from the machine, so assert on the
        // harnesses that are here and skip the rest
        let s = spec("tools = [\"read\", \"web\"]");
        for h in Harness::ALL {
            let v = persona_verdict(Some(&s), &[], h);
            if !available(h) {
                assert_eq!(v, Verdict::NotInstalled, "{h:?}");
                continue;
            }
            match h {
                // OpenCode maps `web` onto its own tools, so it is Ready too
                Harness::Claude | Harness::OpenCode => {
                    assert_eq!(v, Verdict::Ready, "{h:?}")
                }
                _ => assert!(matches!(v, Verdict::Degraded(_)), "{h:?}: {v:?}"),
            }
        }
        // a persona that does not load is Blocked wherever it could run
        let v = persona_verdict(None, &["bad toml".into()], Harness::Claude);
        if available(Harness::Claude) {
            assert!(matches!(v, Verdict::Blocked(r) if r.contains("bad toml")));
        }
    }

    #[test]
    fn skill_verdicts_name_a_packages_own_restriction() {
        let def = skill(vec![Harness::Claude]);
        if available(Harness::Codex) {
            let v = skill_verdict(&def, Harness::Codex);
            assert!(matches!(v, Verdict::Blocked(r) if r.contains("restricts itself")));
        }
        if available(Harness::Claude) {
            assert_eq!(skill_verdict(&def, Harness::Claude), Verdict::Ready);
        }
        let all = skill(Harness::ALL.to_vec());
        for h in Harness::ALL {
            if available(h) {
                assert_eq!(skill_verdict(&all, h), Verdict::Ready, "{h:?}");
            } else {
                assert_eq!(skill_verdict(&all, h), Verdict::NotInstalled, "{h:?}");
            }
        }
    }

    #[test]
    fn verdict_labels_and_coverage() {
        let vs = vec![
            (Harness::Claude, Verdict::Ready),
            (Harness::Codex, Verdict::Degraded(vec!["x".into()])),
            (Harness::Grok, Verdict::Blocked("no".into())),
            (Harness::Pi, Verdict::NotInstalled),
        ];
        assert_eq!(coverage(&vs), (2, 7));
        assert!(Verdict::Degraded(vec![]).launchable());
        assert!(!Verdict::Blocked("x".into()).launchable());
        assert_eq!(Verdict::NotInstalled.label(), "Not installed");
    }
}
