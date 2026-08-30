//! Incremental, case-insensitive substring search over screen + scrollback.

use crate::selection::row_cells;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Grid-absolute row (same coordinate system as selection.rs).
    pub row: usize,
    /// Inclusive cell-column range of the match on that row.
    pub col_start: u16,
    pub col_end: u16,
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String,
    /// Ascending by (row, col_start).
    pub matches: Vec<Match>,
    /// Index into `matches`; starts at the last (nearest live) match.
    pub current: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute matches for the current query. Restores the parser's
    /// scroll offset (row_cells does per row). Resets `current` to the
    /// match nearest the live bottom.
    pub fn run<CB: vt100::Callbacks>(
        &mut self,
        parser: &mut vt100::Parser<CB>,
        scrollback_len: usize,
    ) {
        self.matches.clear();
        let needle = self.query.to_lowercase();
        if needle.is_empty() {
            self.current = 0;
            return;
        }
        let screen_rows = usize::from(parser.screen().size().0);
        for row in 0..scrollback_len + screen_rows {
            let cells = row_cells(parser, scrollback_len, row);
            // build the row text alongside a char-index -> cell-col map
            let mut text = String::new();
            let mut col_of_char: Vec<u16> = Vec::new();
            for (col, contents) in &cells {
                let piece = if contents.is_empty() { " " } else { contents };
                for _ in piece.chars() {
                    col_of_char.push(*col);
                }
                text.push_str(piece);
            }
            let hay = text.to_lowercase();
            let mut from = 0;
            while let Some(found) = hay[from..].find(&needle) {
                let start = from + found;
                let end = start + needle.len(); // byte len of lowercase needle
                // map byte offsets to char indices for the col map
                let start_char = hay[..start].chars().count();
                let end_char = start_char + needle.chars().count();
                if let (Some(&cs), Some(&ce)) = (
                    col_of_char.get(start_char),
                    col_of_char.get(end_char.saturating_sub(1)),
                ) {
                    self.matches.push(Match {
                        row,
                        col_start: cs,
                        col_end: ce,
                    });
                }
                from = end.max(start + 1);
            }
        }
        self.current = self.matches.len().saturating_sub(1);
    }

    /// Step to the next OLDER match (upward through history), wrapping.
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
    }

    /// Step back toward newer matches, wrapping.
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.matches.len();
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.matches.get(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5x20 parser fed rows row-00..row-11 (scrollback_len = 7).
    fn parser_with_lines() -> (vt100::Parser, usize) {
        let mut parser = vt100::Parser::new(5, 20, 100);
        for i in 0..12 {
            parser.process(format!("row-{i:02}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(0);
        (parser, len)
    }

    #[test]
    fn finds_matches_across_scrollback_and_screen() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "row-0".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 10); // row-00 .. row-09
        assert_eq!(st.matches[0].row, 0);
        assert_eq!(st.matches[9].row, 9);
        // current starts nearest the bottom
        assert_eq!(st.current, 9);
    }

    #[test]
    fn match_columns_are_cell_positions() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "w-03".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 1);
        let m = &st.matches[0];
        assert_eq!((m.row, m.col_start, m.col_end), (3, 2, 5));
    }

    #[test]
    fn case_insensitive() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "ROW-05".into();
        st.run(&mut parser, len);
        assert_eq!(st.matches.len(), 1);
        assert_eq!(st.matches[0].row, 5);
    }

    #[test]
    fn navigation_walks_history_and_wraps() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.query = "row-0".into();
        st.run(&mut parser, len);
        assert_eq!(st.current_match().unwrap().row, 9);
        st.next(); // older
        assert_eq!(st.current_match().unwrap().row, 8);
        st.prev(); // newer
        assert_eq!(st.current_match().unwrap().row, 9);
        st.prev(); // newest wraps to oldest? no: prev from last wraps to first
        assert_eq!(st.current_match().unwrap().row, 0);
        st.next(); // older than oldest wraps to newest
        assert_eq!(st.current_match().unwrap().row, 9);
    }

    #[test]
    fn empty_query_and_no_match_are_harmless() {
        let (mut parser, len) = parser_with_lines();
        let mut st = SearchState::new();
        st.run(&mut parser, len);
        assert!(st.matches.is_empty());
        assert!(st.current_match().is_none());
        st.next();
        st.prev(); // no panic on empty
        st.query = "zebra".into();
        st.run(&mut parser, len);
        assert!(st.matches.is_empty());
    }

    #[test]
    fn run_restores_scroll_offset() {
        let (mut parser, len) = parser_with_lines();
        parser.screen_mut().set_scrollback(4);
        let mut st = SearchState::new();
        st.query = "row".into();
        st.run(&mut parser, len);
        assert_eq!(parser.screen().scrollback(), 4);
    }
}
