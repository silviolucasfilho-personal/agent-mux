//! Context-sensitive application actions. The menu and key dispatch use
//! the same availability checks, including after the selection changes.
use super::{App, Focus, Mode, Notice};
use crate::keymap::{ACTIONS, ActionId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Enabled,
    Disabled(String),
}
impl Availability {
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Enabled => None,
            Self::Disabled(s) => Some(s),
        }
    }
}

impl App {
    pub fn command_availability(&self, id: ActionId) -> Availability {
        use super::agents_list::AgentKind;
        use super::runs_view::RunRef;
        use ActionId::*;
        let main_list = self.list_focused();
        let row = main_list.then(|| self.agent_row()).flatten();
        let session = self.sessions.get(self.selected);
        let editor = matches!(self.mode, Mode::AgentEditor(_) | Mode::Markdown(_));
        let editor_library = matches!(&self.mode, Mode::AgentEditor(st) if st.tasks.is_some() || matches!(st.body, super::agent_editor::Body::Flow(_)));
        let runs_item = match &self.mode {
            Mode::RunsView(st) => st.selected_item(),
            _ => None,
        };
        let config_item = matches!(&self.mode, Mode::ConfigView(v) if v.selected_asset().is_some());
        let skills_package =
            matches!(&self.mode, Mode::SkillsView(v) if v.selected_package().is_some());
        let supported = match id {
            SubmitDialog => matches!(self.mode, Mode::NewSession(_) | Mode::WorkflowDialog(_)),
            Commands | Help | FocusAgents | TestShortcuts | Quit | Runs | Skills
            | Configuration | Inbox | History => true,
            ReturnToSession => self.return_session_id().is_some(),
            PrevSession | NextSession => session.is_some(),
            ToggleSidebar => matches!(self.mode, Mode::Main | Mode::TraceBrowser(_)),
            Traces => true,
            SetVerdict => matches!(&self.mode, Mode::TraceBrowser(b) if !b.turns.is_empty()),
            FindNext | FindPrev => {
                matches!(&self.mode, Mode::TraceBrowser(b) if b.zoomed && b.find.is_some())
            }
            FilterClaude | FilterCodex | FilterAntigravity => {
                matches!(self.mode, Mode::SkillsView(_))
            }
            Save => editor || runs_item.is_some_and(|i| i.run.is_flow()),
            New => main_list || matches!(self.mode, Mode::ConfigView(_)) || editor_library,
            Edit => {
                matches!(
                    row,
                    Some(AgentKind::Persona(_) | AgentKind::Loop(_) | AgentKind::Flow(_))
                ) || config_item
                    || skills_package
                    || runs_item.is_some()
                    || matches!(self.mode, Mode::Markdown(_) | Mode::AgentEditor(_))
            }
            Delete => {
                matches!(row, Some(AgentKind::Loop(_)))
                    || (main_list
                        && self.sidebar_section == super::SidebarSection::Workflows
                        && matches!(
                            self.selected_workflow_row(),
                            Some(super::workflows::WorkflowRow::Planned(_))
                        ))
                    || (matches!(row, Some(AgentKind::Session(_)))
                        && session.is_some_and(|s| {
                            matches!(s.status(Instant::now()), crate::status::Status::Exited(_))
                        }))
                    || runs_item.is_some_and(|i| matches!(i.run, RunRef::Inbox(_)))
                    || matches!(self.mode, Mode::Inbox(_))
                    || editor_library
            }
            Run => {
                matches!(
                    row,
                    Some(
                        AgentKind::Persona(_)
                            | AgentKind::Loop(_)
                            | AgentKind::Flow(_)
                            | AgentKind::Skill(_)
                    )
                ) || (matches!(row, Some(AgentKind::Session(_)))
                    && session.is_some_and(|s| {
                        matches!(s.status(Instant::now()), crate::status::Status::Exited(_))
                    }))
                    || (main_list
                        && self.sidebar_section == super::SidebarSection::History
                        && !self.history_sessions.is_empty())
                    || matches!(self.mode, Mode::AgentEditor(_))
                    || runs_item.is_some()
                    || skills_package
                    || matches!(&self.mode, Mode::TraceBrowser(b) if !b.sessions.is_empty())
                    || matches!(&self.mode, Mode::SessionHistory(h) if !h.sessions.is_empty())
            }
            Stop => {
                (matches!(row, Some(AgentKind::Session(_)))
                    && session.is_some_and(|s| {
                        !matches!(s.status(Instant::now()), crate::status::Status::Exited(_))
                    }))
                    || matches!(row, Some(AgentKind::Flow(_)))
                    || runs_item.is_some_and(|i| {
                        matches!(i.run, RunRef::LoopLive { .. } | RunRef::FlowLive(_))
                    })
            }
            Reload => matches!(
                self.mode,
                Mode::ConfigView(_) | Mode::SkillsView(_) | Mode::RunsView(_) | Mode::Markdown(_)
            ),
            Restore => {
                editor_library
                    || config_item
                    || matches!(&self.mode, Mode::AgentEditor(st) if matches!(&st.body, super::agent_editor::Body::Persona(p) if p.copy_of_builtin))
            }
            Editor => {
                editor
                    || config_item
                    || matches!(self.mode, Mode::TraceBrowser(_))
                    || runs_item.is_some_and(|i| {
                        matches!(i.run, RunRef::Inbox(super::inbox::InboxItem::Plan { .. }))
                    })
            }
        };
        if self.confirmation_active() && !matches!(id, Commands | Help | TestShortcuts) {
            return Availability::Disabled("Finish the pending confirmation first".into());
        }
        if self.text_entry_active()
            && !matches!(id, Save | Editor | SubmitDialog)
            && ACTIONS
                .iter()
                .find(|a| a.id == id)
                .is_some_and(|a| matches!(a.scope, crate::keymap::ActionScope::Local))
        {
            return Availability::Disabled("Finish editing the field first".into());
        }
        if supported {
            Availability::Enabled
        } else {
            Availability::Disabled(
                match id {
                    ReturnToSession | PrevSession | NextSession => "No session available",
                    ToggleSidebar => "This screen has no sidebar control",
                    _ => "Unavailable for this screen or selection",
                }
                .into(),
            )
        }
    }

