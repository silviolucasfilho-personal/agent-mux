//! The agent editor (spec `docs/superpowers/specs/2026-09-26-agent-first-design.md`,
//! section 5): one screen for every agent, in five tabs. **Who** runs it
//! (profile, model, persona fields), **What** it does (the task or the
//! steps), **When** it runs (on demand or every interval), **Limits** (what
//! it may change, budgets) and **Review**. The body is the agent's own
//! state: a scheduled agent is the loop dialog's values, a flow is the flow
//! builder's draft, a persona is the agent form; each saves where it always
//! did.

use super::flow_builder::{After, AgentField, AgentForm, FlowBuilderState, Screen, text_key};
use super::loops::{LoopDialogState, LoopField};
use super::text_area::TextArea;
use super::{App, Mode, Notice};
use crate::keymap::{Verb, verb};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Who,
    What,
    When,
    Limits,
    Review,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Who, Tab::What, Tab::When, Tab::Limits, Tab::Review];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Who => "Who",
            Tab::What => "What",
            Tab::When => "When",
            Tab::Limits => "Limits",
            Tab::Review => "Review",
        }
    }
}

/// A persona agent in the form, with where it came from.
#[derive(Debug)]
pub struct PersonaBody {
    pub form: AgentForm,
    /// The name it was loaded under; `None` for a new one.
    pub original: Option<String>,
}

#[derive(Debug)]
pub enum Body {
    Scheduled(Box<LoopDialogState>),
    Flow(Box<FlowBuilderState>),
    Persona(Box<PersonaBody>),
}

#[derive(Debug)]
pub struct AgentEditorState {
    pub tab: Tab,
    pub body: Body,
    /// The field under the cursor, an index into `fields()`.
    pub field: usize,
    /// A text field is being typed.
    pub editing: bool,
    /// "Leave without saving?" is showing.
    pub confirm_discard: bool,
    pub dirty: bool,
    pub error: Option<String>,
}

/// One field of the scheduled or persona body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdField {
    Loop(LoopField),
    Agent(AgentField),
}

impl EdField {
    pub fn label(self) -> &'static str {
        match self {
            EdField::Loop(f) => match f {
                LoopField::Workspace => "In",
                LoopField::Pattern => "Task",
                LoopField::Profile => "On",
                LoopField::Model => "Model",
                LoopField::VerifierModel => "Checked by",
                LoopField::Every => "Every",
                LoopField::Level => "May edit files",
                LoopField::MaxRuns => "Runs a day",
                LoopField::MaxTokens => "Tokens a day",
                LoopField::MaxCost => "Cost a run",
                LoopField::Scaffold => "Set up files",
            },
            EdField::Agent(f) => match f {
                AgentField::Name => "Name",
                AgentField::Purpose => "What it is for",
                AgentField::Instructions => "Instructions",
                AgentField::Tools => "Tools",
                AgentField::Mcp => "MCP servers",
                AgentField::ModelClaude => "Claude model",
                AgentField::ModelCodex => "Codex model",
                AgentField::ModelAgy => "Antigravity model",
                AgentField::EffortCodex => "Codex effort",
                AgentField::EffortAgy => "Antigravity effort",
                AgentField::SaveTo => "Saved in",
            },
        }
    }

    /// `Enter` types it (the rest change with `←`/`→`/`Space`).
    pub fn is_text(self) -> bool {
        match self {
            EdField::Loop(f) => matches!(
                f,
                LoopField::Workspace
                    | LoopField::Model
                    | LoopField::VerifierModel
                    | LoopField::Every
                    | LoopField::MaxRuns
                    | LoopField::MaxTokens
                    | LoopField::MaxCost
            ),
            EdField::Agent(f) => matches!(
                f,
                AgentField::Name
                    | AgentField::Purpose
                    | AgentField::Instructions
                    | AgentField::Mcp
                    | AgentField::ModelClaude
                    | AgentField::ModelCodex
                    | AgentField::ModelAgy
            ),
        }
    }
}

impl AgentEditorState {
    fn new(body: Body) -> Self {
        let mut st = AgentEditorState {
            tab: Tab::Who,
            body,
            field: 0,
            editing: false,
            confirm_discard: false,
            dirty: false,
            error: None,
        };
        st.sync_flow_tab();
        st.sync_loop_field();
        st
    }

    pub fn flow(st: FlowBuilderState) -> Self {
        Self::new(Body::Flow(Box::new(st)))
    }

