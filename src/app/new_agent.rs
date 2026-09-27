//! `n` in the Agents list: how a new agent starts (spec section 4). Describe
//! it (the planner drafts), start blank (a session, a scheduled agent, a
//! flow, a persona), or start from a template (a loop pattern, a workflow,
//! a built-in persona). Each choice opens the creator that already exists
//! for it; the unified agent editor replaces them in a later step.

use super::{App, Mode, Notice};
use crate::keymap::{Verb, verb};
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    Describe,
    Session,
    Scheduled,
    Flow,
    Persona,
    Pattern(String),
    Workflow(String),
    PersonaTemplate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub group: &'static str,
    pub start: Start,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentState {
    pub choices: Vec<Choice>,
    pub selected: usize,
}

impl App {
    pub fn open_new_agent(&mut self) {
        let mut choices = vec![
            Choice {
                group: "Describe it",
                start: Start::Describe,
                label: "describe what it should do".into(),
                detail: "a planner drafts it from a sentence; you review it before it exists"
                    .into(),
            },
            Choice {
                group: "Blank",
                start: Start::Session,
                label: "a session".into(),
                detail: "a harness in a folder, driven by you".into(),
            },
            Choice {
                group: "Blank",
                start: Start::Scheduled,
                label: "a scheduled agent".into(),
                detail: "runs one task every interval in a workspace".into(),
            },
            Choice {
                group: "Blank",
                start: Start::Flow,
                label: "a flow".into(),
                detail: "steps in order, each run by an agent".into(),
            },
            Choice {
                group: "Blank",
                start: Start::Persona,
                label: "a persona".into(),
                detail: "instructions, tools and a model that steps and runs can use".into(),
            },
        ];
        for p in crate::loops::patterns::all() {
            choices.push(Choice {
                group: "From a template",
                start: Start::Pattern(p.id.clone()),
                label: format!(
                    "{}  ⟳ {}",
                    p.id,
                    crate::loops::format_interval(p.default_interval_s)
                ),
                detail: p.goal.clone(),
            });
        }
        for e in self.workflow_entries().into_iter().filter(|e| e.valid()) {
            choices.push(Choice {
                group: "From a template",
                start: Start::Workflow(e.name.clone()),
                label: format!("{}  ⚙ flow", e.name),
                detail: e.doc.map(|d| d.description).unwrap_or_default(),
            });
        }
        for (name, text) in crate::agents::BUILTIN {
            let desc = crate::agents::AgentSpec::parse(text)
                .map(|a| a.description)
                .unwrap_or_default();
            choices.push(Choice {
                group: "From a template",
                start: Start::PersonaTemplate(name.to_string()),
                label: format!("{name}  persona"),
                detail: desc,
            });
        }
        // start on the kind of agent the cursor is on
        use super::agents_list::AgentKind;
        let hint = match self.agent_row() {
            Some(AgentKind::Harness(_) | AgentKind::Session(_)) => Some(Start::Session),
            Some(AgentKind::Loop(_)) => Some(Start::Scheduled),
            Some(AgentKind::Flow(_)) => Some(Start::Flow),
            Some(AgentKind::Persona(_)) => Some(Start::Persona),
            _ => None,
        };
        let selected = hint
            .and_then(|h| choices.iter().position(|c| c.start == h))
            .unwrap_or(0);
        self.mode = Mode::NewAgent(Box::new(NewAgentState { choices, selected }));
    }

    pub fn handle_new_agent_key(&mut self, key: &KeyEvent) {
        let Mode::NewAgent(st) = &mut self.mode else {
            return;
        };
        let n = st.choices.len();
        let group_start =
            |st: &NewAgentState, g: &str| st.choices.iter().position(|c| c.group == g);
        match key.code {
            KeyCode::Char('1') => st.selected = group_start(st, "Describe it").unwrap_or(0),
            KeyCode::Char('2') => st.selected = group_start(st, "Blank").unwrap_or(0),
            KeyCode::Char('3') => st.selected = group_start(st, "From a template").unwrap_or(0),
            KeyCode::Up | KeyCode::Char('k') => st.selected = st.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => st.selected = (st.selected + 1).min(n - 1),
            _ => match verb(&crate::keymap::list_alias(key)) {
                Some(Verb::Back) => self.mode = Mode::Control,
                Some(Verb::Top) => st.selected = 0,
                Some(Verb::Bottom) => st.selected = n - 1,
                Some(Verb::PageDown) => st.selected = (st.selected + 10).min(n - 1),
                Some(Verb::PageUp) => st.selected = st.selected.saturating_sub(10),
                Some(Verb::Open) => {
                    let start = st.choices[st.selected].start.clone();
                    self.mode = Mode::Control;
                    self.start_new_agent(start);
                }
                _ => {}
            },
        }
    }

