//! Context-sensitive application actions. The menu and key dispatch use
//! the same availability checks, including after the selection changes.
use super::{App, Focus, Mode, Notice};
use crate::keymap::{ActionId, ACTIONS};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability { Enabled, Disabled(String) }
impl Availability {
    pub fn enabled(&self) -> bool { matches!(self, Self::Enabled) }
    pub fn reason(&self) -> Option<&str> { match self { Self::Enabled => None, Self::Disabled(s) => Some(s) } }
}

impl App {
    pub fn command_availability(&self, id: ActionId) -> Availability {
        use ActionId::*;
        let main_list = self.list_focused();
        let editor = matches!(self.mode, Mode::AgentEditor(_) | Mode::Markdown(_));
        let has_session = self.sessions.get(self.selected).is_some();
        let supported = match id {
            Commands | Help | FocusAgents | TestShortcuts => true,
            Quit => true,
            ReturnToSession | PrevSession | NextSession => has_session,
            ToggleSidebar => matches!(self.mode, Mode::Main | Mode::TraceBrowser(_)),
            Traces => has_session,
            Runs => true,
            SetVerdict => matches!(&self.mode, Mode::TraceBrowser(b) if !b.observations.is_empty()),
            Save => editor || matches!(self.mode, Mode::RunsView(_)),
            New => main_list || matches!(self.mode, Mode::ConfigView(_) | Mode::AgentEditor(_)),
            Edit => main_list || matches!(self.mode, Mode::ConfigView(_) | Mode::SkillsView(_) | Mode::RunsView(_) | Mode::Markdown(_)),
            Delete => main_list || matches!(self.mode, Mode::ConfigView(_) | Mode::RunsView(_) | Mode::Inbox(_) | Mode::AgentEditor(_)),
            Run => main_list || matches!(self.mode, Mode::AgentEditor(_) | Mode::RunsView(_)),
            Stop => (main_list && has_session) || matches!(self.mode, Mode::RunsView(_)),
            Reload => matches!(self.mode, Mode::ConfigView(_) | Mode::SkillsView(_)),
            Restore => matches!(self.mode, Mode::AgentEditor(_) | Mode::ConfigView(_)),
            Editor => editor || matches!(self.mode, Mode::ConfigView(_) | Mode::TraceBrowser(_) | Mode::RunsView(_)),
        };
        if supported { Availability::Enabled } else {
            Availability::Disabled(match id {
                ReturnToSession | PrevSession | NextSession => "No session available",
                ToggleSidebar => "This screen has no sidebar control",
                _ => "Unavailable for this screen or selection",
            }.into())
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
            ReturnToSession => { if let Some(id) = self.return_session_id() { self.request_navigation(NavigationTarget::Session(id), now); } },
            ToggleSidebar => {
                if let Mode::TraceBrowser(b) = &mut self.mode { b.with_sidebar = !b.with_sidebar; }
                else { self.toggle_sidebar(); }
            }
            Traces => self.request_navigation(NavigationTarget::Traces, now),
            Runs => self.request_navigation(NavigationTarget::Runs, now),
            PrevSession => self.select_adjacent_session(-1),
            NextSession => self.select_adjacent_session(1),
            Quit => self.request_navigation(NavigationTarget::Quit, now),
            SetVerdict => { if let Mode::TraceBrowser(b) = &mut self.mode {
                let result = b.cycle_score(); self.notice = Some(match result {
                    Ok(s) => Notice::info(s), Err(e) => Notice::error(e),
                });
            } }
            Commands => self.command_overlay = Some(Overlay::Commands(CommandMenu::default())),
            Help => self.command_overlay = Some(Overlay::Help),
            TestShortcuts => {},
            _ => {
                if let Some(c) = ACTIONS.iter().find(|a| a.id == id).and_then(|a| a.key) {
                    let modifiers = if matches!(id, Reload | Editor) { KeyModifiers::CONTROL } else { KeyModifiers::NONE };
                    self.handle_key(&KeyEvent::new(KeyCode::Char(c), modifiers), now);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct CommandMenu { pub query: String, pub selected: usize }
#[derive(Debug)]
pub enum Overlay { Commands(CommandMenu), Help, Navigation(NavigationTarget) }

#[derive(Debug, Clone, Copy)]
pub enum NavigationTarget { Agents, Session(usize), Runs, Traces, Quit }

impl App {
    pub fn filtered_commands(&self) -> Vec<&'static crate::keymap::ActionDef> {
        let query = match &self.command_overlay {
            Some(Overlay::Commands(m)) => m.query.to_lowercase(),
            _ => String::new(),
        };
        ACTIONS.iter().filter(|a| query.split_whitespace().all(|word|
            a.label.to_lowercase().contains(word) || a.keywords.contains(word))).collect()
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
                    KeyCode::Down => m.selected = (m.selected + 1).min(items.len().saturating_sub(1)),
                    KeyCode::Enter => execute = items.get(m.selected).map(|a| a.id),
                    KeyCode::Backspace => { m.query.pop(); m.selected = 0; },
                    KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
                        m.query.push(c); m.selected = 0;
                    }
                    _ => {}
                },
                Some(Overlay::Help) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q' | '?') => close = true,
                    KeyCode::F(1) => self.command_overlay = Some(Overlay::Commands(CommandMenu::default())),
                    _ => {}
                },
                Some(Overlay::Navigation(target)) => {
                    if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                        let target = *target;
                        self.command_overlay = None;
                        self.finish_navigation(target, now);
                    } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) { close = true; }
                }
                None => {}
            }
            if close { self.command_overlay = None; }
            if let Some(id) = execute {
                if self.command_availability(id).enabled() {
                    self.command_overlay = None;
                    self.execute_command(id, now);
                } else { self.notice = Some(Notice::info("This command is unavailable")); }
            }
            return true;
        }
        if key.code == KeyCode::F(1) {
            self.command_overlay = Some(Overlay::Commands(CommandMenu::default()));
            return true;
        }
        if crate::keymap::chord(key) == Some(crate::keymap::Chord::Help) {
            self.command_overlay = Some(Overlay::Help);
            return true;
        }
        false
    }
}