    /// The agent's name, for the header.
    pub fn name(&self) -> String {
        match &self.body {
            Body::Scheduled(d) => d
                .editing
                .clone()
                .or_else(|| d.pattern().map(|p| p.id.clone()))
                .unwrap_or_else(|| "new scheduled agent".into()),
            Body::Flow(f) => f.draft.name().to_string(),
            Body::Persona(p) => {
                let n = p.form.name.text.trim();
                if n.is_empty() {
                    "new persona".into()
                } else {
                    n.to_string()
                }
            }
        }
    }

    pub fn kind(&self) -> &'static str {
        match &self.body {
            Body::Scheduled(_) => "scheduled",
            Body::Flow(_) => "flow",
            Body::Persona(_) => "persona",
        }
    }

    /// The fields the tab shows (none: the tab is a panel of facts).
    pub fn fields(&self) -> Vec<EdField> {
        use AgentField as A;
        use LoopField as L;
        match &self.body {
            Body::Scheduled(_) => match self.tab {
                Tab::Who => vec![L::Profile, L::Model, L::VerifierModel],
                Tab::What => vec![L::Pattern],
                Tab::When => vec![L::Every, L::Workspace, L::Scaffold],
                Tab::Limits => vec![L::Level, L::MaxRuns, L::MaxTokens, L::MaxCost],
                Tab::Review => vec![],
            }
            .into_iter()
            .map(EdField::Loop)
            .collect(),
            Body::Persona(p) => {
                let who: Vec<AgentField> = if p.original.is_some() {
                    vec![A::Purpose]
                } else {
                    vec![A::Name, A::Purpose]
                };
                match self.tab {
                    Tab::Who => who
                        .into_iter()
                        .chain([
                            A::ModelClaude,
                            A::ModelCodex,
                            A::ModelAgy,
                            A::EffortCodex,
                            A::EffortAgy,
                        ])
                        .collect(),
                    Tab::What => vec![A::Instructions],
                    Tab::Limits => vec![A::Tools, A::Mcp],
                    Tab::When | Tab::Review => vec![],
                }
                .into_iter()
                .map(EdField::Agent)
                .collect()
            }
            Body::Flow(_) => vec![],
        }
    }

    pub fn current(&self) -> Option<EdField> {
        self.fields().get(self.field).copied()
    }

    /// Selects `tab`; a flow moves its builder to the matching screen.
    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.field = 0;
        self.editing = false;
        if let Body::Flow(f) = &mut self.body {
            match tab {
                Tab::Who => {
                    f.screen = Screen::Steps;
                    f.selected = 0;
                    f.focus = super::flow_builder::Focus::Fields;
                    f.field = 0;
                }
                Tab::What => {
                    f.screen = Screen::Steps;
                    if f.selected == 0 && !f.draft.is_empty() {
                        f.selected = 1;
                    }
                    f.focus = super::flow_builder::Focus::List;
                }
                Tab::Review => {
                    f.screen = Screen::Review;
                    f.review_scroll = 0;
                }
                Tab::When | Tab::Limits => {}
            }
        }
        self.sync_loop_field();
    }

    /// A flow's tab follows its builder's screen (When and Limits are the
    /// editor's own panels).
    fn sync_flow_tab(&mut self) {
        if let Body::Flow(f) = &self.body
            && !matches!(self.tab, Tab::When | Tab::Limits)
        {
            self.tab = match f.screen {
                Screen::Review => Tab::Review,
                Screen::Exchange => Tab::What,
                Screen::Steps if f.selected == 0 => Tab::Who,
                Screen::Steps => Tab::What,
            };
        }
    }

    /// Keeps the loop dialog's (or the form's) own field on the editor's,
    /// so their cycling and text helpers act on it.
    fn sync_loop_field(&mut self) {
        let cur = self.current();
        match (&mut self.body, cur) {
            (Body::Scheduled(d), Some(EdField::Loop(f))) => d.field = f,
            (Body::Persona(p), Some(EdField::Agent(f))) => p.form.field = f,
            _ => {}
        }
    }

    /// The text being typed, for `Ctrl+O`.
    pub fn text_mut(&mut self) -> Option<&mut TextArea> {
        match &mut self.body {
            Body::Persona(p) if self.editing => p.form.text_mut(),
            _ => None,
        }
    }
}

/// A new persona's form from an agent file's text.
fn form_from_text(text: &str) -> Option<AgentForm> {
    let spec = crate::agents::AgentSpec::parse(text).ok()?;
    Some(AgentForm::from_spec(&spec))
}

impl App {
    pub(crate) fn agent_editor_mode(st: AgentEditorState) -> Mode {
        Mode::AgentEditor(Box::new(st))
    }

