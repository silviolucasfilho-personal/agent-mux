//! The Markdown viewer and editor: one file, read rendered (`View`) or
//! written as source (`Edit`, with a live preview beside it on a wide
//! screen). Opened by `agent-mux md <file>` (`markdown::cli`) and by a
//! click on a Markdown link in a session's pane (`App::open_link`); the
//! state and its keys are the same in both, the owner only acts on the
//! `Outcome` (close, open a URL, run `$EDITOR`).
//!
//! A relative link to another Markdown file opens it here, `Esc` comes
//! back; a `#heading` link jumps; anything else goes to the platform's
//! handler. Nothing is written until `s` / `Ctrl+S`; closing with unsaved
//! changes asks first.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::text_area::TextArea;
use crate::keymap::{self, TextKey};
use crate::markdown::{self, Doc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    View,
    Edit,
}

/// What the owner of the view must do after a key or a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    /// Open with the platform's handler (`links::open`).
    OpenUrl(String),
    /// Run the external editor on the file, then call `reload`.
    OpenEditor(PathBuf),
}

/// The kind of the last edit, so typing a word undoes as one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
}

const UNDO_DEPTH: usize = 500;

#[derive(Debug)]
pub struct MarkdownView {
    pub path: PathBuf,
    pub text: TextArea,
    /// The text as it is on disk (empty for a file not written yet).
    saved: String,
    pub pane: Pane,
    /// The live preview beside the editor (on a wide enough screen).
    pub preview: bool,
    /// First rendered line shown in `View`.
    pub scroll: usize,
    /// First display column shown, for diagrams and code wider than the
    /// window.
    pub hscroll: usize,
    /// First visual row of the editor; the renderer keeps the cursor in
    /// sight.
    pub edit_top: Cell<usize>,
    /// First rendered line of the preview beside the editor, as drawn.
    pub preview_top: Cell<usize>,
    /// Where the rendered text and the editor's text were last drawn.
    pub view_area: Cell<Rect>,
    pub edit_area: Cell<Rect>,
    /// Closing with unsaved changes: asking.
    pub confirm_close: bool,
    pub notice: Option<String>,
    /// Files this one was reached from, with their scroll.
    back: Vec<(PathBuf, usize)>,
    generation: u64,
    cache: RefCell<Option<(u64, usize, Arc<Doc>)>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<EditKind>,
}

impl MarkdownView {
    /// Opens `path`; a file that does not exist yet opens empty, in `Edit`.
    pub fn open(path: &Path) -> Result<MarkdownView, String> {
        let path = absolute(path);
        let (text, pane) = read(&path)?;
        Ok(MarkdownView {
            text: TextArea::new(text.clone()),
            saved: text,
            pane,
            preview: true,
            scroll: 0,
            hscroll: 0,
            edit_top: Cell::new(0),
            preview_top: Cell::new(0),
            view_area: Cell::new(Rect::new(0, 0, 80, 24)),
            edit_area: Cell::new(Rect::new(0, 0, 80, 24)),
            confirm_close: false,
            notice: None,
            back: Vec::new(),
            generation: 0,
            cache: RefCell::new(None),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            path,
        })
        .map(|mut v| {
            v.text.set_cursor(0);
            v
        })
    }

    pub fn dirty(&self) -> bool {
        self.text.text != self.saved
    }

    /// The document rendered at `width`, cached until the text changes.
    pub fn doc(&self, width: usize) -> Arc<Doc> {
        let mut cache = self.cache.borrow_mut();
        if let Some((g, w, doc)) = cache.as_ref()
            && *g == self.generation
            && *w == width
        {
            return doc.clone();
        }
        let doc = Arc::new(markdown::render(&self.text.text, width));
        *cache = Some((self.generation, width, doc.clone()));
        doc
    }

    fn view_doc(&self) -> Arc<Doc> {
        self.doc(render_width(self.view_area.get()))
    }

    /// The directory relative links resolve against.
    fn base_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn save(&mut self) {
        match std::fs::write(&self.path, &self.text.text) {
            Ok(()) => {
                self.saved = self.text.text.clone();
                self.notice = Some(format!("saved {}", self.path.display()));
            }
            Err(e) => self.notice = Some(format!("could not save: {e}")),
        }
    }

