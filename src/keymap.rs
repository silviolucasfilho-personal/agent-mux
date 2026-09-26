//! The keymap every screen shares: one verb, one key. A screen may leave a
//! verb out, but never gives its key another meaning. The help overlay and
//! the footer hints are written from `VERBS`, so a hint cannot drift from
//! the key that does it, and the labels follow the platform (`⌃J` on
//! macOS, `Ctrl+J` elsewhere).
//!
//! | Key | Verb |
//! | --- | --- |
//! | `Enter` | open, choose, confirm |
//! | `Esc` / `q` | back one level; close at the top (`q` outside text) |
//! | `n` | new, in the place you are |
//! | `e` | edit the selected item |
//! | `d` | delete / remove / dismiss, with a y/n confirm |
//! | `x` | stop something running: kill a session, cancel a run |
//! | `s` | save (`Ctrl+S` too, inside forms) |
//! | `r` | run / run now / resume |
//! | `Ctrl+R` | reload / rescan |
//! | `R` | restore the built-in |
//! | `J` / `K` | move the selected item down / up |
//! | `g` / `G` | top / bottom (`Home` / `End` too) |
//! | `Ctrl+D` / `Ctrl+U` | page down / up (`PgDn` / `PgUp` too) |
//! | `1`-`9` | tab or screen |
//! | `Ctrl+O` | open in `$EDITOR` |
//!
//! Text fields: `Ctrl+J` new line (and `⌥↩` / `⇧↩` where the terminal sends
//! them), `Ctrl+A` / `Ctrl+E` start / end, `Ctrl+W` or `⌥⌫` delete word,
//! `Ctrl+U` clear, `Ctrl+V` paste, `Ctrl+O` compose in `$EDITOR`.

use crate::app::text_area::TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const MACOS: bool = cfg!(target_os = "macos");

/// A verb of the shared keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Open,
    Back,
    New,
    Edit,
    Delete,
    Stop,
    Save,
    Run,
    Reload,
    Restore,
    MoveDown,
    MoveUp,
    Top,
    Bottom,
    PageDown,
    PageUp,
    Editor,
    Help,
}

/// `(verb, key label, key label on macOS, meaning)`.
pub const VERBS: &[(Verb, &str, &str, &str)] = &[
    (Verb::Open, "Enter", "↩", "open, choose, confirm"),
    (
        Verb::Back,
        "Esc / q",
        "Esc / q",
        "back one level; close at the top",
    ),
    (Verb::New, "n", "n", "new, in the place you are"),
    (Verb::Edit, "e", "e", "edit the selected item"),
    (Verb::Delete, "d", "d", "delete or remove (asks first)"),
    (Verb::Stop, "x", "x", "stop: kill a session, cancel a run"),
    (Verb::Save, "s", "s", "save"),
    (Verb::Run, "r", "r", "run, run now, resume"),
    (Verb::Reload, "Ctrl+R", "⌃R", "reload, rescan"),
    (Verb::Restore, "R", "R", "restore the built-in"),
    (Verb::MoveDown, "J", "J", "move the item down"),
    (Verb::MoveUp, "K", "K", "move the item up"),
    (Verb::Top, "g", "g", "top"),
    (Verb::Bottom, "G", "G", "bottom"),
    (Verb::PageDown, "Ctrl+D", "⌃D", "page down"),
    (Verb::PageUp, "Ctrl+U", "⌃U", "page up"),
    (Verb::Editor, "Ctrl+O", "⌃O", "open in $EDITOR"),
    (Verb::Help, "?", "?", "help"),
];

/// The label of a verb's key on this platform.
pub fn label(v: Verb) -> &'static str {
    label_for(v, MACOS)
}

pub fn label_for(v: Verb, macos: bool) -> &'static str {
    VERBS
        .iter()
        .find(|(x, ..)| *x == v)
        .map(|(_, k, m, _)| if macos { *m } else { *k })
        .unwrap_or("")
}

/// Labels of the text-field keys, platform-aware.
pub fn newline_label() -> &'static str {
    if MACOS { "⌃J" } else { "Ctrl+J" }
}