    /// A scheduled agent in the editor: `id` an existing one, `pattern`
    /// the task a new one starts from.
    pub fn open_scheduled_editor(&mut self, id: Option<String>, pattern: Option<&str>) {
        let choices = self.workspace_choices();
        let mut dialog = match id
            .as_deref()
            .and_then(|i| self.loop_registry.find(i).cloned())
        {
            Some(entry) => LoopDialogState::from_entry(&self.profiles, choices, &entry),
            None => LoopDialogState::new(&self.profiles, choices),
        };
        if let Some(p) = pattern
            && let Some(i) = crate::loops::patterns::all().iter().position(|x| x.id == p)
        {
            dialog.field = LoopField::Pattern;
            dialog.pattern_idx = i;
            dialog.cycle(0);
        }
        self.refresh_dialog_audit(&mut dialog);
        let mut st = AgentEditorState::new(Body::Scheduled(Box::new(dialog)));
        st.set_tab(if id.is_some() { Tab::Who } else { Tab::What });
        self.mode = Mode::AgentEditor(Box::new(st));
    }

    /// A persona in the editor: an existing one by name.
    pub fn open_persona_editor(&mut self, name: &str) {
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        let Some(entry) = catalog.entry(name) else {
            return;
        };
        let Some(form) = crate::agents::text_of(entry).and_then(|t| form_from_text(&t)) else {
            // a file that does not load is fixed in the editor it was written in
            self.edit_persona(name);
            return;
        };
        let st = AgentEditorState::new(Body::Persona(Box::new(PersonaBody {
            form,
            original: Some(name.to_string()),
        })));
        self.mode = Mode::AgentEditor(Box::new(st));
    }

    /// A new persona from template `template`, under a free name.
    pub fn new_persona_editor(&mut self, template: &str, base: &str) {
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
        let Some(mut form) = form_from_text(&text) else {
            return;
        };
        if crate::agents::AgentSpec::parse(&text).is_ok_and(|s| s.tools.is_none()) {
            // a template that leaves the tools open starts reading, not editing
            form.tools = [true, false, true, false];
        }
        let mut st = AgentEditorState::new(Body::Persona(Box::new(PersonaBody {
            form,
            original: None,
        })));
        st.dirty = true;
        self.mode = Mode::AgentEditor(Box::new(st));
    }

    pub fn handle_agent_editor_key(&mut self, key: &KeyEvent) {
        let Mode::AgentEditor(mut st) = std::mem::replace(&mut self.mode, Mode::Control) else {
            return;
        };
        match self.agent_editor_key(&mut st, key) {
            After::Stay => self.mode = Mode::AgentEditor(st),
            After::Close => {}
            After::Into(m) => self.mode = *m,
        }
    }