    /// Reads the file again (after `$EDITOR`, or `Ctrl+R`), keeping the
    /// cursor and the scroll where they can stay.
    pub fn reload(&mut self) {
        match read(&self.path) {
            Ok((text, _)) => {
                let cursor = self.text.cursor();
                self.text.set(text.clone());
                self.text.set_cursor(cursor);
                self.saved = text;
                self.changed();
                self.undo.clear();
                self.redo.clear();
                self.notice = Some(format!("reloaded {}", self.path.display()));
            }
            Err(e) => self.notice = Some(e),
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
    }

    // ---- keys -----------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        self.notice = None;
        if self.confirm_close {
            return self.key_confirm(key);
        }
        match self.pane {
            Pane::View => self.key_view(key),
            Pane::Edit => self.key_edit(key),
        }
    }

    fn key_confirm(&mut self, key: &KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('y') => {
                self.save();
                self.confirm_close = false;
                if self.dirty() {
                    Outcome::None
                } else {
                    self.close()
                }
            }
            KeyCode::Char('d') | KeyCode::Char('n') => {
                self.confirm_close = false;
                self.text.set(self.saved.clone());
                self.changed();
                self.close()
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.confirm_close = false;
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    /// Back one file, or close.
    fn close(&mut self) -> Outcome {
        if self.dirty() {
            self.confirm_close = true;
            return Outcome::None;
        }
        match self.back.pop() {
            Some((path, scroll)) => {
                self.load(path);
                self.scroll = scroll;
                Outcome::None
            }
            None => Outcome::Close,
        }
    }

    fn key_view(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = (self.view_area.get().height as usize).max(2) - 1;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return self.close(),
            KeyCode::Char('d') if ctrl => self.scroll_by(page as isize),
            KeyCode::Char('u') if ctrl => self.scroll_by(-(page as isize)),
            KeyCode::Char('r') if ctrl => {
                if self.dirty() {
                    self.notice = Some("unsaved changes: save them (s) first".into());
                } else {
                    self.reload();
                }
            }
            KeyCode::Char('o') if ctrl => return self.external_editor(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page as isize),
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll_by(-(page as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Left | KeyCode::Char('h') => self.hscroll_by(-8),
            KeyCode::Right | KeyCode::Char('l') => self.hscroll_by(8),
            KeyCode::Char(']') => self.heading_step(true),
            KeyCode::Char('[') => self.heading_step(false),
            KeyCode::Char('e') | KeyCode::Char('i') | KeyCode::Enter => {
                let src = self.view_doc().source_for_line(self.scroll);
                self.text.set_cursor(line_start_byte(&self.text.text, src));
                self.edit_top.set(usize::MAX); // the renderer brings the cursor in
                self.pane = Pane::Edit;
            }
            KeyCode::Char('s') => self.save(),
            _ => {}
        }
        Outcome::None
    }

    fn key_edit(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                // the view opens where the cursor was
                let src = line_of(&self.text.text, self.text.cursor());
                self.scroll = self.view_doc().line_for_source(src).min(self.max_scroll());
                self.pane = Pane::View;
                self.last_edit = None;
                return Outcome::None;
            }
            KeyCode::Char('s') if ctrl => self.save(),
            KeyCode::Char('z') if ctrl => self.undo(),
            KeyCode::Char('y') if ctrl => self.redo(),
            KeyCode::Char('p') if ctrl => self.preview = !self.preview,
            KeyCode::Char('r') if ctrl => {
                if self.dirty() {
                    self.notice = Some("unsaved changes: save them (Ctrl+S) first".into());
                } else {
                    self.reload();
                }
            }
            KeyCode::Char('u') if ctrl => self.edit(EditKind::Delete, |t| {
                let start = line_start_byte_at(&t.text, t.cursor());
                let end = t.cursor();
                t.text.replace_range(start..end, "");
                t.set_cursor(start);
            }),
            KeyCode::Home if ctrl => self.move_to(0),
            KeyCode::End if ctrl => self.move_to(self.text.text.len()),
            KeyCode::PageDown => self.move_rows(self.edit_area.get().height as isize - 1),
            KeyCode::PageUp => self.move_rows(-(self.edit_area.get().height as isize - 1)),
            KeyCode::Enter if key.modifiers.is_empty() => self.edit(EditKind::Other, continue_list),
            KeyCode::Tab => self.edit(EditKind::Other, |t| t.insert_str("    ")),
            KeyCode::BackTab => self.edit(EditKind::Other, dedent),
            _ => {
                let kind = match keymap::text_key(key) {
                    TextKey::Insert(c) if !c.is_whitespace() => EditKind::Insert,
                    TextKey::Insert(_) | TextKey::Newline => EditKind::Other,
                    TextKey::Backspace | TextKey::DeleteForward => EditKind::Delete,
                    TextKey::DeleteWord => EditKind::Other,
                    TextKey::Paste => {
                        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                            Ok(s) => self.paste(&s),
                            Err(e) => self.notice = Some(format!("clipboard: {e}")),
                        }
                        return Outcome::None;
                    }
                    TextKey::Editor => return self.external_editor(),
                    TextKey::Clear | TextKey::Other => return Outcome::None,
                    _ => {
                        // movement
                        keymap::apply_text(&mut self.text, key);
                        self.last_edit = None;
                        return Outcome::None;
                    }
                };
                self.edit(kind, |t| {
                    keymap::apply_text(t, key);
                });
            }
        }
        Outcome::None
    }