impl App {
    pub fn command_label(&self, id: ActionId) -> Option<String> {
        if !self.command_availability(id).enabled() { return None; }
        if id == ActionId::Commands { return Some("F1".into()); }
        ACTIONS.iter().find(|a| a.id == id).and_then(|a| a.key).map(|c|
            if matches!(id, ActionId::Reload | ActionId::Editor) { format!("Ctrl+{}", c.to_ascii_uppercase()) }
            else { c.to_string() })
    }
}

impl App {
    fn return_session_id(&self) -> Option<usize> {
        self.command_origin.filter(|id| self.sessions.iter().any(|s| s.id == *id))
            .or_else(|| self.sessions.get(self.selected).map(|s| s.id))
    }
    pub fn editor_dirty(&self) -> bool {
        match &self.mode {
            Mode::AgentEditor(st) => st.dirty || match &st.body {
                super::agent_editor::Body::Flow(f) => f.dirty || f.edit.is_some(),
                _ => st.editing || st.tasks.as_ref().is_some_and(|t| t.dirty() || t.edit.is_some()),
            },
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
        } else { self.finish_navigation(target, now); }
    }
    fn finish_navigation(&mut self, target: NavigationTarget, _now: Instant) {
        match target {
            NavigationTarget::Agents => {
                self.mode = Mode::Main; self.sidebar_hidden = false; self.focus = Focus::List;
                self.recompute_pane_size();
            }
            NavigationTarget::Session(id) => {
                if let Some(index) = self.session_index(id).or_else(|| self.sessions.get(self.selected).map(|_| self.selected)) {
                    self.mode = Mode::Main; self.selected = index; self.focus_pane();
                } else { self.notice = Some(Notice::info("No session available")); }
            }
            NavigationTarget::Runs => self.open_runs_view(),
            NavigationTarget::Traces => self.open_trace_browser(),
            NavigationTarget::Quit => self.quit_from_chord(),
        }
    }
}