    fn start_new_agent(&mut self, start: Start) {
        match start {
            Start::Describe => self.open_workflow_plan(),
            Start::Session => self.open_new_session_dialog(None),
            Start::Scheduled => self.open_loop_dialog(None),
            Start::Flow => self.open_flow_builder_new(),
            Start::Persona => self.new_persona("blank", "my-agent"),
            Start::Pattern(id) => {
                self.open_loop_dialog(None);
                if let Mode::NewLoop(d) = &mut self.mode
                    && let Some(i) = crate::loops::patterns::all()
                        .iter()
                        .position(|p| p.id == id)
                {
                    d.pattern_idx = i;
                }
            }
            Start::Workflow(name) => {
                let Some(e) = self.workflow_entries().into_iter().find(|e| e.name == name) else {
                    return;
                };
                let origin = crate::app::flow_builder::Origin::Document(e.name.clone());
                let ws = self
                    .dialog_workspaces()
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                self.open_flow_builder_text(&e.text, origin, Vec::new(), ws);
            }
            Start::PersonaTemplate(t) => self.new_persona(&t, &format!("my-{t}")),
        }
    }

    /// Writes a new persona agent into the library from `template` under
    /// a free name, and opens it in `$EDITOR`.
    fn new_persona(&mut self, template: &str, base: &str) {
        let dir = crate::agents::library_dir(&self.library_root());
        let taken =
            |n: &str| dir.join(format!("{n}.toml")).exists() || crate::agents::builtin(n).is_some();
        let name = (1..)
            .map(|i| {
                if i == 1 {
                    base.to_string()
                } else {
                    format!("{base}-{i}")
                }
            })
            .find(|n| !taken(n))
            .expect("a free name");
        let Some(text) = crate::agents::from_template(template, &name) else {
            return;
        };
        let path = dir.join(format!("{name}.toml"));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return;
        }
        self.reload_personas();
        self.editor_request = Some(super::EditorRequest {
            path,
            asset_id: format!("agent:{name}"),
            command: crate::assets::editor_command(self.editor.as_deref()),
        });
    }

    /// `e` / `Enter` on a persona: its file in `$EDITOR`; a built-in one is
    /// copied into the library first and the copy is what changes.
    pub fn edit_persona(&mut self, name: &str) {
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        let Some(entry) = catalog.entry(name).cloned() else {
            return;
        };
        let path = entry.source.path().to_path_buf();
        if matches!(entry.source, crate::agents::Source::Builtin(_)) && !path.exists() {
            let text = crate::agents::text_of(&entry).unwrap_or_default();
            if let Err(e) = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::write(&path, text))
            {
                self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
                return;
            }
        }
        self.editor_request = Some(super::EditorRequest {
            path,
            asset_id: format!("agent:{name}"),
            command: crate::assets::editor_command(self.editor.as_deref()),
        });
    }

    /// The New session dialog, on harness profile `profile` when given.
    pub fn open_new_session_dialog(&mut self, profile: Option<usize>) {
        let (default, available) = match &self.tracing {
            Some(rt) => (rt.default_backend(), rt.langfuse_configured()),
            None => (crate::config::Backend::Local, false),
        };
        let mut d = super::DialogState::new(&self.profiles)
            .with_bypass_default(self.bypass_approvals_default, &self.profiles)
            .with_backend_options(default, available, &self.profiles)
            .with_experiments(self.tracing.is_some());
        if let Some(p) = profile.filter(|p| *p < self.profiles.len()) {
            d.set_profile(p, &self.profiles);
        }
        self.mode = Mode::NewSession(d);
    }
}