    pub fn execute_command(&mut self, id: ActionId, now: Instant) {
        if let Availability::Disabled(reason) = self.command_availability(id) {
            self.notice = Some(Notice::info(reason));
            return;
        }
        use ActionId::*;
        match id {
            FocusAgents => self.request_navigation(NavigationTarget::Agents, now),
            ReturnToSession => {
                if let Some(id) = self.return_session_id() {
                    self.request_navigation(NavigationTarget::Session(id), now);
                }
            }
            ToggleSidebar => {
                if let Mode::TraceBrowser(b) = &mut self.mode {
                    b.with_sidebar = !b.with_sidebar;
                } else {
                    self.toggle_sidebar();
                }
            }
            Traces => self.request_navigation(NavigationTarget::Traces, now),
            Runs => self.request_navigation(NavigationTarget::Runs, now),
            PrevSession => self.select_adjacent_session(-1),
            NextSession => self.select_adjacent_session(1),
            Quit => self.request_navigation(NavigationTarget::Quit, now),
            SubmitDialog => {
                self.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), now)
            }
            Skills => self.request_navigation(NavigationTarget::Skills, now),
            Configuration => self.request_navigation(NavigationTarget::Configuration, now),
            Inbox => self.request_navigation(NavigationTarget::Inbox, now),
            History => self.request_navigation(NavigationTarget::History, now),
            FilterClaude | FilterCodex | FilterAntigravity => {
                if let Mode::SkillsView(v) = &mut self.mode {
                    v.toggle_filter(match id {
                        FilterClaude => crate::harness::Harness::Claude,
                        FilterCodex => crate::harness::Harness::Codex,
                        _ => crate::harness::Harness::Antigravity,
                    });
                }
            }
            FindNext | FindPrev => {
                if let Mode::TraceBrowser(b) = &mut self.mode {
                    b.find_next(id == FindNext);
                }
            }
            SetVerdict => {
                if let Mode::TraceBrowser(b) = &mut self.mode {
                    let result = b.cycle_score();
                    self.notice = Some(match result {
                        Ok(s) => Notice::info(s),
                        Err(e) => Notice::error(e),
                    });
                }
            }
            Delete
                if self.list_focused()
                    && matches!(
                        self.agent_row(),
                        Some(super::agents_list::AgentKind::Session(_))
                    ) =>
            {
                if let Some(session) = self.sessions.get(self.selected) {
                    self.command_overlay = Some(Overlay::DeleteSession(session.id));
                }
            }
            Commands => self.command_overlay = Some(Overlay::Commands(CommandMenu::default())),
            Help => self.command_overlay = Some(Overlay::Help(0)),
            TestShortcuts => {
                self.shortcut_probe = Some(super::shortcut_probe::ShortcutProbe::default())
            }
            Save if matches!(&self.mode, Mode::RunsView(st) if st.save_name.is_some()) => {
                self.command_dispatching = true;
                self.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now);
                self.command_dispatching = false;
            }
            _ => {
                if let Some(c) = ACTIONS.iter().find(|a| a.id == id).and_then(|a| a.key) {
                    if id == Save
                        && let Mode::Markdown(view) = &mut self.mode
                    {
                        view.save();
                        return;
                    }
                    let modifiers = if matches!(id, Reload | Editor)
                        || (id == Save && matches!(self.mode, Mode::AgentEditor(_)))
                    {
                        KeyModifiers::CONTROL
                    } else {
                        KeyModifiers::NONE
                    };
                    self.command_dispatching = true;
                    self.handle_key(&KeyEvent::new(KeyCode::Char(c), modifiers), now);
                    self.command_dispatching = false;
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct CommandMenu {
    pub query: String,
    pub selected: usize,
}
#[derive(Debug)]
pub enum Overlay {
    Commands(CommandMenu),
    Help(usize),
    DeleteSession(usize),
    Navigation(NavigationTarget),
}

#[derive(Debug, Clone, Copy)]
pub enum NavigationTarget {
    Agents,
    Session(usize),
    Runs,
    Traces,
    Quit,
    Skills,
    Configuration,
    Inbox,
    History,
}

impl App {
    pub fn filtered_commands(&self) -> Vec<&'static crate::keymap::ActionDef> {
        let query = match &self.command_overlay {
            Some(Overlay::Commands(m)) => m.query.to_lowercase(),
            _ => String::new(),
        };
        ACTIONS
            .iter()
            .filter(|a| {
                query
                    .split_whitespace()
                    .all(|word| a.label.to_lowercase().contains(word) || a.keywords.contains(word))
            })
            .collect()
    }

    /// Global overlay entry runs before text/search handlers. Underlying
    /// mode and buffers are never replaced to display help or Commands.
    pub fn handle_command_overlay(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if self.command_overlay.is_some() {
            let items = self.filtered_commands();
            let mut execute = None;
            let mut close = false;
            match &mut self.command_overlay {
                Some(Overlay::Commands(m)) => match key.code {
                    KeyCode::Esc | KeyCode::F(1) => close = true,
                    KeyCode::Up => m.selected = m.selected.saturating_sub(1),
                    KeyCode::Down => {
                        m.selected = (m.selected + 1).min(items.len().saturating_sub(1))
                    }
                    KeyCode::Enter => execute = items.get(m.selected).map(|a| a.id),
                    KeyCode::Backspace => {
                        m.query.pop();
                        m.selected = 0;
                    }
                    KeyCode::Char(c)
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        m.query.push(c);
                        m.selected = 0;
                    }
                    _ => {}
                },
                Some(Overlay::Help(selected)) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | '?') => close = true,
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => *selected = (*selected + 1).min(ACTIONS.len() + 2),
                    KeyCode::Home => *selected = 0,
                    KeyCode::End => *selected = ACTIONS.len() + 2,
                    KeyCode::PageDown => *selected = (*selected + 10).min(ACTIONS.len() + 2),
                    KeyCode::PageUp => *selected = selected.saturating_sub(10),
                    KeyCode::F(1) => {
                        self.command_overlay = Some(Overlay::Commands(CommandMenu::default()))
                    }
                    _ => {}
                },
                Some(Overlay::DeleteSession(id)) => {
                    if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                        let id = *id;
                        self.command_overlay = None;
                        if let Some(index) = self.session_index(id) {
                            self.selected = index;
                            self.apply(super::Action::RemoveSelected, key, now);
                        }
                    } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) {
                        close = true;
                    }
                }
                Some(Overlay::Navigation(target)) => {
                    if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                        let target = *target;
                        self.command_overlay = None;
                        self.finish_navigation(target, now);
                    } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) {
                        close = true;
                    }
                }
                None => {}
            }
            if close {
                self.command_overlay = None;
            }
            if let Some(id) = execute {
                if self.command_availability(id).enabled() {
                    self.command_overlay = None;
                    self.execute_command(id, now);
                } else {
                    self.notice = Some(Notice::info("This command is unavailable"));
                }
            }
            return true;
        }
        if let Some(id) = super::shortcut_probe::shortcut_action(key) {
            self.shortcuts.observe(id, *key);
            if id == ActionId::Traces && matches!(self.mode, Mode::TraceBrowser(_)) {
                self.request_navigation(
                    self.return_session_id()
                        .map(NavigationTarget::Session)
                        .unwrap_or(NavigationTarget::Agents),
                    now,
                );
            } else {
                self.execute_command(id, now);
            }
            return true;
        }
        false
    }
}

