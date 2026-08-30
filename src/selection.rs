//! Pure selection model over the vt100 grid.
//!
//! Coordinates are grid-absolute so a selection survives scrolling: row 0
//! is the oldest scrollback row, rows [scrollback_len, scrollback_len +
//! screen_rows) are the live screen. With scroll offset `o`, visible
//! visual row `v` shows abs row `scrollback_len - o + v`.
//!
//! Known limit (same trade-off as any emulator at its buffer cap): once
//! vt100 starts dropping rows at SCROLLBACK_LINES, absolute rows shift by
//! the number of dropped rows and a held selection drifts. Selections are
//! short-lived (a drag), so this is acceptable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub row: usize,
    pub col: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

/// Grid-absolute row shown at visible row `visual_row` of the current view.
pub fn abs_row(scrollback_len: usize, offset: usize, visual_row: u16) -> usize {
    scrollback_len - offset + usize::from(visual_row)
}

impl Selection {
    pub fn new(p: Pos) -> Self {
        Selection { anchor: p, head: p }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// (start, end) in reading order, both inclusive.
    pub fn normalized(&self) -> (Pos, Pos) {
        let a = self.anchor;
        let h = self.head;
        if (h.row, h.col) < (a.row, a.col) {
            (h, a)
        } else {
            (a, h)
        }
    }

    /// Linear (stream) selection semantics, like every terminal: first row
    /// from start col to end-of-row, middle rows fully, last row up to end
    /// col, all bounds inclusive.
    pub fn contains(&self, row: usize, col: u16) -> bool {
        let (s, e) = self.normalized();
        if row < s.row || row > e.row {
            return false;
        }
        if s.row == e.row {
            return col >= s.col && col <= e.col;
        }
        if row == s.row {
            return col >= s.col;
        }
        if row == e.row {
            return col <= e.col;
        }
        true
    }
}

/// (col, contents) for every non-continuation cell of a grid-absolute row,
/// including cells whose contents are empty (e.g. blank/unwritten
/// columns) -- callers distinguish "empty" from "absent" themselves.
/// Temporarily moves the scrollback offset to bring the row into view and
/// restores it before returning.
pub fn row_cells<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    row: usize,
) -> Vec<(u16, String)> {
    let (screen_rows, cols) = parser.screen().size();
    let saved = parser.screen().scrollback();
    let (offset, visual) = if row >= scrollback_len {
        let v = row - scrollback_len;
        if v >= usize::from(screen_rows) {
            return Vec::new(); // below the live screen: nothing there
        }
        (0, v as u16)
    } else {
        (scrollback_len - row, 0u16)
    };
    parser.screen_mut().set_scrollback(offset);
    let mut out = Vec::new();
    {
        let screen = parser.screen();
        for c in 0..cols {
            if let Some(cell) = screen.cell(visual, c) {
                if cell.is_wide_continuation() {
                    continue;
                }
                out.push((c, cell.contents().to_string()));
            }
        }
    }
    parser.screen_mut().set_scrollback(saved);
    out
}

/// Text of the selection: rows joined with `\n`, trailing whitespace
/// trimmed per row, empty cells inside the range read as spaces.
pub fn extract_text<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    scrollback_len: usize,
    sel: &Selection,
) -> String {
    let (start, end) = sel.normalized();
    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let cells = row_cells(parser, scrollback_len, row);
        let (from, to) = (
            if row == start.row { start.col } else { 0 },
            if row == end.row { end.col } else { u16::MAX },
        );
        let mut line = String::new();
        for (col, contents) in &cells {
            if *col < from || *col > to {
                continue;
            }
            if contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(contents);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5-row x 20-col parser fed 12 numbered lines: rows row-00..row-11,
    /// scrollback holds the first 7, screen shows row-07..row-11.
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
    fn abs_row_formula() {
        // live view (offset 0): visual 0 is the first on-screen row
        assert_eq!(abs_row(7, 0, 0), 7);
        // scrolled all the way back: visual 0 is the oldest row
        assert_eq!(abs_row(7, 7, 0), 0);
        assert_eq!(abs_row(7, 3, 2), 6);
    }

    #[test]
    fn normalization_swaps_reverse_drags() {
        let sel = Selection {
            anchor: Pos { row: 5, col: 10 },
            head: Pos { row: 3, col: 2 },
        };
        let (start, end) = sel.normalized();
        assert_eq!(start, Pos { row: 3, col: 2 });
        assert_eq!(end, Pos { row: 5, col: 10 });
        // same row, reversed cols
        let sel = Selection {
            anchor: Pos { row: 4, col: 9 },
            head: Pos { row: 4, col: 1 },
        };
        let (start, end) = sel.normalized();
        assert_eq!((start.col, end.col), (1, 9));
    }

    #[test]
    fn contains_linear_selection_semantics() {
        let sel = Selection {
            anchor: Pos { row: 2, col: 5 },
            head: Pos { row: 4, col: 3 },
        };
        assert!(!sel.contains(2, 4)); // before start col on first row
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 19)); // rest of first row
        assert!(sel.contains(3, 0)); // full middle row
        assert!(sel.contains(3, 19));
        assert!(sel.contains(4, 0));
        assert!(sel.contains(4, 3)); // inclusive end
        assert!(!sel.contains(4, 4));
        assert!(!sel.contains(1, 10));
        assert!(!sel.contains(5, 0));
    }

    #[test]
    fn extract_within_live_screen() {
        let (mut parser, len) = parser_with_lines();
        // rows 7 and 8 are "row-07" and "row-08" on the live screen
        let sel = Selection {
            anchor: Pos { row: 7, col: 0 },
            head: Pos { row: 8, col: 5 },
        };
        assert_eq!(extract_text(&mut parser, len, &sel), "row-07\nrow-08");
    }

    #[test]
    fn extract_spans_scrollback_and_restores_offset() {
        let (mut parser, len) = parser_with_lines();
        parser.screen_mut().set_scrollback(2); // some arbitrary view
        let sel = Selection {
            anchor: Pos { row: 5, col: 0 }, // in scrollback
            head: Pos { row: 7, col: 5 },   // first live row
        };
        assert_eq!(
            extract_text(&mut parser, len, &sel),
            "row-05\nrow-06\nrow-07"
        );
        assert_eq!(parser.screen().scrollback(), 2, "offset must be restored");
    }

    #[test]
    fn extract_reverse_drag_same_row_column_range() {
        let (mut parser, len) = parser_with_lines();
        let sel = Selection {
            anchor: Pos { row: 7, col: 4 },
            head: Pos { row: 7, col: 0 },
        };
        assert_eq!(extract_text(&mut parser, len, &sel), "row-0");
    }

    #[test]
    fn extract_handles_wide_chars() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process("你好ab".as_bytes());
        let sel = Selection {
            anchor: Pos { row: 0, col: 0 },
            head: Pos { row: 0, col: 5 },
        };
        assert_eq!(extract_text(&mut parser, 0, &sel), "你好ab");
    }

    #[test]
    fn row_cells_skips_wide_continuations() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process("你a".as_bytes());
        let cells = row_cells(&mut parser, 0, 0);
        // col 0 = 你 (wide), col 1 is its continuation (skipped), col 2 = a
        assert_eq!(cells[0], (0, "你".to_string()));
        assert!(!cells.iter().any(|(c, _)| *c == 1));
        assert!(cells.contains(&(2, "a".to_string())));
    }
}