    /// Pasted text (bracketed paste, or `Ctrl+V`): inserted while editing.
    pub fn paste(&mut self, s: &str) {
        if self.pane == Pane::Edit && !self.confirm_close {
            self.edit(EditKind::Other, |t| t.insert_str(s));
        }
    }

    fn external_editor(&mut self) -> Outcome {
        if self.dirty() {
            self.notice = Some("unsaved changes: save them first".into());
            return Outcome::None;
        }
        Outcome::OpenEditor(self.path.clone())
    }

    /// Applies an edit, keeping an undo step unless it continues the last
    /// one (a word typed, a run of deletes).
    fn edit(&mut self, kind: EditKind, f: impl FnOnce(&mut TextArea)) {
        let before = Snapshot {
            text: self.text.text.clone(),
            cursor: self.text.cursor(),
        };
        f(&mut self.text);
        if self.text.text == before.text {
            return;
        }
        let continues = kind != EditKind::Other && self.last_edit == Some(kind);
        if !continues {
            self.undo.push(before);
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = Some(kind);
        self.changed();
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(s);
        } else {
            self.notice = Some("nothing to undo".into());
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.text.clone(),
            cursor: self.text.cursor(),
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.text.set(s.text);
        self.text.set_cursor(s.cursor);
        self.last_edit = None;
        self.changed();
    }

    fn move_to(&mut self, at: usize) {
        self.text.set_cursor(at);
        self.last_edit = None;
    }

    fn move_rows(&mut self, n: isize) {
        for _ in 0..n.unsigned_abs() {
            let moved = if n < 0 {
                self.text.up()
            } else {
                self.text.down()
            };
            if !moved {
                break;
            }
        }
        self.last_edit = None;
    }

    // ---- view -----------------------------------------------------------

    pub fn max_scroll(&self) -> usize {
        let doc = self.view_doc();
        let h = self.view_area.get().height as usize;
        doc.lines.len().saturating_sub(h.max(1))
    }

    /// Slides the diagrams and code wider than the view `n` columns.
    fn hscroll_by(&mut self, n: isize) {
        let max = self
            .view_doc()
            .max_hscroll(self.view_area.get().width as usize);
        self.hscroll = self.hscroll.min(max).saturating_add_signed(n).min(max);
    }

    fn scroll_by(&mut self, n: isize) {
        self.scroll = self.scroll.saturating_add_signed(n).min(self.max_scroll());
    }

    fn heading_step(&mut self, forward: bool) {
        let doc = self.view_doc();
        let target = if forward {
            doc.headings.iter().find(|h| h.line > self.scroll)
        } else {
            doc.headings.iter().rev().find(|h| h.line < self.scroll)
        };
        match target {
            Some(h) => self.scroll = h.line.min(self.max_scroll()),
            None if !forward => self.scroll = 0,
            None => {}
        }
    }

    /// The heading the top of the view is under.
    pub fn current_heading(&self) -> Option<String> {
        let doc = self.view_doc();
        doc.headings
            .iter()
            .rev()
            .find(|h| h.line <= self.scroll)
            .map(|h| h.text.clone())
    }

    // ---- mouse ----------------------------------------------------------

    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Outcome {
        if self.confirm_close {
            return Outcome::None;
        }
        let in_rect = |r: Rect| {
            ev.column >= r.x
                && ev.column < r.x + r.width
                && ev.row >= r.y
                && ev.row < r.y + r.height
        };
        let editing = self.pane == Pane::Edit;
        let in_editor = editing && in_rect(self.edit_area.get());
        let view = self.view_area.get();
        let in_view = in_rect(view) && (!editing || self.preview);
        match ev.kind {
            // a sideways swipe, or Shift with the wheel
            MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
                if in_view
                    && (matches!(
                        ev.kind,
                        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                    ) || ev.modifiers.contains(KeyModifiers::SHIFT)) =>
            {
                let right = matches!(
                    ev.kind,
                    MouseEventKind::ScrollRight | MouseEventKind::ScrollDown
                );
                self.hscroll_by(if right { 4 } else { -4 });
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let n = if ev.kind == MouseEventKind::ScrollDown {
                    3
                } else {
                    -3
                };
                if in_editor {
                    self.move_rows(n);
                } else if !editing {
                    self.scroll_by(n);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_editor => {
                let area = self.edit_area.get();
                let rows = self.text.rows();
                let r = (self.edit_top.get() + (ev.row - area.y) as usize).min(rows.len() - 1);
                let row = rows[r];
                let col = (ev.column - area.x) as usize;
                let at = self.text.text[row.start..row.end]
                    .char_indices()
                    .nth(col)
                    .map(|(i, _)| row.start + i)
                    .unwrap_or(row.end);
                self.move_to(at);
            }
            MouseEventKind::Down(MouseButton::Left) if in_view => {
                let top = if editing {
                    self.preview_top.get()
                } else {
                    self.scroll
                };
                let line = top + (ev.row - view.y) as usize;
                let doc = self.view_doc();
                let hscroll = self.hscroll.min(doc.max_hscroll(view.width as usize));
                let col = doc.column(line, (ev.column - view.x) as usize, hscroll);
                let url = doc.link_at(line, col).map(str::to_string);
                if let Some(url) = url {
                    return self.follow(&url);
                }
            }
            _ => {}
        }
        Outcome::None
    }

    /// Follows a link from the document.
    pub fn follow(&mut self, url: &str) -> Outcome {
        if let Some(anchor) = url.strip_prefix('#') {
            self.jump_to_anchor(anchor);
            return Outcome::None;
        }
        let (target, anchor) = match url.split_once('#') {
            Some((t, a)) => (t, Some(a)),
            None => (url, None),
        };
        let path = if let Some(p) = file_url_path(target) {
            p
        } else if target.contains("://") || target.starts_with("mailto:") {
            return Outcome::OpenUrl(url.to_string());
        } else {
            self.base_dir().join(percent_decode(target))
        };
        if !path.exists() {
            self.notice = Some(format!("no such file: {}", path.display()));
            return Outcome::None;
        }
        if !markdown::is_markdown_path(&path) {
            return Outcome::OpenUrl(format!("file://{}", absolute(&path).display()));
        }
        if self.dirty() {
            self.notice = Some("unsaved changes: save them (s) before leaving".into());
            return Outcome::None;
        }
        self.back.push((self.path.clone(), self.scroll));
        self.load(path);
        if let Some(a) = anchor {
            self.jump_to_anchor(a);
        }
        Outcome::None
    }

    fn jump_to_anchor(&mut self, anchor: &str) {
        let line = self
            .view_doc()
            .heading_by_slug(&percent_decode(anchor))
            .map(|h| h.line);
        match line {
            Some(l) => {
                self.pane = Pane::View;
                self.scroll = l.min(self.max_scroll());
            }
            None => self.notice = Some(format!("no heading #{anchor}")),
        }
    }

    /// Replaces the file shown, in `View`.
    fn load(&mut self, path: PathBuf) {
        match read(&path) {
            Ok((text, pane)) => {
                self.path = absolute(&path);
                self.text.set(text.clone());
                self.text.set_cursor(0);
                self.saved = text;
                self.pane = pane;
                self.scroll = 0;
                self.hscroll = 0;
                self.edit_top.set(0);
                self.undo.clear();
                self.redo.clear();
                self.last_edit = None;
                self.changed();
            }
            Err(e) => self.notice = Some(e),
        }
    }
}

/// The width a document is rendered at in `area`: a column of margin on
/// the right.
pub fn render_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(1).max(20)
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The file's text and the pane it opens in: `View`, or `Edit` for a file
/// not written yet.
fn read(path: &Path) -> Result<(String, Pane), String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(|t| (t.replace("\r\n", "\n"), Pane::View))
            .map_err(|_| format!("{} is not UTF-8 text", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), Pane::Edit)),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// The path of a `file://` URL.
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("file://"))
        .map(|_| &url[7..])?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let rest = rest.split(['#', '?']).next().unwrap_or(rest);
    Some(PathBuf::from(percent_decode(rest)))
}