impl App {
    pub fn command_label(&self, id: ActionId) -> Option<String> {
        if !self.command_availability(id).enabled() {
            return None;
        }
        if let Some(label) = self.shortcuts.preferred_label(id) {
            return Some(label);
        }
        if id == ActionId::Commands {
            return Some("F1".into());
        }
        self.local_key_label(id)
    }

    /// The key a local action answers to on this screen.
    fn local_key_label(&self, id: ActionId) -> Option<String> {
        ACTIONS
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.key)
            .map(|c| {
                if matches!(id, ActionId::Reload | ActionId::Editor)
                    || (id == ActionId::Save
                        && matches!(self.mode, Mode::AgentEditor(_) | Mode::Markdown(_)))
                {
                    format!("Ctrl+{}", c.to_ascii_uppercase())
                } else {
                    c.to_string()
                }
            })
    }

    /// The shortcut the Commands menu shows beside `id`, available or
    /// not, and whether it is known to reach agent-mux here: observed by
    /// Test shortcuts, a function key or a plain key. A modifier chord
    /// not yet observed is a candidate (`false`): the terminal may keep it.
    pub fn command_shortcut(&self, id: ActionId) -> Option<(String, bool)> {
        if let Some(label) = self.shortcuts.preferred_label(id) {
            return Some((label, true));
        }
        if let Some(s) = crate::app::shortcut_probe::default_shortcut(id) {
            return Some((s.to_string(), s.starts_with('F')));
        }
        self.local_key_label(id).map(|l| (l, true))
    }
}

