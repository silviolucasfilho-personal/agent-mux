//! Modifier protocol support is not evidence that a host shortcut arrives.
//! Preferences last only for this App instance, and probes never execute.
use super::{App, Notice};
use crate::keymap::{self, ActionId, Chord};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ShortcutState {
    observed: HashMap<ActionId, String>,
}
impl ShortcutState {
    pub fn observe(&mut self, id: ActionId, key: KeyEvent) {
        if shortcut_action(&key) == Some(id) {
            self.observed.insert(id, event_label(&key));
        }
    }
    pub fn preferred_label(&self, id: ActionId) -> Option<String> {
        self.observed.get(&id).cloned()
    }
}

pub const PROBES: &[ActionId] = &[
    ActionId::Commands,
    ActionId::FocusAgents,
    ActionId::ToggleSidebar,
    ActionId::Help,
    ActionId::Traces,
];
#[derive(Debug, Default)]
pub struct ShortcutProbe {
    pub step: usize,
    pub last_event: Option<String>,
}
impl ShortcutProbe {
    pub fn current(&self) -> Option<ActionId> {
        PROBES.get(self.step).copied()
    }
}

pub fn shortcut_action(key: &KeyEvent) -> Option<ActionId> {
    if key.code == KeyCode::F(1) {
        return Some(ActionId::Commands);
    }
    match keymap::chord(key)? {
        Chord::Commands => Some(ActionId::Commands),
        Chord::FocusAgents => Some(ActionId::FocusAgents),
        Chord::ReturnSession => Some(ActionId::ReturnToSession),
        Chord::ToggleSidebar => Some(ActionId::ToggleSidebar),
        Chord::Help => Some(ActionId::Help),
        Chord::Traces => Some(ActionId::Traces),
        Chord::PrevSession => Some(ActionId::PrevSession),
        Chord::NextSession => Some(ActionId::NextSession),
        Chord::Quit => Some(ActionId::Quit),
        _ => None,
    }
}

pub fn candidates(id: ActionId) -> &'static str {
    match id {
        ActionId::Commands => "F1 / ⌘P / Ctrl+Shift+P",
        ActionId::FocusAgents => "⌘L / Ctrl+Shift+L",
        ActionId::ReturnToSession => "⌘Enter / Ctrl+Shift+Enter",
        ActionId::ToggleSidebar => "⌘B / Ctrl+Shift+B",
        ActionId::Help => "⌘/ / Ctrl+Shift+/",
        ActionId::Traces => "F3 / ⌘I / Ctrl+Shift+I",
        ActionId::Quit => "F10 / Ctrl+Shift+Q",
        ActionId::PrevSession => "⌘↑ / Ctrl+Shift+↑",
        ActionId::NextSession => "⌘↓ / Ctrl+Shift+↓",
        _ => "Commands",
    }
}
fn event_label(key: &KeyEvent) -> String {
    let mut label = String::new();
    if key.modifiers.contains(KeyModifiers::SUPER) {
        label.push('⌘');
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        label.push_str("Ctrl+");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        label.push_str("Alt+");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        label.push_str("Shift+");
    }
    label.push_str(&match key.code {
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        _ => format!("{:?}", key.code),
    });
    label
}

impl App {
    pub fn handle_shortcut_probe(&mut self, key: &KeyEvent) -> bool {
        let Some(probe) = self.shortcut_probe.as_mut() else {
            return false;
        };
        let Some(key) = crate::keys::normalize_event(*key) else {
            return true;
        };
        if key.code == KeyCode::Esc {
            self.shortcut_probe = None;
            return true;
        }
        let skip = key.code == KeyCode::Char(' ') && key.modifiers.is_empty();
        let observed = probe
            .current()
            .filter(|id| shortcut_action(&key) == Some(*id));
        if let Some(id) = observed {
            self.shortcuts.observe(id, key);
        }
        if skip || observed.is_some() {
            probe.step += 1;
            probe.last_event = None;
            if probe.current().is_none() {
                self.shortcut_probe = None;
                self.notice = Some(Notice::info(
                    "Shortcut test complete; observed bindings apply to this app session",
                ));
            }
        } else {
            probe.last_event = Some(format!(
                "Received {} — no matching shortcut",
                event_label(&key)
            ));
        }
        true
    }
}
