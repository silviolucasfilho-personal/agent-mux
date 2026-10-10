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
//!
//! # The chord layer
//!
//! The main screen has no modes: focus is in the list or in the pane, and
//! with the pane focused every plain key, `Ctrl+letter`, `Alt`, `Esc` and
//! `Tab` goes to the harness. agent-mux's own keys that must work while
//! typing into a harness live in the layer GUI terminals reserve for the
//! application: `⌘` on macOS, `Ctrl+Shift` on Windows and Linux (and on
//! macOS too, for terminals that keep `⌘`). Terminals that report no
//! modifiers at all (Terminal.app, plain xterm) deliver `Ctrl+Shift+B` as
//! `Ctrl+B`, so the two chords that matter also have a protocol-free form:
//! `F2` switches focus, `F3` opens the traces and `F1` opens help. `CHORDS` is the whole set; the
//! status bar, the help overlay and the welcome card print from it with
//! `chord_label`. `agent-mux keys` shows what a terminal delivers.
//!
//! Probed 2026-10-01 (`agent-mux keys`, see `src/keys_cli.rs`): the
//! `Ctrl+Shift` forms need the kitty keyboard protocol on Unix (Ghostty,
//! kitty, WezTerm, foot, iTerm2 ≥ 3.5, Alacritty ≥ 0.13, VTE ≥ 0.78) and
//! arrive natively on Windows; `⌘` arrives as `SUPER` only from terminals
//! that pass unbound `⌘` chords through. A chord the host terminal binds
//! itself (`⌘F` find in Terminal.app, iTerm2 and Ghostty; `Ctrl+Shift+F`
//! in GNOME Terminal and kitty; `⌘↑`/`⌘↓` marks in Terminal.app and iTerm2)
//! never reaches agent-mux: its twin, or `F2`, still works.
//!
//! Ghostty 1.3.1 on macOS (`ghostty +list-keybinds --default`, 2026-10-02):
//! the kitty protocol is supported; `⌘E` (search_selection), `⌘F`
//! (start_search) and `⌘J` (scroll_to_selection) are the terminal's, while
//! `⌘B`, `⌘/`, `⌘↑`, `⌘↓` and the letters h i l m o p r s u x y are unbound
//! and no `Ctrl+Shift` chord is bound. Whether unbound `⌘` chords arrive
//! as `SUPER` was probed on 2026-10-09 (`agent-mux keys`, Ghostty 1.3.1,
//! default config): `⌘E`, `⌘B`, `⌘I` and `⌘L` arrive as `SUPER`, so its
//! `search_selection` on `⌘E` takes the key only over a selection, and
//! `Ctrl+Shift+I` and `F3` arrive as well.

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

/// A key of the chord layer: works with either focus, never reaches the
/// harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    /// `⌘E` / `Ctrl+Shift+E` / `F2`: focus list ⇄ pane.
    ToggleFocus,
    /// `⌘B` / `Ctrl+Shift+B`: show or hide the sidebar.
    ToggleSidebar,
    /// `⌘↑` / `Ctrl+Shift+↑`: select the previous session.
    PrevSession,
    /// `⌘↓` / `Ctrl+Shift+↓`: select the next session.
    NextSession,
    /// `⌘F` / `Ctrl+Shift+F`: search the pane's scrollback.
    Find,
    /// `Ctrl+Shift+C`: copy the selection (`⌘C` is the host's).
    Copy,
    /// `Ctrl+Shift+V`: paste into the pane (`⌘V` is the host's).
    Paste,
    /// `⌘/` / `Ctrl+Shift+/` / `F1`: help.
    Help,
    /// `⌘I` / `Ctrl+Shift+I` / `F3`: the Trace Browser on this session,
    /// and back.
    Traces,
    /// `⌃⇧Q` / `Ctrl+Shift+Q` / `F10`: quit agent-mux from the pane. Not
    /// `⌘Q`, which every macOS terminal keeps to quit itself; asks first
    /// while a session runs.
    Quit,
    /// `Shift+↑`: scroll three lines back.
    LineUp,
    /// `Shift+↓`: scroll three lines forward.
    LineDown,
    /// `PgUp` (`Fn+↑`): one page back.
    PageUp,
    /// `PgDn` (`Fn+↓`): one page forward.
    PageDown,
    /// `Shift+Home`: the oldest line.
    Top,
    /// `Shift+End`: back to live.
    Bottom,
}