    fn agent_editor_key(&mut self, st: &mut AgentEditorState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Body::Flow(f) = &mut st.body {
            let busy = f.overlay.is_some() || f.edit.is_some();
            let tab = match key.code {
                KeyCode::Char(c @ '1'..='5') if !busy && !ctrl => {
                    Some(Tab::ALL[c as usize - '1' as usize])
                }
                _ => None,
            };
            if let Some(t) = tab {
                st.set_tab(t);
                return After::Stay;
            }
            // When and Limits are panels: save, run and leave still work
            if matches!(st.tab, Tab::When | Tab::Limits) && !busy {
                let passes =
                    matches!(key.code, KeyCode::Char('s' | 'r' | 'q') | KeyCode::Esc) && !ctrl;
                if !passes {
                    return After::Stay;
                }
                f.screen = Screen::Steps;
            }
            let after = self.flow_key(f, key);
            st.sync_flow_tab();
            return after;
        }
        if st.confirm_discard {
            st.confirm_discard = false;
            return match key.code {
                KeyCode::Char('y') | KeyCode::Enter => After::Close,
                _ => After::Stay,
            };
        }
        if st.editing {
            self.agent_editor_type(st, key);
            return After::Stay;
        }
        let n = st.fields().len();
        match key.code {
            KeyCode::Char(c @ '1'..='5') if !ctrl => {
                st.set_tab(Tab::ALL[c as usize - '1' as usize]);
                return After::Stay;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let at = Tab::ALL.iter().position(|t| *t == st.tab).unwrap_or(0);
                let d = if key.code == KeyCode::Tab { 1 } else { 4 };
                st.set_tab(Tab::ALL[(at + d) % 5]);
                return After::Stay;
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                st.field = st.field.saturating_sub(1);
                st.sync_loop_field();
                return After::Stay;
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                st.field = (st.field + 1).min(n.saturating_sub(1));
                st.sync_loop_field();
                return After::Stay;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                self.agent_editor_cycle(st, delta, key.code == KeyCode::Char(' '));
                return After::Stay;
            }
            KeyCode::Enter => {
                match st.current() {
                    Some(f) if f.is_text() => st.editing = true,
                    Some(_) => self.agent_editor_cycle(st, 1, true),
                    None if st.tab == Tab::Review => return self.agent_editor_save(st, false),
                    None => {}
                }
                return After::Stay;
            }
            KeyCode::Char('s') if !ctrl => return self.agent_editor_save(st, false),
            KeyCode::Char('r') if !ctrl => return self.agent_editor_save(st, true),
            KeyCode::Char('e') if !ctrl && st.tab == Tab::What => {
                if let Body::Scheduled(d) = &st.body {
                    // the task itself is a pattern: the task builder edits it
                    let id = d.pattern().map(|p| p.id.clone());
                    if let Some(bst) = self.loop_builder_state(id.as_deref()) {
                        return After::Into(Box::new(Mode::LoopBuilder(Box::new(bst))));
                    }
                }
                return After::Stay;
            }
            KeyCode::Char('o') if ctrl => {
                if let Body::Scheduled(d) = &st.body
                    && let Some(name) = d
                        .editing
                        .as_deref()
                        .and_then(|id| self.loop_registry.find(id))
                        .and_then(|e| e.agent.clone())
                {
                    // the agent file in $EDITOR; the registry reloads after
                    self.editor_request = Some(super::EditorRequest {
                        path: crate::agents::schedule::file_of(&self.library_root(), &name),
                        asset_id: format!("agent:{name}"),
                        command: crate::assets::editor_command(self.editor.as_deref()),
                    });
                    return After::Close;
                }
                if let Body::Scheduled(d) = &st.body
                    && d.editing.is_some()
                {
                    self.notice = Some(Notice::info(
                        "this loop keeps its settings in loops.json: `agent-mux agent migrate --write` gives it an agent file",
                    ));
                    return After::Stay;
                }
                if let Body::Persona(p) = &st.body
                    && let Some(name) = p.original.clone()
                {
                    // the whole file, in $EDITOR
                    self.edit_persona(&name);
                    return After::Close;
                }
                return After::Stay;
            }
            _ => {}
        }
        match verb(&crate::keymap::list_alias(key)) {
            Some(Verb::Back) => {
                if st.dirty {
                    st.confirm_discard = true;
                    After::Stay
                } else {
                    After::Close
                }
            }
            Some(Verb::Top) => {
                st.field = 0;
                st.sync_loop_field();
                After::Stay
            }
            Some(Verb::Bottom) => {
                st.field = n.saturating_sub(1);
                st.sync_loop_field();
                After::Stay
            }
            _ => After::Stay,
        }
    }

    /// `←`/`→`/`Space` on a field that is not typed.
    fn agent_editor_cycle(&mut self, st: &mut AgentEditorState, delta: isize, toggle: bool) {
        let Some(field) = st.current() else { return };
        if field.is_text() {
            return;
        }
        match &mut st.body {
            Body::Scheduled(d) => {
                d.cycle(delta);
                if matches!(
                    field,
                    EdField::Loop(LoopField::Pattern | LoopField::Profile)
                ) {
                    self.refresh_dialog_audit(d);
                }
            }
            Body::Persona(p) => {
                let form = &mut p.form;
                match field {
                    EdField::Agent(AgentField::Tools) => {
                        if toggle {
                            form.tools[form.tool_cursor] = !form.tools[form.tool_cursor];
                        } else {
                            form.tool_cursor =
                                (form.tool_cursor as isize + delta).clamp(0, 3) as usize;
                            return;
                        }
                    }
                    EdField::Agent(f @ (AgentField::EffortCodex | AgentField::EffortAgy)) => {
                        let k = usize::from(f == AgentField::EffortAgy);
                        form.efforts[k] = (form.efforts[k] as isize + delta)
                            .rem_euclid(super::flow_builder::EFFORTS.len() as isize)
                            as usize;
                    }
                    _ => return,
                }
            }
            Body::Flow(_) => return,
        }
        st.dirty = true;
        st.error = None;
    }

