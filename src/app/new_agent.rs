//! `n` in the Agents list: how a new agent starts (spec section 4). Describe
//! it (the planner drafts), start blank (a session, a scheduled agent, a
//! flow, a persona), or start from a template (a loop pattern, a workflow,
//! a built-in persona). A scheduled agent, a flow or a persona opens in the
//! agent editor (`agent_editor`); a session opens the New session dialog.

use super::{App, Mode, Notice};
use crate::keymap::{Verb, verb};
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    Describe,
    Session,
    Scheduled,
    Task,
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
                start: Start::Task,
                label: "a task".into(),
                detail: "a prompt or a skill it runs when you start it, or on a schedule".into(),
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
            Start::Scheduled => self.open_scheduled_editor(None, None),
            Start::Flow => self.open_flow_builder_new(),
            Start::Persona => self.new_persona_editor("blank", "my-agent"),
            Start::Task => {
                self.new_persona_editor("blank", "my-task");
                if let Mode::AgentEditor(st) = &mut self.mode
                    && let super::agent_editor::Body::Persona(p) = &mut st.body
                {
                    p.task = super::agent_editor::TaskKind::Prompt;
                    p.form.instructions.set("");
                    st.set_tab(super::agent_editor::Tab::What);
                }
            }
            Start::Pattern(id) => self.open_scheduled_editor(None, Some(&id)),
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
            Start::PersonaTemplate(t) => self.new_persona_editor(&t, &format!("my-{t}")),
        }
    }

    /// `Ctrl+O` on a persona in the editor: its file in `$EDITOR`; a
    /// built-in one is copied into the library first and the copy is what
    /// changes.
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

    /// `Enter` on a task agent, `r` on a persona: the New session dialog
    /// with the agent attached. The first profile on a harness the agent
    /// names a model for is preselected.
    pub fn open_agent_session(&mut self, name: &str) {
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        let Some(spec) = catalog.entry(name).and_then(|e| e.spec.clone()) else {
            self.notice = Some(Notice::error(format!("agent {name} does not load")));
            return;
        };
        if let Some(t) = &spec.task
            && let crate::agents::TaskBody::Pattern(p) = t.body()
        {
            self.notice = Some(Notice::info(format!(
                "{name} runs the {p} loop: give it a schedule (e, When) to run it"
            )));
            return;
        }
        let preferred = spec.backends.keys().next().cloned();
        let profile = preferred.and_then(|h| {
            self.profiles.iter().position(|p| {
                crate::harness::Harness::detect(&p.command).map(|x| x.as_str()) == Some(h.as_str())
            })
        });
        self.open_new_session_dialog(profile);
        if let Mode::NewSession(d) = &mut self.mode {
            d.agent = Some(name.to_string());
        }
    }

    /// The agent's command-line changes on `harness` and its opening
    /// message: the task's prompt, or its skill's invocation (installed
    /// first). A persona has no opening message.
    pub fn session_agent(
        &mut self,
        name: &str,
        harness: Option<crate::harness::Harness>,
        dir: &std::path::Path,
    ) -> Result<(crate::agents::launch::LaunchPlan, Vec<String>), String> {
        use crate::agents::TaskBody;
        use crate::harness::Harness;
        let harness = harness.ok_or(
            "an agent runs on Claude Code, Codex or Antigravity: pick one of their profiles",
        )?;
        let catalog =
            crate::agents::Catalog::load(&self.library_root(), Some(dir).filter(|d| d.is_dir()));
        let spec = catalog
            .entry(name)
            .and_then(|e| e.spec.clone())
            .ok_or_else(|| format!("agent {name} does not load"))?;
        let plan = crate::agents::launch::plan(&spec, harness);
        if harness == Harness::Antigravity {
            crate::agents::launch::install_agy(&spec, &self.skill_home())
                .map_err(|e| format!("agent {name}: {e}"))?;
        }
        let text = match spec.task.as_ref().map(|t| t.body()) {
            None => None,
            Some(TaskBody::Prompt(p)) => Some(p),
            Some(TaskBody::Skill(skill)) => {
                if let Some(pkg) = self.skills.iter().find(|s| s.id == skill).cloned() {
                    crate::skill::install::install(&pkg, harness, &self.skill_home(), false)
                        .map_err(|e| format!("skill {skill}: {e}"))?;
                }
                Some(match harness {
                    Harness::Codex => format!("${skill}"),
                    _ => format!("/{skill}"),
                })
            }
            Some(TaskBody::Pattern(p)) => {
                return Err(format!("{name} runs the {p} loop on its schedule"));
            }
        };
        let opening = match (text, harness) {
            (None, _) => Vec::new(),
            (Some(t), Harness::Antigravity) => vec!["--prompt-interactive".into(), t],
            (Some(t), _) => vec![t],
        };
        Ok((plan, opening))
    }

    /// `f` on a session: the New session dialog on the same profile and
    /// folder, continuing that session (`DialogState::handoff`).
    pub fn open_handoff_dialog(&mut self, idx: usize) {
        let Some(s) = self.sessions.get(idx) else {
            return;
        };
        let (from, dir) = (s.id, s.dir.to_string_lossy().into_owned());
        let profile = self.profiles.iter().position(|p| p.name == s.profile.name);
        self.open_new_session_dialog(profile);
        if let Mode::NewSession(d) = &mut self.mode {
            d.handoff = Some(from);
            d.dir = dir;
            d.dir_edited = true;
            d.dir_picker.refresh(&d.dir);
        }
    }

    /// How session `from` continues on `harness` in `dir`: a fork of its
    /// conversation on the same Claude Code or Codex, else its whole
    /// transcript written under `<runtime>/handoffs/` and a first message
    /// that has the new session read it.
    pub fn session_handoff(
        &mut self,
        from: usize,
        harness: Option<crate::harness::Harness>,
        dir: &std::path::Path,
    ) -> Result<crate::handoff::Continue, String> {
        use crate::handoff::{self, Continue};
        use crate::harness::Harness;
        let harness = harness.ok_or("continue on a Claude Code, Codex or Antigravity profile")?;
        let s = self
            .sessions
            .iter()
            .find(|s| s.id == from)
            .ok_or("the session to continue is gone")?;
        let from_harness = Harness::detect(&s.profile.command)
            .ok_or("that session's CLI is not one agent-mux knows")?;
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let from_provider = match from_harness {
            Harness::Antigravity => "antigravity",
            h => h.as_str(),
        };
        // the launch's conversation; else the one it resumed, by id
        let stored = s
            .trace
            .as_ref()
            .zip(conn.as_ref())
            .and_then(|(t, c)| {
                crate::tracing::store::query::launch_transcript(c, &t.launch_id)
                    .ok()
                    .flatten()
            })
            .or_else(|| {
                let id = s.conversation.clone()?;
                let path = conn.as_ref().and_then(|c| {
                    crate::tracing::store::query::conversation_transcript(c, from_provider, &id)
                        .ok()
                        .flatten()
                });
                Some((from_provider.to_string(), id, path))
            });
        let conversation = stored
            .as_ref()
            .map(|(_, id, _)| id.clone())
            .ok_or(
                "agent-mux has not seen this session's conversation yet (is tracing on, and has it had a turn?)",
            )?;
        if harness == from_harness && crate::harness::can_fork(harness) {
            return Ok(Continue::Fork(conversation));
        }
        let provider = stored
            .as_ref()
            .and_then(|(p, _, _)| handoff::provider_of(p))
            .or_else(|| handoff::provider_of(from_provider))
            .ok_or("unknown provider")?;
        let path = stored
            .and_then(|(_, _, p)| p)
            .ok_or("the trace store has no transcript file for this session")?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let name = format!(
            "{}-{}",
            provider.as_str(),
            conversation.chars().take(8).collect::<String>()
        );
        let file = handoff::write(
            &self.workflows_runtime_dir().join("handoffs"),
            &name,
            &handoff::render(provider, &text),
        )
        .map_err(|e| format!("handoff: {e}"))?;
        let message = handoff::opening_message(provider, dir, &file);
        Ok(Continue::Transcript(match harness {
            Harness::Antigravity => vec!["--prompt-interactive".into(), message],
            _ => vec![message],
        }))
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