/// `(chord, macOS label, label elsewhere, protocol-free label, meaning)`.
/// The protocol-free label is empty when the chord has no such form.
pub const CHORDS: &[(Chord, &str, &str, &str, &str)] = &[
    (
        Chord::ToggleFocus,
        "⌘E",
        "Ctrl+Shift+E",
        "F2",
        "focus list ⇄ pane",
    ),
    (Chord::ToggleSidebar, "⌘B", "Ctrl+Shift+B", "", "sidebar"),
    (
        Chord::PrevSession,
        "⌘↑",
        "Ctrl+Shift+↑",
        "",
        "previous session",
    ),
    (Chord::NextSession, "⌘↓", "Ctrl+Shift+↓", "", "next session"),
    (Chord::Find, "⌘F", "Ctrl+Shift+F", "", "find in the pane"),
    (Chord::Copy, "⌃⇧C", "Ctrl+Shift+C", "", "copy the selection"),
    (
        Chord::Paste,
        "⌃⇧V",
        "Ctrl+Shift+V",
        "",
        "paste into the pane",
    ),
    (Chord::Help, "⌘/", "Ctrl+Shift+/", "F1", "help"),
    (
        Chord::Traces,
        "⌘I",
        "Ctrl+Shift+I",
        "F3",
        "traces of this session",
    ),
    (
        Chord::Quit,
        "⌃⇧Q",
        "Ctrl+Shift+Q",
        "F10",
        "quit agent-mux (asks while a session runs)",
    ),
    (
        Chord::LineUp,
        "⇧↑",
        "Shift+↑",
        "Shift+↑",
        "scroll three lines back",
    ),
    (
        Chord::LineDown,
        "⇧↓",
        "Shift+↓",
        "Shift+↓",
        "scroll three lines forward",
    ),
    (Chord::PageUp, "Fn+↑", "PgUp", "PgUp", "one page back"),
    (Chord::PageDown, "Fn+↓", "PgDn", "PgDn", "one page forward"),
    (
        Chord::Top,
        "⇧Fn+←",
        "Shift+Home",
        "Shift+Home",
        "the oldest line",
    ),
    (
        Chord::Bottom,
        "⇧Fn+→",
        "Shift+End",
        "Shift+End",
        "back to live",
    ),
];

/// The chord a key is, if any. `⌘` is `KeyModifiers::SUPER`, which only a
/// terminal with the kitty keyboard protocol reports; the `Ctrl+Shift`
/// forms are matched wherever the event carries both modifiers, since a
/// terminal that delivers them meant them. Plain `Ctrl+letter` is never a
/// chord: it is the harness's.
pub fn chord(key: &KeyEvent) -> Option<Chord> {
    let m = key.modifiers;
    let shift = m.contains(KeyModifiers::SHIFT);
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let sup = m.contains(KeyModifiers::SUPER);
    let app = sup || (ctrl && shift);
    let letter = |c: char, want: char| c.eq_ignore_ascii_case(&want);
    Some(match key.code {
        KeyCode::F(2) => Chord::ToggleFocus,
        KeyCode::F(1) => Chord::Help,
        KeyCode::F(3) => Chord::Traces,
        KeyCode::F(10) => Chord::Quit,
        KeyCode::Char(c) if app && letter(c, 'e') => Chord::ToggleFocus,
        KeyCode::Char(c) if app && letter(c, 'b') => Chord::ToggleSidebar,
        KeyCode::Char(c) if app && letter(c, 'f') => Chord::Find,
        KeyCode::Char(c) if app && letter(c, 'i') => Chord::Traces,
        KeyCode::Char(c) if ctrl && shift && letter(c, 'c') => Chord::Copy,
        KeyCode::Char(c) if ctrl && shift && letter(c, 'v') => Chord::Paste,
        KeyCode::Char(c) if ctrl && shift && letter(c, 'q') => Chord::Quit,
        KeyCode::Char('/') | KeyCode::Char('?') if app => Chord::Help,
        KeyCode::Up if app => Chord::PrevSession,
        KeyCode::Down if app => Chord::NextSession,
        KeyCode::Up if shift => Chord::LineUp,
        KeyCode::Down if shift => Chord::LineDown,
        // macOS keyboards report Fn+↑/Fn+↓ as a bare PageUp/PageDown
        KeyCode::PageUp => Chord::PageUp,
        KeyCode::PageDown => Chord::PageDown,
        KeyCode::Home if shift => Chord::Top,
        KeyCode::End if shift => Chord::Bottom,
        _ => return None,
    })
}

/// The label of a chord for this platform and terminal: the `⌘` form on
/// macOS, the `Ctrl+Shift` form elsewhere, and the protocol-free form
/// (`F2`, `F1`) when the terminal reports no modifiers (`enhanced` false
/// on Unix).
pub fn chord_label(c: Chord, enhanced: bool) -> &'static str {
    chord_label_for(c, MACOS, cfg!(windows) || enhanced)
}