/// The Markdown file a link in a session's pane points at, if it is one
/// that exists: a `file://` URL or, for plain text, a path relative to
/// the session's directory.
pub fn markdown_target(url: &str, dir: Option<&Path>) -> Option<PathBuf> {
    let path = match file_url_path(url) {
        Some(p) => p,
        None if url.contains("://") => return None,
        None => {
            let p = PathBuf::from(url);
            match dir {
                Some(d) if p.is_relative() => d.join(p),
                _ => p,
            }
        }
    };
    (markdown::is_markdown_path(&path) && path.is_file()).then_some(path)
}

/// Decodes `%XX` escapes; anything malformed stays as written.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// The 0-based line the byte `at` is on.
fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count()
}

/// The byte where 0-based line `line` starts (the end for a line past it).
fn line_start_byte(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 1)
        .map(|(i, _)| i + 1)
        .unwrap_or(text.len())
}

/// The start of the line holding byte `at`.
fn line_start_byte_at(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// The list marker a line starts with, after its indentation: `- `,
/// `* `, `+ `, `1. `, `1) `, `> `, with a task box (`[ ] `) if any.
fn list_marker(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let mut marker = if let Some(m) = ["- ", "* ", "+ ", "> "]
        .iter()
        .find(|m| rest.starts_with(**m))
    {
        m.to_string()
    } else {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let after = &rest[digits..];
        if digits == 0 || digits > 9 || !(after.starts_with(". ") || after.starts_with(") ")) {
            return None;
        }
        rest[..digits + 2].to_string()
    };
    let after = &rest[marker.len()..];
    for task in ["[ ] ", "[x] ", "[X] "] {
        if after.starts_with(task) {
            marker.push_str(task);
            break;
        }
    }
    Some((indent, marker))
}

/// `Enter` in the editor: a new line that keeps the indentation and
/// continues a list (the next number, an empty task box); on an item with
/// nothing after its marker, ends the list instead.
fn continue_list(t: &mut TextArea) {
    let at = t.cursor();
    let start = line_start_byte_at(&t.text, at);
    let line = t.text[start..at].to_string();
    let Some((indent, marker)) = list_marker(&line) else {
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        t.insert_str(&format!("\n{ws}"));
        return;
    };
    let line_end = t.text[at..]
        .find('\n')
        .map(|i| at + i)
        .unwrap_or(t.text.len());
    if line.len() == indent + marker.len() && at == line_end {
        // an empty item: the list ends here
        t.text.replace_range(start..at, "");
        t.set_cursor(start);
        return;
    }
    let next = if let Some(n) = marker
        .split(['.', ')'])
        .next()
        .and_then(|d| d.parse::<u64>().ok())
    {
        let sep = &marker[n.to_string().len()..n.to_string().len() + 2];
        let task = if marker.contains('[') { "[ ] " } else { "" };
        format!("{}{sep}{task}", n + 1)
    } else if marker.contains('[') {
        format!("{}[ ] ", &marker[..2])
    } else {
        marker
    };
    t.insert_str(&format!("\n{}{next}", &line[..indent]));
}

/// `Shift+Tab`: the line loses up to four spaces of indentation.
fn dedent(t: &mut TextArea) {
    let at = t.cursor();
    let start = line_start_byte_at(&t.text, at);
    let n = t.text[start..]
        .chars()
        .take(4)
        .take_while(|c| *c == ' ')
        .count();
    if n > 0 {
        t.text.replace_range(start..start + n, "");
        t.set_cursor(at.saturating_sub(n).max(start));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(v: &mut MarkdownView, s: &str) {
        for c in s.chars() {
            v.handle_key(&key(KeyCode::Char(c)));
        }
    }

    fn temp(name: &str, text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_sideways_swipe_or_shift_wheel_slides_wide_blocks() {
        let (_d, path) = temp("a.md", &format!("```\n{}\n```\n", "x".repeat(60)));
        let mut v = MarkdownView::open(&path).unwrap();
        v.view_area.set(Rect::new(1, 1, 30, 10));
        let mouse = |kind, modifiers| MouseEvent {
            kind,
            column: 5,
            row: 3,
            modifiers,
        };
        v.handle_mouse(&mouse(MouseEventKind::ScrollRight, KeyModifiers::NONE));
        assert_eq!(v.hscroll, 4);
        v.handle_mouse(&mouse(MouseEventKind::ScrollDown, KeyModifiers::SHIFT));
        assert_eq!(v.hscroll, 8);
        v.handle_mouse(&mouse(MouseEventKind::ScrollDown, KeyModifiers::NONE));
        assert_eq!(v.hscroll, 8, "the plain wheel scrolls down, not across");
        for _ in 0..50 {
            v.handle_key(&key(KeyCode::Right));
        }
        let max = v.view_doc().max_hscroll(30);
        assert!(max > 0);
        assert_eq!(v.hscroll, max, "stops at the block's right end");
        v.handle_mouse(&mouse(MouseEventKind::ScrollLeft, KeyModifiers::NONE));
        assert_eq!(v.hscroll, max - 4);
    }

    #[test]
    fn a_new_file_opens_in_the_editor_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        let mut v = MarkdownView::open(&path).unwrap();
        assert_eq!(v.pane, Pane::Edit);
        assert!(!v.dirty());
        typed(&mut v, "# Notes");
        assert!(v.dirty());
        v.handle_key(&ctrl('s'));
        assert!(!v.dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Notes");
    }

    #[test]
    fn closing_with_unsaved_changes_asks() {
        let (_d, path) = temp("a.md", "text");
        let mut v = MarkdownView::open(&path).unwrap();
        assert_eq!(v.pane, Pane::View);
        v.handle_key(&key(KeyCode::Char('e')));
        typed(&mut v, "more ");
        v.handle_key(&key(KeyCode::Esc));
        assert_eq!(v.pane, Pane::View);
        assert_eq!(v.handle_key(&key(KeyCode::Char('q'))), Outcome::None);
        assert!(v.confirm_close);
        // keep editing
        v.handle_key(&key(KeyCode::Esc));
        assert!(!v.confirm_close && v.dirty());
        v.handle_key(&key(KeyCode::Char('q')));
        assert_eq!(v.handle_key(&key(KeyCode::Char('s'))), Outcome::Close);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "more text");
        // discard
        let mut v = MarkdownView::open(&path).unwrap();
        v.handle_key(&key(KeyCode::Char('e')));
        typed(&mut v, "x");
        v.handle_key(&key(KeyCode::Esc));
        v.handle_key(&key(KeyCode::Esc));
        assert_eq!(v.handle_key(&key(KeyCode::Char('d'))), Outcome::Close);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "more text");
    }

    #[test]
    fn typing_undoes_a_word_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = MarkdownView::open(&dir.path().join("u.md")).unwrap();
        typed(&mut v, "hello world");
        assert_eq!(v.text.text, "hello world");
        v.handle_key(&ctrl('z'));
        assert_eq!(v.text.text, "hello ");
        v.handle_key(&ctrl('z'));
        assert_eq!(v.text.text, "hello");
        v.handle_key(&ctrl('z'));
        assert_eq!(v.text.text, "");
        v.handle_key(&ctrl('y'));
        v.handle_key(&ctrl('y'));
        assert_eq!(v.text.text, "hello ");
    }

    #[test]
    fn enter_continues_and_ends_lists() {
        let mut t = TextArea::new("  - one");
        continue_list(&mut t);
        assert_eq!(t.text, "  - one\n  - ");
        continue_list(&mut t);
        assert_eq!(t.text, "  - one\n", "an empty item ends the list");
        let mut t = TextArea::new("9. nine");
        continue_list(&mut t);
        assert_eq!(t.text, "9. nine\n10. ");
        let mut t = TextArea::new("- [x] done");
        continue_list(&mut t);
        assert_eq!(t.text, "- [x] done\n- [ ] ");
        let mut t = TextArea::new("1) a");
        continue_list(&mut t);
        assert_eq!(t.text, "1) a\n2) ");
        let mut t = TextArea::new("    code");
        continue_list(&mut t);
        assert_eq!(t.text, "    code\n    ");
        let mut t = TextArea::new("plain");
        continue_list(&mut t);
        assert_eq!(t.text, "plain\n");
    }

    #[test]
    fn tab_indents_and_shift_tab_dedents() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = MarkdownView::open(&dir.path().join("t.md")).unwrap();
        typed(&mut v, "- a");
        v.handle_key(&key(KeyCode::Tab));
        assert_eq!(v.text.text, "- a    ");
        let mut t = TextArea::new("      x");
        dedent(&mut t);
        assert_eq!(t.text, "  x");
    }

    #[test]
    fn relative_markdown_links_open_here_and_esc_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/b file.md"), "# B\n\n## Part two\n").unwrap();
        std::fs::write(dir.path().join("a.md"), "[b](docs/b%20file.md)").unwrap();
        let mut v = MarkdownView::open(&dir.path().join("a.md")).unwrap();
        assert_eq!(v.follow("docs/b%20file.md#part-two"), Outcome::None);
        assert!(v.path.ends_with("docs/b file.md"));
        assert!(v.text.text.starts_with("# B"));
        assert_eq!(v.handle_key(&key(KeyCode::Esc)), Outcome::None);
        assert!(v.path.ends_with("a.md"));
        assert_eq!(v.handle_key(&key(KeyCode::Esc)), Outcome::Close);
        // the web and missing files
        assert_eq!(
            v.follow("https://example.com"),
            Outcome::OpenUrl("https://example.com".into())
        );
        assert_eq!(v.follow("nope.md"), Outcome::None);
        assert!(v.notice.as_deref().unwrap().starts_with("no such file"));
    }

    #[test]
    fn a_click_on_a_rendered_link_follows_it() {
        let (_d, path) = temp("c.md", "see [site](https://example.com) now");
        let mut v = MarkdownView::open(&path).unwrap();
        v.view_area.set(Rect::new(2, 1, 60, 10));
        let click = |col, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(v.handle_mouse(&click(2, 1)), Outcome::None);
        assert_eq!(
            v.handle_mouse(&click(2 + 5, 1)),
            Outcome::OpenUrl("https://example.com".into())
        );
    }

    #[test]
    fn headings_step_and_anchors_jump() {
        let mut md = String::from("# Top\n\n");
        for i in 0..40 {
            md.push_str(&format!("line {i}\n\n"));
        }
        md.push_str("## Bottom part\n\nend\n");
        let (_d, path) = temp("h.md", &md);
        let mut v = MarkdownView::open(&path).unwrap();
        v.view_area.set(Rect::new(0, 0, 60, 10));
        v.handle_key(&key(KeyCode::Char(']')));
        let bottom = v.view_doc().headings[1].line;
        assert!(v.scroll > 0 && (v.scroll..v.scroll + 10).contains(&bottom));
        assert_eq!(v.current_heading().as_deref(), Some("Top"));
        v.handle_key(&key(KeyCode::Char('[')));
        assert_eq!(v.scroll, 0);
        v.follow("#bottom-part");
        assert!(v.scroll > 0);
    }

    #[test]
    fn pane_links_to_markdown_files_are_recognized() {
        let (d, path) = temp("README.md", "x");
        let url = format!("file://{}", path.display()).replace(' ', "%20");
        assert_eq!(markdown_target(&url, None), Some(path.clone()));
        assert_eq!(
            markdown_target("README.md", Some(d.path())),
            Some(d.path().join("README.md"))
        );
        assert_eq!(markdown_target("https://x.org/a.md", None), None);
        assert_eq!(markdown_target("missing.md", Some(d.path())), None);
        assert_eq!(
            file_url_path("file://localhost/tmp/a%20b.md#x"),
            Some(PathBuf::from("/tmp/a b.md"))
        );
        assert_eq!(percent_decode("100%"), "100%");
    }
}