impl App {
    fn return_session_id(&self) -> Option<usize> {
        self.command_origin
            .filter(|id| self.sessions.iter().any(|s| s.id == *id))
            .or_else(|| self.sessions.get(self.selected).map(|s| s.id))
    }
    pub fn editor_dirty(&self) -> bool {
        match &self.mode {
            Mode::AgentEditor(st) => {
                st.dirty
                    || match &st.body {
                        super::agent_editor::Body::Flow(f) => {
                            f.dirty || f.edit.is_some() || f.overlay.is_some()
                        }
                        _ => {
                            st.editing
                                || st.tasks.as_ref().is_some_and(|t| {
                                    t.dirty() || t.edit.is_some() || t.overlay.is_some()
                                })
                        }
                    }
            }
            Mode::Markdown(v) => v.dirty(),
            _ => false,
        }
    }
    pub fn request_navigation(&mut self, target: NavigationTarget, now: Instant) {
        if matches!(self.mode, Mode::Main) {
            self.command_origin = self.sessions.get(self.selected).map(|s| s.id);
        }
        if self.editor_dirty() {
            self.command_overlay = Some(Overlay::Navigation(target));
        } else {
            self.finish_navigation(target, now);
        }
    }
    fn finish_navigation(&mut self, target: NavigationTarget, _now: Instant) {
        self.search = None;
        match target {
            NavigationTarget::Agents => {
                self.mode = Mode::Main;
                self.sidebar_hidden = false;
                self.focus = Focus::List;
                self.recompute_pane_size();
            }
            NavigationTarget::Session(id) => {
                if let Some(index) = self
                    .session_index(id)
                    .or_else(|| self.sessions.get(self.selected).map(|_| self.selected))
                {
                    self.mode = Mode::Main;
                    self.selected = index;
                    self.focus_pane();
                } else {
                    self.notice = Some(Notice::info("No session available"));
                }
            }
            NavigationTarget::Runs => self.open_runs_view(),
            NavigationTarget::Traces => self.open_trace_browser(),
            NavigationTarget::Quit => self.quit_from_chord(),
            NavigationTarget::Skills => self.open_skills_view(),
            NavigationTarget::Configuration => self.open_config_view(),
            NavigationTarget::Inbox => self.open_inbox(),
            NavigationTarget::History => self.apply(
                super::Action::OpenSessionHistory,
                &KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
                _now,
            ),
        }
    }
}