pub fn ctrl_label(c: char) -> String {
    if MACOS {
        format!("⌃{}", c.to_ascii_uppercase())
    } else {
        format!("Ctrl+{}", c.to_ascii_uppercase())
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// The verb a key means outside a text field.
pub fn verb(key: &KeyEvent) -> Option<Verb> {
    let c = ctrl(key);
    Some(match key.code {
        KeyCode::Enter => Verb::Open,
        KeyCode::Esc => Verb::Back,
        KeyCode::Char('q') if !c => Verb::Back,
        KeyCode::Char('n') if !c => Verb::New,
        KeyCode::Char('e') if !c => Verb::Edit,
        KeyCode::Char('d') if !c => Verb::Delete,
        KeyCode::Delete => Verb::Delete,
        KeyCode::Char('x') if !c => Verb::Stop,
        KeyCode::Char('s') => Verb::Save,
        KeyCode::Char('r') if c => Verb::Reload,
        KeyCode::Char('r') => Verb::Run,
        KeyCode::Char('R') => Verb::Restore,
        KeyCode::Char('J') => Verb::MoveDown,
        KeyCode::Char('K') => Verb::MoveUp,
        KeyCode::Char('g') | KeyCode::Home => Verb::Top,
        KeyCode::Char('G') | KeyCode::End => Verb::Bottom,
        KeyCode::Char('d') if c => Verb::PageDown,
        KeyCode::PageDown => Verb::PageDown,
        KeyCode::Char('u') if c => Verb::PageUp,
        KeyCode::PageUp => Verb::PageUp,
        KeyCode::Char('o') if c => Verb::Editor,
        KeyCode::Char('?') | KeyCode::F(1) => Verb::Help,
        _ => return None,
    })
}

/// The canonical key of a list alias: `g`/`G` → `Home`/`End`, `Ctrl+D`/
/// `Ctrl+U` → `PgDn`/`PgUp`. Views that already handle the canonical keys
/// get the Mac-friendly ones for free; nothing else changes.
pub fn list_alias(key: &KeyEvent) -> KeyEvent {
    let to = |code| KeyEvent::new(code, KeyModifiers::NONE);
    match key.code {
        KeyCode::Char('g') if key.modifiers.is_empty() => to(KeyCode::Home),
        KeyCode::Char('G') => to(KeyCode::End),
        KeyCode::Char('d') if ctrl(key) => to(KeyCode::PageDown),
        KeyCode::Char('u') if ctrl(key) => to(KeyCode::PageUp),
        _ => *key,
    }
}

/// What a key does in a text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKey {
    Insert(char),
    Newline,
    Backspace,
    DeleteForward,
    DeleteWord,
    Clear,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Paste,
    Editor,
    /// Not a text key: the field's owner decides (Enter, Esc, Tab …).
    Other,
}

/// The shared text-field keys. `Enter` is never a new line: it submits,
/// and the owner handles it (`Other`).
pub fn text_key(key: &KeyEvent) -> TextKey {
    let c = ctrl(key);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Enter if alt || shift => TextKey::Newline,
        KeyCode::Char('j') if c => TextKey::Newline,
        KeyCode::Char('a') if c => TextKey::Home,
        KeyCode::Char('e') if c => TextKey::End,
        KeyCode::Char('w') if c => TextKey::DeleteWord,
        KeyCode::Backspace if alt || c => TextKey::DeleteWord,
        KeyCode::Char('u') if c => TextKey::Clear,
        KeyCode::Char('v') if c => TextKey::Paste,
        KeyCode::Char('o') if c => TextKey::Editor,
        KeyCode::Backspace => TextKey::Backspace,
        KeyCode::Delete => TextKey::DeleteForward,
        KeyCode::Left => TextKey::Left,
        KeyCode::Right => TextKey::Right,
        KeyCode::Up => TextKey::Up,
        KeyCode::Down => TextKey::Down,
        KeyCode::Home => TextKey::Home,
        KeyCode::End => TextKey::End,
        KeyCode::Char(ch) if !c => TextKey::Insert(ch),
        _ => TextKey::Other,
    }
}

/// Applies an editing key to `t`; returns the key when it was not one
/// (`Paste`, `Editor` and `Other` are the owner's to handle).
pub fn apply_text(t: &mut TextArea, key: &KeyEvent) -> Option<TextKey> {
    match text_key(key) {
        TextKey::Insert(ch) => t.insert(ch),
        TextKey::Newline => t.newline(),
        TextKey::Backspace => t.backspace(),
        TextKey::DeleteForward => t.delete(),
        TextKey::DeleteWord => t.delete_word(),
        TextKey::Clear => t.set(""),
        TextKey::Left => {
            t.left();
        }
        TextKey::Right => {
            t.right();
        }
        TextKey::Up => {
            t.up();
        }
        TextKey::Down => {
            t.down();
        }
        TextKey::Home => t.home(),
        TextKey::End => t.end(),
        other => return Some(other),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn every_verb_has_one_key_and_a_label() {
        for (v, key, mac, what) in VERBS {
            assert!(
                !key.is_empty() && !mac.is_empty() && !what.is_empty(),
                "{v:?}"
            );
        }
        assert_eq!(
            verb(&k(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(Verb::New)
        );
        assert_eq!(
            verb(&k(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Verb::Delete)
        );
        assert_eq!(
            verb(&k(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Verb::PageDown)
        );
        assert_eq!(
            verb(&k(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(Verb::Reload)
        );
        assert_eq!(
            verb(&k(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(Verb::Run)
        );
        assert_eq!(
            verb(&k(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Verb::Editor)
        );
        assert_eq!(label_for(Verb::Editor, true), "⌃O");
        assert_eq!(label_for(Verb::Editor, false), "Ctrl+O");
    }

    #[test]
    fn list_aliases_become_the_canonical_keys() {
        assert_eq!(
            list_alias(&k(KeyCode::Char('g'), KeyModifiers::NONE)).code,
            KeyCode::Home
        );
        assert_eq!(
            list_alias(&k(KeyCode::Char('G'), KeyModifiers::SHIFT)).code,
            KeyCode::End
        );
        assert_eq!(
            list_alias(&k(KeyCode::Char('d'), KeyModifiers::CONTROL)).code,
            KeyCode::PageDown
        );
        assert_eq!(
            list_alias(&k(KeyCode::Char('u'), KeyModifiers::CONTROL)).code,
            KeyCode::PageUp
        );
        assert_eq!(
            list_alias(&k(KeyCode::Char('x'), KeyModifiers::NONE)).code,
            KeyCode::Char('x')
        );
    }

    #[test]
    fn text_fields_take_the_mac_keys() {
        let mut t = TextArea::new("hello world");
        apply_text(&mut t, &k(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 0);
        apply_text(&mut t, &k(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), t.text.len());
        apply_text(&mut t, &k(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(t.text, "hello ");
        apply_text(&mut t, &k(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(t.text, "hello \n");
        assert_eq!(
            apply_text(&mut t, &k(KeyCode::Enter, KeyModifiers::NONE)),
            Some(TextKey::Other),
            "Enter submits; it is the owner's"
        );
        assert_eq!(
            apply_text(&mut t, &k(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(TextKey::Editor)
        );
        apply_text(&mut t, &k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(t.text.is_empty());
    }
}