    /// A key while a text field is typed: `Enter` or `Esc` ends it.
    fn agent_editor_type(&mut self, st: &mut AgentEditorState, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('o') {
            self.agent_editor_open_text(st);
            return;
        }
        match &mut st.body {
            Body::Scheduled(d) => {
                if d.field == LoopField::Workspace {
                    let LoopDialogState {
                        dir_picker,
                        workspace,
                        ..
                    } = &mut **d;
                    match dir_picker.handle_key(key, workspace) {
                        super::dir_picker::PickerEvent::Submit => {
                            d.dir_picker.leave();
                            st.editing = false;
                        }
                        super::dir_picker::PickerEvent::Consumed { path_changed } => {
                            if path_changed {
                                self.refresh_dialog_audit(d);
                                st.dirty = true;
                            }
                        }
                        super::dir_picker::PickerEvent::Ignored => {
                            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                                d.dir_picker.leave();
                                st.editing = false;
                            }
                        }
                    }
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => st.editing = false,
                    KeyCode::Backspace => {
                        if let Some(t) = d.text_mut() {
                            t.pop();
                            st.dirty = true;
                        }
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(t) = d.text_mut() {
                            t.push(c);
                            st.dirty = true;
                        }
                    }
                    _ => {}
                }
            }
            Body::Persona(p) => match key.code {
                KeyCode::Esc => st.editing = false,
                KeyCode::Enter if key.modifiers.is_empty() => st.editing = false,
                _ => {
                    if let Some(t) = p.form.text_mut() {
                        text_key(t, key, ctrl);
                        st.dirty = true;
                    }
                }
            },
            Body::Flow(_) => {}
        }
        st.error = None;
    }

    /// `Ctrl+O` while typing: the text in `$EDITOR`, back into the field.
    fn agent_editor_open_text(&mut self, st: &mut AgentEditorState) {
        let Some(t) = st.text_mut() else { return };
        let dir = self.library_root().join(".editing");
        let path = dir.join("agent-text.md");
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &t.text)) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return;
        }
        self.editor_request = Some(super::EditorRequest {
            path,
            asset_id: "agent-editor:text".into(),
            command: crate::assets::editor_command(self.editor.as_deref()),
        });
    }

    /// After `$EDITOR`: the text returns to the field it came from.
    pub fn agent_editor_text_finished(&mut self, path: &std::path::Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
                return;
            }
        };
        if let Mode::AgentEditor(st) = &mut self.mode
            && let Some(t) = st.text_mut()
        {
            t.set(text.trim_end_matches('\n'));
            st.dirty = true;
        }
    }

    /// `s` saves; `r` saves and runs a scheduled agent now.
    fn agent_editor_save(&mut self, st: &mut AgentEditorState, run: bool) -> After {
        match &mut st.body {
            Body::Scheduled(d) => match self.save_loop_dialog(d) {
                Ok(id) => {
                    d.editing = Some(id);
                    d.scaffold = false;
                    st.dirty = false;
                    st.error = None;
                    if run {
                        self.run_selected_loop_now();
                        return After::Close;
                    }
                }
                Err(e) => st.error = Some(e),
            },
            Body::Persona(p) => match self.save_persona(p) {
                Ok(name) => {
                    p.original = Some(name.clone());
                    st.dirty = false;
                    st.error = None;
                    self.reload_personas();
                    self.notice = Some(Notice::info(format!("agent {name} saved")));
                    if run {
                        self.notice = Some(Notice::info(format!(
                            "agent {name} saved; a persona runs as a flow step or a run's agent"
                        )));
                    }
                }
                Err(e) => st.error = Some(e),
            },
            Body::Flow(_) => {}
        }
        st.field = st.field.min(st.fields().len().saturating_sub(1));
        st.sync_loop_field();
        After::Stay
    }

    /// Writes the persona's file; a built-in one is written as the
    /// library copy that shadows it.
    fn save_persona(&mut self, p: &PersonaBody) -> Result<String, String> {
        let name = p.form.name.text.trim().to_string();
        if !crate::agents::is_valid_name(&name) {
            return Err(format!(
                "{name:?}: an agent name must match ^[a-z][a-z0-9_-]*$"
            ));
        }
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        let path = match (&p.original, catalog.entry(&name)) {
            (Some(_), Some(e)) => e.source.path().to_path_buf(),
            (None, Some(_)) => {
                return Err(format!("an agent called {name} already exists"));
            }
            (_, None) => {
                crate::agents::library_dir(&self.library_root()).join(format!("{name}.toml"))
            }
        };
        let text = p.form.render();
        crate::agents::AgentSpec::parse(&text).map_err(|e| e.join("; "))?;
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, text))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(name)
    }
}