impl App {
    pub fn keyboard_focus_short(&self) -> &'static str {
        match &self.mode {
            Mode::Main if self.focus == Focus::List => "Agents",
            Mode::Main => "Session",
            Mode::AgentEditor(_) => "Editor",
            Mode::RunsView(_) => "Runs",
            Mode::TraceBrowser(_) => "Traces",
            Mode::SkillsView(_) => "Skills",
            Mode::ConfigView(_) => "Configuration",
            Mode::Markdown(_) => "Markdown",
            _ => "Dialog",
        }
    }
    pub fn keyboard_focus_label(&self) -> String {
        match &self.mode {
            Mode::Main if self.focus == Focus::Pane => self
                .sessions
                .get(self.selected)
                .map(|s| format!("Session • Keyboard goes to {}", s.profile.name))
                .unwrap_or_else(|| "Session • No session available".into()),
            Mode::AgentEditor(st) if st.editing => format!(
                "Editing {} • Keyboard focus",
                st.current().map(|f| f.label()).unwrap_or("field")
            ),
            Mode::RunsView(st) => format!(
                "Runs {} • Keyboard focus",
                if st.detail_focused { "detail" } else { "list" }
            ),
            _ => format!("{} • Keyboard focus", self.keyboard_focus_short()),
        }
    }
    pub fn local_hints(&self) -> String {
        ACTIONS
            .iter()
            .filter_map(|a| {
                let key = a.key?;
                self.command_availability(a.id).enabled().then(|| {
                    let label = self.command_label(a.id).unwrap_or_else(|| key.to_string());
                    format!(" [{label}] {}", a.label)
                })
            })
            .take(4)
            .collect::<String>()
    }
}

impl App {
    pub fn text_entry_active(&self) -> bool {
        if self.search.is_some() {
            return true;
        }
        match &self.mode {
            Mode::NewSession(_) | Mode::WorkflowDialog(_) | Mode::SkillLauncher(_) => true,
            Mode::AgentEditor(st) => {
                st.editing
                    || match &st.body {
                        super::agent_editor::Body::Flow(f) => {
                            f.edit.is_some() || f.overlay.is_some()
                        }
                        _ => st
                            .tasks
                            .as_ref()
                            .is_some_and(|t| t.edit.is_some() || t.overlay.is_some()),
                    }
            }
            Mode::Markdown(v) => v.pane == super::markdown_view::Pane::Edit,
            Mode::TraceBrowser(b) => b.search_input.is_some(),
            Mode::ConfigView(v) => matches!(v.pending, super::Pending::NewName { .. }),
            Mode::RunsView(v) => v.save_name.is_some(),
            _ => false,
        }
    }
    pub(super) fn confirmation_active(&self) -> bool {
        match &self.mode {
            Mode::ConfirmKill | Mode::ConfirmQuit | Mode::ConfirmRemoveLoop => true,
            Mode::AgentEditor(st) => {
                st.confirm_discard
                    || st.confirm_restore
                    || match &st.body {
                        super::agent_editor::Body::Flow(f) => matches!(
                            f.overlay,
                            Some(super::flow_builder::Overlay::Confirm { .. })
                        ),
                        _ => st.tasks.as_ref().is_some_and(|t| {
                            matches!(
                                t.overlay,
                                Some(super::loop_builder::Overlay::Confirm { .. })
                            )
                        }),
                    }
            }
            Mode::Markdown(v) => v.confirm_close,
            Mode::RunsView(v) => v.confirm_discard,
            Mode::ConfigView(v) => {
                matches!(v.pending, super::Pending::Reset | super::Pending::Push)
            }
            _ => false,
        }
    }
    pub fn local_command(&self, key: &KeyEvent) -> Option<ActionId> {
        if self.text_entry_active()
            || self.confirmation_active()
            || (matches!(self.mode, Mode::Main) && self.focus == Focus::Pane)
        {
            return None;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        ACTIONS
            .iter()
            .find(|a| {
                a.key.is_some_and(|c| key.code == KeyCode::Char(c))
                    && ctrl == matches!(a.id, ActionId::Reload | ActionId::Editor)
            })
            .map(|a| a.id)
            .or_else(|| (ctrl && key.code == KeyCode::Char('s')).then_some(ActionId::Save))
    }
    pub fn handle_local_command(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if self.command_dispatching {
            return false;
        }
        let Some(id) = self.local_command(key) else {
            return false;
        };
        self.execute_command(id, now);
        true
    }
}
