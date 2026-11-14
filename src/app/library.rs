//! The Library screen (`Mode::Library`, spec
//! `docs/superpowers/specs/2026-11-14-library-screen-flows-skills-personas-design.md`):
//! every definition — flows, skills, personas — in a left tree on the
//! Swarm screen's geometry, and one harness picker for every launch.
//! Launched sessions are not rows here; they live in the Agents list.
//!
//! The coverage reader in this module is the one source of truth for
//! "can this item run on this harness": the tree's `n/7`, the Harnesses
//! tab and the picker all read it.

use super::{App, Mode};
use crate::agents::AgentSpec;
use crate::harness::Harness;
use crate::skill::SkillDefinition;
use crate::workflows::document::Workflow;

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
}

impl Row {
    pub fn section(&self) -> Section {
        match self {
            Row::Header(s) => *s,
            Row::Flow(_) => Section::Flows,
            Row::Skill(_) => Section::Skills,
            Row::Persona(_) => Section::Personas,
            Row::Loop(_) => Section::Scheduled,
        }
    }
    pub fn is_header(&self) -> bool {
        matches!(self, Row::Header(_))
    }
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
}

impl From<&Row> for Option<Item> {
    fn from(r: &Row) -> Self {
        match r {
            Row::Header(_) => None,
            Row::Flow(i) => Some(Item::Flow(*i)),
            Row::Skill(i) => Some(Item::Skill(*i)),
            Row::Persona(n) => Some(Item::Persona(n.clone())),
            Row::Loop(i) => Some(Item::Loop(*i)),
        }
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

#[derive(Debug, Clone, Default)]
pub struct LibraryState {
    pub cursor: usize,
    /// The selected item's tab (0 = its first tab).
    pub tab: usize,
    pub filter: String,
    pub filtering: bool,
    pub folded: std::collections::HashSet<Section>,
    pub picker: Option<Picker>,
    /// Tab-independent scroll of the main pane, in lines.
    pub scroll: usize,
}

impl LibraryState {
    pub fn new(section: Section) -> Self {
        Self {
            cursor: match section {
                Section::Flows => 0,
                Section::Skills => 1,
                Section::Personas => 2,
                Section::Scheduled => 3,
            },
            ..Self::default()
        }
    }

    pub fn text_entry_active(&self) -> bool {
        self.filtering
    }
    pub fn confirmation_active(&self) -> bool {
        false
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

    /// The Library tree's rows: the three sections in order, folded
    /// sections and the filter applied.
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
            if !state.folded.contains(&section) {
                out.extend(items);
            }
        }
        out
    }

    /// The selected row's item, if the cursor is on one.
    pub fn library_item(&self, state: &LibraryState) -> Option<Item> {
        let rows = self.library_rows(state);
        rows.get(state.cursor).and_then(Option::<Item>::from)
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

    /// A live session running this item, if any.
    pub fn library_live_session(&self, item: &Item) -> Option<usize> {
        match item {
            Item::Skill(i) => {
                let id = &self.skills.get(*i)?.id;
                self.sessions
                    .iter()
                    .position(|s| s.skill_id.as_deref() == Some(id.as_str()))
            }
            Item::Persona(name) => self
                .sessions
                .iter()
                .position(|s| s.agent.as_deref() == Some(name.as_str())),
            Item::Loop(_) => None,
            Item::Flow(i) => {
                let name = &self.workflow_list.get(*i)?.name;
                let run = self.live_workflow_runs.iter().find(|r| &r.name == name)?;
                run.sessions.first().map(|s| s.session_id)
            }
        }
    }

    pub fn open_library(&mut self, section: Section) {
        self.reload_skills();
        self.mode = Mode::Library(Box::new(LibraryState::new(section)));
    }

    fn library_move(&mut self, delta: isize) {
        let (cursor, len) = match &self.mode {
            Mode::Library(st) => (st.cursor, self.library_rows(st).len()),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let next = (cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        if let Mode::Library(st) = &mut self.mode
            && next != st.cursor
        {
            st.cursor = next;
            st.tab = 0;
            st.scroll = 0;
            st.picker = None;
        }
    }

    fn library_open_picker(&mut self) {
        let Some(Mode::Library(_)) = Some(&self.mode) else {
            return;
        };
        let item = match &self.mode {
            Mode::Library(st) => self.library_item(st),
            _ => None,
        };
        let Some(item) = item else { return };
        let rows = self.library_verdicts(&item);
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

    fn library_launch(&mut self, item: &Item, h: Harness) {
        match item {
            Item::Loop(_) => {} // loops launch from their row's verbs, not the picker
            Item::Skill(i) => {
                // the existing launch path: the SkillLauncher dialog
                // (workspace choice) preset to the picked harness, or
                // the running session the skill is a singleton of
                let i = *i;
                self.open_skill_launcher_on(i, h);
            }
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

    fn library_item_current(&self) -> Option<Item> {
        match &self.mode {
            Mode::Library(st) => self.library_item(st),
            _ => None,
        }
    }

    pub fn handle_library_key(&mut self, key: &crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        // the filter's text entry takes the keys first
        let filtering = matches!(&self.mode, Mode::Library(st) if st.filtering);
        if filtering {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.filtering = false;
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.filter.pop();
                        st.cursor = 0;
                    }
                }
                KeyCode::Char(c) => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.filter.push(c);
                        st.cursor = 0;
                    }
                }
                _ => {}
            }
            return;
        }
        // then the picker
        let picker_open = matches!(&self.mode, Mode::Library(st) if st.picker.is_some());
        if picker_open {
            match key.code {
                KeyCode::Esc => {
                    if let Mode::Library(st) = &mut self.mode {
                        st.picker = None;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Mode::Library(st) = &mut self.mode
                        && let Some(p) = &mut st.picker
                    {
                        p.cursor = p.cursor.saturating_sub(1);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Mode::Library(st) = &mut self.mode
                        && let Some(p) = &mut st.picker
                    {
                        p.cursor = (p.cursor + 1).min(p.rows.len().saturating_sub(1));
                    }
                }
                KeyCode::Enter => {
                    let picked = match &self.mode {
                        Mode::Library(st) => st.picker.as_ref().and_then(|p| {
                            p.rows
                                .get(p.cursor)
                                .map(|(h, v)| (p.item.clone(), *h, v.clone()))
                        }),
                        _ => None,
                    };
                    if let Some((item, h, v)) = picked {
                        if v.launchable() {
                            self.library_launch(&item, h);
                        } else {
                            self.notice = Some(super::Notice::warn(format!(
                                "{}: {} — {}",
                                h.as_str(),
                                v.label(),
                                v.notes().join("; ")
                            )));
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Main,
            KeyCode::Up | KeyCode::Char('k') => self.library_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.library_move(1),
            KeyCode::Left | KeyCode::Right => {
                let row = match &self.mode {
                    Mode::Library(st) => self.library_rows(st).get(st.cursor).cloned(),
                    _ => None,
                };
                if let Some(Row::Header(section)) = row
                    && let Mode::Library(st) = &mut self.mode
                {
                    if key.code == KeyCode::Left {
                        st.folded.insert(section);
                    } else {
                        st.folded.remove(&section);
                    }
                }
            }
            KeyCode::Char('/') => {
                if let Mode::Library(st) = &mut self.mode {
                    st.filtering = true;
                }
            }
            KeyCode::Char(c @ '1'..='4') => {
                if let Mode::Library(st) = &mut self.mode {
                    st.tab = (c as usize) - ('1' as usize);
                    st.scroll = 0;
                }
            }
            KeyCode::Char('p') => {
                if let Some(Item::Loop(i)) = self.library_item_current() {
                    self.selected_loop = i;
                    self.toggle_selected_loop_pause();
                }
            }
            KeyCode::Char('d') => {
                if let Some(Item::Loop(i)) = self.library_item_current() {
                    self.selected_loop = i;
                    self.mode = super::Mode::ConfirmRemoveLoop;
                }
            }
            KeyCode::Enter | KeyCode::Char('r') => {
                if let Some(Item::Loop(i)) = self.library_item_current() {
                    self.selected_loop = i;
                    self.run_selected_loop_now();
                } else {
                    self.library_open_picker();
                }
            }
            KeyCode::Char('i') => {
                let item = match &self.mode {
                    Mode::Library(st) => self.library_item(st),
                    _ => None,
                };
                if let Some(item) = item
                    && let Some(idx) = self.library_live_session(&item)
                {
                    self.selected = idx;
                    self.mode = Mode::Main;
                } else {
                    self.notice = Some(super::Notice::info("no live session for this item"));
                }
            }
            KeyCode::Char('e') => {
                let item = match &self.mode {
                    Mode::Library(st) => self.library_item(st),
                    _ => None,
                };
                match item {
                    Some(Item::Loop(i)) => {
                        self.selected_loop = i;
                        let entry = self.loop_registry.loops.get(i).cloned();
                        let own_task =
                            entry.as_ref().and_then(|e| e.agent.clone()).filter(|name| {
                                crate::agents::Catalog::load(&self.library_root(), None)
                                    .entry(name)
                                    .and_then(|x| x.spec.as_ref())
                                    .and_then(|s| s.task.as_ref())
                                    .is_some_and(|t| {
                                        !matches!(t.body(), crate::agents::TaskBody::Pattern(_))
                                    })
                            });
                        match (own_task, entry) {
                            (Some(name), _) => self.open_persona_editor(&name),
                            (None, Some(e)) => self.open_scheduled_editor(Some(e.id.clone()), None),
                            (None, None) => {}
                        }
                    }
                    Some(Item::Flow(i)) => {
                        self.mode = Mode::Main;
                        self.open_workflow_run_editor(i);
                    }
                    Some(Item::Persona(name)) => self.open_persona_editor(&name),
                    Some(Item::Skill(i)) => {
                        if let Some(def) = self.skills.get(i).cloned()
                            && let Some(dir) = &def.dir
                        {
                            self.editor_request = Some(super::EditorRequest {
                                path: dir.join("SKILL.md"),
                                asset_id: format!("skill:{}", def.id),
                                command: crate::assets::editor_command(self.editor.as_deref()),
                            });
                        } else {
                            self.notice = Some(super::Notice::info(
                                "a built-in skill is edited from the Configuration view (C)",
                            ));
                        }
                    }
                    None => {}
                }
            }
            KeyCode::Char('n') => {
                let section = match &self.mode {
                    Mode::Library(st) => self.library_rows(st).get(st.cursor).map(|r| r.section()),
                    _ => None,
                };
                match section {
                    Some(Section::Flows) => self.open_workflow_plan(),
                    Some(Section::Scheduled) => self.open_scheduled_editor(None, None),
                    Some(Section::Personas) => self.open_new_agent(),
                    Some(Section::Skills) => {
                        self.notice = Some(super::Notice::info(
                            "create a skill with `agent-mux skill new <id>`",
                        ));
                    }
                    None => {}
                }
            }
            KeyCode::PageDown => {
                if let Mode::Library(st) = &mut self.mode {
                    st.scroll = st.scroll.saturating_add(10);
                }
            }
            KeyCode::PageUp => {
                if let Mode::Library(st) = &mut self.mode {
                    st.scroll = st.scroll.saturating_sub(10);
                }
            }
            _ => {}
        }
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
