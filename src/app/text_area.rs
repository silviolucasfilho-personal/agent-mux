//! A small multi-line text field for dialogs: the workflow task and the
//! prose arguments, where a user writes a whole prompt rather than a word.
//! Holds the text and a byte cursor, wraps for display, and moves by
//! visual row so `↑`/`↓` behave the way the text looks on screen. The
//! renderer writes the width it drew with back into `width`, the same way
//! the views write back `viewport_rows`.
//!
//! Enter stays "submit" in every agent-mux dialog, so a newline is
//! `Alt+Enter` (also `Shift+Enter` and `Ctrl+J` where the terminal
//! reports them). Anything longer than a screenful belongs in `$EDITOR`,
//! which `Ctrl+E` opens through the same round-trip the Configuration
//! view uses.

use std::cell::Cell;

/// One visual row: a contiguous byte range of the text, without the
/// newline that ended it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TextArea {
    pub text: String,
    /// Byte index of the cursor; always on a character boundary.
    cursor: usize,
    /// The width the renderer last drew with, for visual row movement.
    pub width: Cell<usize>,
}

impl PartialEq for TextArea {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.cursor == other.cursor
    }
}

impl TextArea {
    pub fn new(text: impl Into<String>) -> TextArea {
        let text = text.into().replace("\r\n", "\n").replace('\r', "\n");
        TextArea {
            cursor: text.len(),
            text,
            width: Cell::new(40),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the whole text and puts the cursor at the end (the editor
    /// round-trip and a reset).
    pub fn set(&mut self, text: impl Into<String>) {
        let text = text.into().replace("\r\n", "\n").replace('\r', "\n");
        self.cursor = text.len();
        self.text = text;
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Inserts pasted text; carriage returns are normalized to newlines.
    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.text.len());
        self.text.replace_range(self.cursor..next, "");
    }

    /// Deletes the word before the cursor (`Ctrl+W`).
    pub fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches([' ', '\t']);
        let start = trimmed.rfind([' ', '\t', '\n']).map(|i| i + 1).unwrap_or(0);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        true
    }

    pub fn right(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        self.cursor += self.text[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        true
    }

    /// The visual rows at the last rendered width.
    pub fn rows(&self) -> Vec<Row> {
        wrap(&self.text, self.width.get().max(1))
    }

    /// `(row index, column in characters)` of the cursor.
    pub fn cursor_at(&self, rows: &[Row]) -> (usize, usize) {
        let idx = rows
            .iter()
            .rposition(|r| r.start <= self.cursor && self.cursor <= r.end)
            .unwrap_or(0);
        let row = rows.get(idx).copied().unwrap_or(Row { start: 0, end: 0 });
        let col = self.text[row.start..self.cursor.min(row.end).max(row.start)]
            .chars()
            .count();
        (idx, col)
    }

    /// Moves to the same column one visual row up; false at the top, so
    /// the caller can move to the previous field instead.
    pub fn up(&mut self) -> bool {
        let rows = self.rows();
        let (r, col) = self.cursor_at(&rows);
        if r == 0 {
            return false;
        }
        self.cursor = byte_at_col(&self.text, rows[r - 1], col);
        true
    }

    /// Moves one visual row down; false at the bottom.
    pub fn down(&mut self) -> bool {
        let rows = self.rows();
        let (r, col) = self.cursor_at(&rows);
        if r + 1 >= rows.len() {
            return false;
        }
        self.cursor = byte_at_col(&self.text, rows[r + 1], col);
        true
    }

    /// Start of the visual row.
    pub fn home(&mut self) {
        let rows = self.rows();
        let (r, _) = self.cursor_at(&rows);
        self.cursor = rows.get(r).map(|row| row.start).unwrap_or(0);
    }

    /// End of the visual row.
    pub fn end(&mut self) {
        let rows = self.rows();
        let (r, _) = self.cursor_at(&rows);
        self.cursor = rows.get(r).map(|row| row.end).unwrap_or(self.text.len());
    }

    /// One line for an unfocused preview: the first line, with a marker
    /// when there is more.
    pub fn preview(&self, width: usize) -> String {
        let first = self.text.lines().next().unwrap_or("");
        let more = self.text.lines().count() > 1;
        let mut out: String = first.chars().take(width.max(4)).collect();
        if first.chars().count() > width.max(4) || more {
            out.push('…');
        }
        out
    }
}

/// The byte index `col` characters into a row, clamped to its end.
fn byte_at_col(text: &str, row: Row, col: usize) -> usize {
    text[row.start..row.end]
        .char_indices()
        .nth(col)
        .map(|(i, _)| row.start + i)
        .unwrap_or(row.end)
}

/// Wraps `text` into visual rows of at most `width` characters. Rows are
/// contiguous byte ranges, so every position in the text maps to a row;
/// a break prefers the last space that fits and keeps that space on the
/// row it ended.
pub fn wrap(text: &str, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0usize;
    for line in text.split('\n') {
        let line_end = line_start + line.len();
        if line.is_empty() {
            rows.push(Row {
                start: line_start,
                end: line_start,
            });
        } else {
            let mut pos = line_start;
            while pos < line_end {
                let rest = &text[pos..line_end];
                let mut take = rest.len();
                let mut last_space = None;
                for (count, (i, ch)) in rest.char_indices().enumerate() {
                    if count == width {
                        take = i;
                        break;
                    }
                    if ch == ' ' {
                        last_space = Some(i + ch.len_utf8());
                    }
                }
                if take < rest.len()
                    && let Some(s) = last_space
                    && s > 0
                {
                    take = s;
                }
                rows.push(Row {
                    start: pos,
                    end: pos + take,
                });
                pos += take;
            }
        }
        line_start = line_end + 1; // past the '\n'
    }
    if rows.is_empty() {
        rows.push(Row { start: 0, end: 0 });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ta(text: &str, width: usize) -> TextArea {
        let t = TextArea::new(text);
        t.width.set(width);
        t
    }

    #[test]
    fn typing_editing_and_pasting() {
        let mut t = TextArea::new("");
        assert!(t.is_empty());
        for c in "audit".chars() {
            t.insert(c);
        }
        t.newline();
        t.insert_str("the parser\r\nfor injection");
        assert_eq!(t.text, "audit\nthe parser\nfor injection");
        assert_eq!(t.cursor(), t.text.len());
        assert!(!t.is_empty());
        t.backspace();
        assert!(t.text.ends_with("for injectio"));
        t.delete_word();
        assert!(t.text.ends_with("for "), "{:?}", t.text);
        assert!(t.left() && t.left());
        t.delete();
        assert_eq!(
            t.text, "audit\nthe parser\nfo ",
            "the word's trailing space stays"
        );
        t.set("fresh");
        assert_eq!((t.text.as_str(), t.cursor()), ("fresh", 5));
        // a cursor at the start stops moving left
        while t.left() {}
        assert_eq!(t.cursor(), 0);
        t.backspace();
        assert_eq!(t.text, "fresh");
        while t.right() {}
        t.delete();
        assert_eq!(t.text, "fresh");
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_covers_every_byte() {
        let rows = wrap("hello world again", 8);
        assert_eq!(
            rows.iter()
                .map(|r| &"hello world again"[r.start..r.end])
                .collect::<Vec<_>>(),
            vec!["hello ", "world ", "again"]
        );
        // a row that fits exactly is not broken early
        assert_eq!(wrap("hello world again", 11).len(), 2);
        // contiguous: each row starts where the previous ended
        for pair in rows.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        // a word longer than the width breaks hard
        let long = "supercalifragilistic";
        assert_eq!(wrap(long, 8).len(), 3);
        // explicit newlines make rows, including empty ones
        let rows = wrap("a\n\nb", 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(&"a\n\nb"[rows[1].start..rows[1].end], "");
        assert_eq!(wrap("", 10), vec![Row { start: 0, end: 0 }]);
        // multi-byte characters never split
        let emoji = "⚡⚡⚡⚡";
        let rows = wrap(emoji, 2);
        assert_eq!(rows.len(), 2);
        for r in rows {
            assert!(emoji.is_char_boundary(r.start) && emoji.is_char_boundary(r.end));
        }
    }

    #[test]
    fn cursor_moves_by_visual_row() {
        let mut t = ta("hello world again", 8);
        // rows: "hello " | "world " | "again", cursor at the end
        let rows = t.rows();
        assert_eq!(t.cursor_at(&rows), (2, 5));
        assert!(t.up());
        assert_eq!(t.cursor_at(&t.rows()), (1, 5));
        assert!(t.up());
        assert_eq!(t.cursor_at(&t.rows()), (0, 5));
        assert!(!t.up(), "the top hands navigation back to the dialog");
        assert!(t.down() && t.down());
        assert!(!t.down(), "and so does the bottom");
        t.home();
        assert_eq!(t.cursor_at(&t.rows()), (2, 0));
        t.end();
        assert_eq!(t.cursor_at(&t.rows()), (2, 5));
        // a short row clamps the column
        let mut t = ta("longer line\nab", 20);
        t.home();
        assert_eq!(t.cursor_at(&t.rows()), (1, 0));
        assert!(t.up());
        t.end();
        assert!(t.down());
        assert_eq!(t.cursor_at(&t.rows()), (1, 2), "clamped to the shorter row");
    }

    #[test]
    fn preview_is_one_line_with_a_marker() {
        assert_eq!(TextArea::new("one line").preview(40), "one line");
        assert_eq!(TextArea::new("first\nsecond").preview(40), "first…");
        assert_eq!(TextArea::new("abcdefgh").preview(4), "abcd…");
    }
}