pub fn chord_label_for(c: Chord, macos: bool, modifiers_reported: bool) -> &'static str {
    let Some((_, mac, other, bare, _)) = CHORDS.iter().find(|(x, ..)| *x == c) else {
        return "";
    };
    if !modifiers_reported && !bare.is_empty() {
        bare
    } else if macos {
        mac
    } else {
        other
    }
}

/// The meaning of a chord, for the help overlay.
pub fn chord_meaning(c: Chord) -> &'static str {
    CHORDS
        .iter()
        .find(|(x, ..)| *x == c)
        .map(|(.., m)| *m)
        .unwrap_or("")
}

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

    #[test]
    fn chords_are_the_os_layer_and_never_a_plain_ctrl_key() {
        let cs = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(chord(&k(KeyCode::Char('E'), cs)), Some(Chord::ToggleFocus));
        assert_eq!(
            chord(&k(KeyCode::Char('e'), KeyModifiers::SUPER)),
            Some(Chord::ToggleFocus)
        );
        assert_eq!(
            chord(&k(KeyCode::F(2), KeyModifiers::NONE)),
            Some(Chord::ToggleFocus)
        );
        assert_eq!(
            chord(&k(KeyCode::Char('b'), KeyModifiers::SUPER)),
            Some(Chord::ToggleSidebar)
        );
        assert_eq!(
            chord(&k(KeyCode::Up, KeyModifiers::SUPER)),
            Some(Chord::PrevSession)
        );
        assert_eq!(chord(&k(KeyCode::Down, cs)), Some(Chord::NextSession));
        for traces in [
            k(KeyCode::Char('i'), KeyModifiers::SUPER),
            k(KeyCode::Char('I'), cs),
            k(KeyCode::F(3), KeyModifiers::NONE),
        ] {
            assert_eq!(chord(&traces), Some(Chord::Traces), "{traces:?}");
        }
        assert_eq!(
            chord(&k(KeyCode::Char('i'), KeyModifiers::CONTROL)),
            None,
            "plain Ctrl+I is the harness's"
        );
        for quit in [
            k(KeyCode::Char('Q'), cs),
            k(KeyCode::F(10), KeyModifiers::NONE),
        ] {
            assert_eq!(chord(&quit), Some(Chord::Quit), "{quit:?}");
        }
        for harness in [
            k(KeyCode::Char('q'), KeyModifiers::CONTROL),
            k(KeyCode::Char('q'), KeyModifiers::SUPER),
        ] {
            assert_eq!(chord(&harness), None, "{harness:?} is not a quit");
        }
        assert_eq!(
            chord(&k(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(Chord::LineUp)
        );
        assert_eq!(
            chord(&k(KeyCode::Char('/'), KeyModifiers::SUPER)),
            Some(Chord::Help)
        );
        assert_eq!(
            chord(&k(KeyCode::F(1), KeyModifiers::NONE)),
            Some(Chord::Help)
        );
        // the harness's keys
        assert_eq!(chord(&k(KeyCode::Char('b'), KeyModifiers::CONTROL)), None);
        assert_eq!(chord(&k(KeyCode::Char('q'), KeyModifiers::CONTROL)), None);
        assert_eq!(chord(&k(KeyCode::Char('e'), KeyModifiers::NONE)), None);
        assert_eq!(chord(&k(KeyCode::Esc, KeyModifiers::NONE)), None);
        assert_eq!(chord(&k(KeyCode::Up, KeyModifiers::NONE)), None);
        // ⌘C/⌘V are the host's; only Ctrl+Shift copies and pastes here
        assert_eq!(chord(&k(KeyCode::Char('c'), KeyModifiers::SUPER)), None);
        assert_eq!(chord(&k(KeyCode::Char('C'), cs)), Some(Chord::Copy));
    }

    #[test]
    fn chord_labels_follow_the_platform_and_the_terminal() {
        assert_eq!(chord_label_for(Chord::ToggleFocus, true, true), "⌘E");
        assert_eq!(
            chord_label_for(Chord::ToggleFocus, false, true),
            "Ctrl+Shift+E"
        );
        assert_eq!(chord_label_for(Chord::ToggleFocus, true, false), "F2");
        assert_eq!(chord_label_for(Chord::Help, false, false), "F1");
        // no protocol-free form: the OS label stands
        assert_eq!(chord_label_for(Chord::ToggleSidebar, true, false), "⌘B");
        assert_eq!(chord_meaning(Chord::Find), "find in the pane");
    }
}
