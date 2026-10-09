//! Mermaid diagrams drawn as text, for ```` ```mermaid ```` blocks in the
//! Markdown viewer. `render` turns a block's body into rows of
//! `(text, kind)` runs; the viewer colours each run by its kind.
//!
//! Drawn:
//! - `graph` / `flowchart` in every direction (`TD`, `TB`, `LR`, `RL`,
//!   `BT`): the node shapes (rectangle, round, stadium, circle, database,
//!   subroutine, diamond, hexagon, the slanted ones as rectangles), every
//!   link style (`-->`, `---`, `-.->`, `==>`, `--o`, `--x`, `<-->` and the
//!   longer forms), labels (`-->|yes|`, `-- yes -->`), chains and `&`
//!   groups. `~~~` shapes the layout and is not drawn. `subgraph` grouping,
//!   `classDef`, `style`, `click` and the like are read and ignored.
//! - `stateDiagram` / `stateDiagram-v2`, on the same engine: `[*]` is a
//!   `●` where a transition leaves it and a `◉` where one ends in it,
//!   composite states are flattened, notes are skipped.
//! - `sequenceDiagram`: participants and actors, the eight message arrows,
//!   self-messages, notes, `autonumber`, and the `loop` / `alt` / `opt` /
//!   `par` / `critical` / `break` / `rect` blocks as dashed frame lines.
//! - `pie`, as a horizontal bar chart.
//!
//! Anything else is an `Err` naming the type, and the viewer shows the
//! source instead.
//!
//! Everything is drawn on a `Canvas` of cells. A cell holds a character
//! (a wide one takes two cells) or a line: a mask of the directions it
//! connects to plus a style, turned into a box-drawing character when the
//! rows are emitted, so lines that meet become `┬`, `┼` and the rest on
//! their own. Text always wins over a line.
//!
//! Flowcharts are laid out in layers, written once over a main axis (the
//! rank direction) and a cross axis. A depth-first search reverses the
//! edges that close a cycle (their arrowhead stays where it belongs, and
//! they leave and enter beside the box centre, on a box widened for it,
//! so they never share the forward edges' line);
//! ranks are the longest path from a source; an edge longer than one rank
//! passes through a one-cell dummy in every rank between. Four barycenter
//! sweeps order each rank; cross positions come from the mean port of each
//! node's neighbours, fitted left to right without overlap (an isotonic
//! fit, so a block of nodes that wants the same place shares it evenly).
//! Between two ranks: an exit cell, one track per group of edges that has
//! to jog sideways (a fan-out shares one, a fan-in shares one, tracks are
//! ordered to avoid crossings), the label rows, the arrowhead cell. `BT`
//! and `RL` are `TD` and `LR` with every edge reversed.

use std::collections::{HashMap, VecDeque};

use unicode_width::UnicodeWidthChar;

/// What a cell of a drawn diagram is, so the viewer can colour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    /// Nothing drawn.
    Blank,
    /// A node's or a participant's border.
    Border,
    /// The text inside a node, a participant or a note.
    Text,
    /// An edge or a lifeline.
    Line,
    /// An arrowhead or an end marker.
    Arrow,
    /// The label on an edge or a message.
    Label,
    /// A note's border, a block frame (`loop`, `alt` …) or a title.
    Frame,
}

/// One row of a drawn diagram: `(text, kind)` runs, left to right, whose
/// display widths add up to the row's width.
pub type Row = Vec<(String, CellKind)>;

/// A drawn diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    /// The diagram type as written (`flowchart`, `sequenceDiagram` …).
    pub kind: String,
    pub rows: Vec<Row>,
    /// The widest row, in terminal columns.
    pub width: usize,
}

/// Draws `src`, the body of a ```` ```mermaid ```` block. `Err` says why it
/// could not (an unsupported diagram type, a line it does not understand);
/// the viewer then shows the source with that note.
pub fn render(src: &str) -> Result<Diagram, String> {
    let lines = body_lines(src);
    let Some(first) = lines.first() else {
        return Err("empty diagram".into());
    };
    let kind = first
        .split(|c: char| c.is_whitespace() || c == ';')
        .next()
        .unwrap_or("")
        .to_string();
    let canvas = match kind.as_str() {
        "graph" | "flowchart" | "flowchart-elk" => draw_flow(&parse_flowchart(&lines)?)?,
        "stateDiagram" | "stateDiagram-v2" => draw_flow(&parse_state(&lines)?)?,
        "sequenceDiagram" => draw_sequence(&parse_sequence(&lines)?)?,
        "pie" => draw_pie(&lines)?,
        _ => return Err(format!("{kind}: not drawn in the terminal")),
    };
    let rows = canvas.into_rows()?;
    if rows.is_empty() {
        return Err(format!("{kind}: nothing to draw"));
    }
    let width = rows
        .iter()
        .map(|r| r.iter().map(|(s, _)| str_width(s)).sum::<usize>())
        .max()
        .unwrap_or(0);
    Ok(Diagram { kind, rows, width })
}

/// The largest canvas drawn; anything bigger is refused rather than
/// allocated.
const MAX_COLS: i64 = 1000;
const MAX_ROWS: i64 = 5000;
const MAX_NODES: usize = 2000;
const MAX_EDGES: usize = 3000;
const TOO_LARGE: &str = "too large to draw in the terminal";
/// Node labels wrap past this many columns.
const WRAP: usize = 28;
/// Message and note text wraps past this many columns.
const SEQ_WRAP: usize = 40;

/// The block's lines, trimmed, without blank lines, `%%` comments and a
/// leading `---` front-matter block.
fn body_lines(src: &str) -> Vec<String> {
    let all: Vec<&str> = src.lines().collect();
    let mut i = all.iter().take_while(|l| l.trim().is_empty()).count();
    if all.get(i).is_some_and(|l| l.trim() == "---")
        && let Some(end) = all[i + 1..].iter().position(|l| l.trim() == "---")
    {
        i += end + 2;
    }
    all[i.min(all.len())..]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("%%"))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Text

fn char_width(c: char) -> usize {
    if c.is_control() {
        0
    } else {
        c.width().unwrap_or(0)
    }
}

fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// `s` with tabs as spaces and every zero-width character dropped, so its
/// width is the sum of its characters' widths.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|&c| char_width(c) > 0)
        .collect()
}

/// A label's display lines: unquoted, `<br>` and `\n` as breaks, Markdown
/// emphasis and backticks dropped, entities decoded, wrapped at `wrap`.
fn label_lines(raw: &str, wrap: usize) -> Vec<String> {
    let mut s = raw.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = &s[1..s.len() - 1];
    }
    let s = replace_br(s)
        .replace("\\n", "\n")
        .replace("**", "")
        .replace('`', "")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&#35;", "#")
        .replace("&amp;", "&");
    let mut out = Vec::new();
    for line in s.split('\n') {
        let line = clean(line);
        out.extend(wrap_line(line.trim(), wrap));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// `s` with every `<br>`, `<br/>` and `<br />` (any case) as a newline.
fn replace_br(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with("<br")
            && let Some(end) = lower[i..].find('>')
        {
            let inner = lower[i + 3..i + end].trim().trim_end_matches('/').trim();
            if inner.is_empty() {
                out.push('\n');
                i += end + 1;
                continue;
            }
        }
        let Some(c) = s[i..].chars().next() else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Greedy word wrap by display width; a word wider than `max` is cut.
fn wrap_line(line: &str, max: usize) -> Vec<String> {
    if str_width(line) <= max {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split(' ').filter(|w| !w.is_empty()) {
        let mut pieces = Vec::new();
        if str_width(word) > max {
            let mut piece = String::new();
            for c in word.chars() {
                if str_width(&piece) + char_width(c) > max && !piece.is_empty() {
                    pieces.push(std::mem::take(&mut piece));
                }
                piece.push(c);
            }
            pieces.push(piece);
        } else {
            pieces.push(word.to_string());
        }
        for p in pieces {
            if cur.is_empty() {
                cur = p;
            } else if str_width(&cur) + 1 + str_width(&p) <= max {
                cur.push(' ');
                cur.push_str(&p);
            } else {
                out.push(std::mem::replace(&mut cur, p));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A statement for an error message: at most 40 characters.
fn short(s: &str) -> String {
    let mut out: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------------------
// Canvas

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Solid,
    Dotted,
    Thick,
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Empty,
    Ch(char, CellKind),
    /// The second column of a wide character.
    Cont,
    /// A line through the cell: the directions it connects to.
    Line(u8, Style),
}

#[derive(Default)]
struct Canvas {
    cells: Vec<Vec<Cell>>,
    /// Something was drawn past `MAX_COLS` / `MAX_ROWS`.
    overflow: bool,
}

impl Canvas {
    fn get(&self, x: i64, y: i64) -> Cell {
        if x < 0 || y < 0 {
            return Cell::Empty;
        }
        self.cells
            .get(y as usize)
            .and_then(|r| r.get(x as usize))
            .copied()
            .unwrap_or(Cell::Empty)
    }

    fn slot(&mut self, x: i64, y: i64) -> Option<&mut Cell> {
        if x < 0 || y < 0 {
            return None;
        }
        if x >= MAX_COLS || y >= MAX_ROWS {
            self.overflow = true;
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if self.cells.len() <= y {
            self.cells.resize(y + 1, Vec::new());
        }
        let row = &mut self.cells[y];
        if row.len() <= x {
            row.resize(x + 1, Cell::Empty);
        }
        Some(&mut row[x])
    }

    fn set(&mut self, x: i64, y: i64, cell: Cell) {
        if let Some(s) = self.slot(x, y) {
            *s = cell;
        }
    }

    fn is_text(&self, x: i64, y: i64) -> bool {
        matches!(self.get(x, y), Cell::Ch(..) | Cell::Cont)
    }

    fn width(&self) -> i64 {
        self.cells.iter().map(Vec::len).max().unwrap_or(0) as i64
    }

    /// Writes one character; returns its width.
    fn put(&mut self, x: i64, y: i64, c: char, kind: CellKind) -> i64 {
        let w = char_width(c) as i64;
        if w == 0 {
            return 0;
        }
        if self.get(x, y) == Cell::Cont {
            self.set(x - 1, y, Cell::Empty);
        }
        let was_wide = matches!(self.get(x, y), Cell::Ch(o, _) if char_width(o) == 2);
        self.set(x, y, Cell::Ch(c, kind));
        if w == 2 {
            if matches!(self.get(x + 1, y), Cell::Ch(o, _) if char_width(o) == 2) {
                self.set(x + 2, y, Cell::Empty);
            }
            self.set(x + 1, y, Cell::Cont);
        } else if was_wide && self.get(x + 1, y) == Cell::Cont {
            self.set(x + 1, y, Cell::Empty);
        }
        w
    }

    /// Writes `s` from `x`; returns its width.
    fn text(&mut self, x: i64, y: i64, s: &str, kind: CellKind) -> i64 {
        let mut cx = x;
        for c in s.chars() {
            cx += self.put(cx, y, c, kind);
        }
        cx - x
    }

    /// Writes `s` from `x` into the empty cells only.
    fn text_into_empty(&mut self, x: i64, y: i64, s: &str, kind: CellKind) {
        let mut cx = x;
        for c in s.chars() {
            let w = char_width(c) as i64;
            if (0..w).all(|k| self.get(cx + k, y) == Cell::Empty) {
                self.put(cx, y, c, kind);
            }
            cx += w;
        }
    }

    fn line(&mut self, x: i64, y: i64, bits: u8, style: Style) {
        let Some(s) = self.slot(x, y) else { return };
        match *s {
            Cell::Empty => *s = Cell::Line(bits, style),
            Cell::Line(b, st) => *s = Cell::Line(b | bits, st),
            _ => {}
        }
    }

    /// A straight line between two cells on one row or one column.
    fn seg(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, style: Style) {
        let limit = MAX_COLS.max(MAX_ROWS);
        if [x0, y0, x1, y1].iter().any(|v| *v > limit || *v < -limit) {
            self.overflow = true;
            return;
        }
        if y0 == y1 {
            let (a, b) = (x0.min(x1), x0.max(x1));
            for x in a..=b {
                let bits = if x > a { LEFT } else { 0 } | if x < b { RIGHT } else { 0 };
                if bits != 0 {
                    self.line(x, y0, bits, style);
                }
            }
        } else if x0 == x1 {
            let (a, b) = (y0.min(y1), y0.max(y1));
            for y in a..=b {
                let bits = if y > a { UP } else { 0 } | if y < b { DOWN } else { 0 };
                self.line(x0, y, bits, style);
            }
        }
    }

    fn path(&mut self, pts: &[(i64, i64)], style: Style) {
        for w in pts.windows(2) {
            self.seg(w[0].0, w[0].1, w[1].0, w[1].1, style);
        }
    }

    /// A box of border characters `[tl, tr, bl, br, horizontal, vertical]`
    /// with `lines` centred inside.
    fn frame_box(
        &mut self,
        (x, y, w, h): (i64, i64, i64, i64),
        set: [char; 6],
        border: CellKind,
        lines: &[String],
    ) {
        let [tl, tr, bl, br, hz, vt] = set;
        for row in 0..h {
            for col in 0..w {
                let (top, bottom) = (row == 0, row == h - 1);
                let (left, right) = (col == 0, col == w - 1);
                let c = match (top, bottom, left, right) {
                    (true, _, true, _) => tl,
                    (true, _, _, true) => tr,
                    (_, true, true, _) => bl,
                    (_, true, _, true) => br,
                    (true, _, _, _) | (_, true, _, _) => hz,
                    (_, _, true, _) | (_, _, _, true) => vt,
                    _ => ' ',
                };
                let kind = if c == ' ' { CellKind::Text } else { border };
                self.put(x + col, y + row, c, kind);
            }
        }
        let top = (h - 2 - lines.len() as i64).max(0) / 2;
        for (k, line) in lines.iter().enumerate() {
            let pad = (w - 2 - str_width(line) as i64).max(0) / 2;
            self.text(x + 1 + pad, y + 1 + top + k as i64, line, CellKind::Text);
        }
    }

    /// Replaces a border character at `(x, y)` with `to` if it is `from`.
    fn swap_border(&mut self, x: i64, y: i64, from: char, to: char) {
        if self.get(x, y) == Cell::Ch(from, CellKind::Border) {
            self.set(x, y, Cell::Ch(to, CellKind::Border));
        }
    }

    fn into_rows(self) -> Result<Vec<Row>, String> {
        if self.overflow {
            return Err(TOO_LARGE.into());
        }
        let mut rows = Vec::with_capacity(self.cells.len());
        for cells in &self.cells {
            let mut row: Row = Vec::new();
            let mut skip = false;
            for cell in cells {
                if std::mem::take(&mut skip) {
                    continue;
                }
                let (c, kind) = match *cell {
                    Cell::Empty | Cell::Cont => (' ', CellKind::Blank),
                    Cell::Ch(c, k) => {
                        skip = char_width(c) == 2;
                        (c, k)
                    }
                    Cell::Line(bits, style) => (glyph(bits, style), CellKind::Line),
                };
                match row.last_mut() {
                    Some((s, k)) if *k == kind => s.push(c),
                    _ => row.push((c.to_string(), kind)),
                }
            }
            while matches!(row.last(), Some((_, CellKind::Blank))) {
                row.pop();
            }
            rows.push(row);
        }
        while rows.last().is_some_and(Vec::is_empty) {
            rows.pop();
        }
        Ok(rows)
    }
}

/// The box-drawing character for a line cell. Straight runs keep their
/// style; corners and junctions are always light and solid.
fn glyph(bits: u8, style: Style) -> char {
    let horizontal = bits & (LEFT | RIGHT) != 0;
    let vertical = bits & (UP | DOWN) != 0;
    match (horizontal, vertical) {
        (false, false) => ' ',
        (true, false) => match style {
            Style::Solid => '─',
            Style::Dotted => '╌',
            Style::Thick => '━',
        },
        (false, true) => match style {
            Style::Solid => '│',
            Style::Dotted => '╎',
            Style::Thick => '┃',
        },
        (true, true) => match bits {
            b if b == DOWN | RIGHT => '┌',
            b if b == DOWN | LEFT => '┐',
            b if b == UP | RIGHT => '└',
            b if b == UP | LEFT => '┘',
            b if b == UP | DOWN | RIGHT => '├',
            b if b == UP | DOWN | LEFT => '┤',
            b if b == LEFT | RIGHT | DOWN => '┬',
            b if b == LEFT | RIGHT | UP => '┴',
            _ => '┼',
        },
    }
}

// ---------------------------------------------------------------------------
// Flowchart and state diagram model

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    TD,
    LR,
    BT,
    RL,
}

fn parse_dir(s: &str) -> Dir {
    match s.trim().to_ascii_uppercase().as_str() {
        "LR" | ">" => Dir::LR,
        "RL" | "<" => Dir::RL,
        "BT" | "^" => Dir::BT,
        _ => Dir::TD,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Rect,
    Round,
    Diamond,
    Sub,
    /// A one-cell marker: a state diagram's start, end, choice, fork.
    Mark(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    None,
    Arrow,
    Circle,
    Cross,
}

struct FNode {
    lines: Vec<String>,
    shape: Shape,
    /// The label came from a definition, not from the id.
    labeled: bool,
}

struct FEdge {
    from: usize,
    to: usize,
    style: Style,
    start: Head,
    end: Head,
    label: Vec<String>,
    invisible: bool,
}

struct Graph {
    dir: Dir,
    default_shape: Shape,
    nodes: Vec<FNode>,
    edges: Vec<FEdge>,
    index: HashMap<String, usize>,
}

impl Graph {
    fn new(dir: Dir, default_shape: Shape) -> Self {
        Graph {
            dir,
            default_shape,
            nodes: Vec::new(),
            edges: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// The node `id`, created on first use; its label and shape are set by
    /// its first definition that has them.
    fn define(&mut self, id: &str, def: Option<(Vec<String>, Shape)>) -> usize {
        let v = match self.index.get(id) {
            Some(&v) => v,
            None => {
                self.nodes.push(FNode {
                    lines: label_lines(id, WRAP),
                    shape: self.default_shape,
                    labeled: false,
                });
                self.index.insert(id.to_string(), self.nodes.len() - 1);
                self.nodes.len() - 1
            }
        };
        if let Some((lines, shape)) = def
            && !self.nodes[v].labeled
        {
            self.nodes[v].lines = lines;
            self.nodes[v].shape = shape;
            self.nodes[v].labeled = true;
        }
        v
    }

    fn edge(&mut self, edge: FEdge) -> Result<(), String> {
        if self.edges.len() >= MAX_EDGES {
            return Err(TOO_LARGE.into());
        }
        self.edges.push(edge);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Flowchart parser

/// A cursor over one statement.
struct Cur {
    c: Vec<char>,
    i: usize,
}

impl Cur {
    fn new(s: &str) -> Self {
        Cur {
            c: s.chars().collect(),
            i: 0,
        }
    }
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }
    fn starts(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.at(k) == Some(ch))
    }
    fn eat(&mut self, s: &str) -> bool {
        let ok = self.starts(s);
        if ok {
            self.i += s.chars().count();
        }
        ok
    }
    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }
    fn done(&self) -> bool {
        self.i >= self.c.len()
    }
    /// The index of the next `s` at or after `from`.
    fn find(&self, from: usize, s: &str) -> Option<usize> {
        let pat: Vec<char> = s.chars().collect();
        (from..self.c.len()).find(|&k| self.c[k..].starts_with(&pat))
    }
    fn slice(&self, a: usize, b: usize) -> String {
        self.c[a.min(b)..b.min(self.c.len())].iter().collect()
    }
    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let start = self.i;
        while self.peek().is_some_and(&f) {
            self.i += 1;
        }
        self.slice(start, self.i)
    }
}

fn is_id(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits a line on `;` outside double quotes.
fn split_statements(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_flowchart(lines: &[String]) -> Result<Graph, String> {
    let stmts: Vec<String> = lines.iter().flat_map(|l| split_statements(l)).collect();
    let dir = stmts
        .first()
        .and_then(|h| h.split_whitespace().nth(1))
        .map(parse_dir)
        .unwrap_or(Dir::TD);
    let mut g = Graph::new(dir, Shape::Rect);
    for s in stmts.iter().skip(1) {
        flow_statement(&mut g, s)?;
    }
    if g.nodes.len() > MAX_NODES {
        return Err(TOO_LARGE.into());
    }
    Ok(g)
}

const IGNORED: &[&str] = &[
    "subgraph",
    "end",
    "classDef",
    "class",
    "style",
    "linkStyle",
    "click",
    "direction",
    "accTitle",
    "accDescr",
    "accTitle:",
    "accDescr:",
];

fn flow_statement(g: &mut Graph, s: &str) -> Result<(), String> {
    let first = s.split_whitespace().next().unwrap_or("");
    if IGNORED.contains(&first) || first.starts_with("accTitle") || first.starts_with("accDescr") {
        return Ok(());
    }
    let bad = || format!("flowchart: cannot read `{}`", short(s));
    let mut p = Cur::new(s);
    let mut left = node_group(&mut p, g).ok_or_else(bad)?;
    loop {
        p.ws();
        if p.done() {
            return Ok(());
        }
        let link = link(&mut p).ok_or_else(bad)?;
        p.ws();
        let right = node_group(&mut p, g).ok_or_else(bad)?;
        for &a in &left {
            for &b in &right {
                g.edge(FEdge {
                    from: a,
                    to: b,
                    style: link.style,
                    start: link.start,
                    end: link.end,
                    label: link.label.clone(),
                    invisible: link.invisible,
                })?;
            }
        }
        left = right;
    }
}

/// `A`, or `A & B & C`.
fn node_group(p: &mut Cur, g: &mut Graph) -> Option<Vec<usize>> {
    let mut out = vec![node(p, g)?];
    loop {
        let save = p.i;
        p.ws();
        if p.eat("&") {
            p.ws();
            out.push(node(p, g)?);
        } else {
            p.i = save;
            return Some(out);
        }
    }
}

/// Shape openers, longest first, with their closers.
const SHAPES: &[(&str, &[&str], Shape)] = &[
    ("(((", &[")))"], Shape::Round),
    ("([", &["])"], Shape::Round),
    ("((", &["))"], Shape::Round),
    ("[[", &["]]"], Shape::Sub),
    ("[(", &[")]"], Shape::Round),
    ("[/", &["/]", "\\]"], Shape::Rect),
    ("[\\", &["\\]", "/]"], Shape::Rect),
    ("{{", &["}}"], Shape::Diamond),
    ("[", &["]"], Shape::Rect),
    ("(", &[")"], Shape::Round),
    ("{", &["}"], Shape::Diamond),
    (">", &["]"], Shape::Rect),
];

/// One node reference: an id, an optional shape with its text, an
/// optional `:::class`.
fn node(p: &mut Cur, g: &mut Graph) -> Option<usize> {
    let start = p.i;
    while let Some(c) = p.peek() {
        let dash_in_id = c == '-' && p.i > start && p.at(1).is_some_and(is_id);
        if is_id(c) || dash_in_id {
            p.i += 1;
        } else {
            break;
        }
    }
    if p.i == start {
        return None;
    }
    let id = p.slice(start, p.i);
    let mut def = None;
    if p.eat("@{") {
        p.i = p.find(p.i, "}").map(|k| k + 1)?;
    } else {
        for (open, closers, shape) in SHAPES {
            if !p.starts(open) {
                continue;
            }
            let body = p.i + open.chars().count();
            if let Some((text, end)) = shape_text(p, body, closers) {
                def = Some((label_lines(&text, WRAP), *shape));
                p.i = end;
                break;
            }
        }
    }
    if p.eat(":::") {
        p.take_while(|c| is_id(c) || c == '-');
    }
    Some(g.define(&id, def))
}

/// The text of a shape starting at `body`, up to the nearest closer;
/// returns it and the index after the closer.
fn shape_text(p: &Cur, body: usize, closers: &[&str]) -> Option<(String, usize)> {
    let mut k = body;
    while p.c.get(k).is_some_and(|c| c.is_whitespace()) {
        k += 1;
    }
    if p.c.get(k) == Some(&'"') {
        let q = p.find(k + 1, "\"")?;
        let mut after = q + 1;
        while p.c.get(after).is_some_and(|c| c.is_whitespace()) {
            after += 1;
        }
        let closer = closers.iter().find(|c| {
            let pat: Vec<char> = c.chars().collect();
            p.c[after.min(p.c.len())..].starts_with(&pat)
        })?;
        return Some((p.slice(k + 1, q), after + closer.chars().count()));
    }
    let (at, closer) = closers
        .iter()
        .filter_map(|c| p.find(body, c).map(|at| (at, *c)))
        .min_by_key(|(at, _)| *at)?;
    Some((p.slice(body, at), at + closer.chars().count()))
}

struct Link {
    style: Style,
    start: Head,
    end: Head,
    label: Vec<String>,
    invisible: bool,
}

fn link_body(c: char) -> bool {
    matches!(c, '-' | '=' | '.' | '~')
}

fn end_head(p: &mut Cur) -> Head {
    let head = match p.peek() {
        Some('>') => Head::Arrow,
        Some('o') if !p.at(1).is_some_and(is_id) => Head::Circle,
        Some('x') if !p.at(1).is_some_and(is_id) => Head::Cross,
        _ => return Head::None,
    };
    p.i += 1;
    head
}

/// One link: `-->`, `-.->`, `==>`, `<-->`, `o--o`, `---`, `~~~`, with a
/// `|label|` or an inline `-- label -->` label.
fn link(p: &mut Cur) -> Option<Link> {
    let mut start = Head::None;
    if p.peek() == Some('<') {
        start = Head::Arrow;
        p.i += 1;
    } else if matches!(p.peek(), Some('o' | 'x')) && p.at(1).is_some_and(link_body) {
        start = if p.peek() == Some('o') {
            Head::Circle
        } else {
            Head::Cross
        };
        p.i += 1;
    }
    let body = p.take_while(link_body);
    if body.is_empty() {
        return None;
    }
    let mut end = end_head(p);
    let invisible = body.chars().all(|c| c == '~');
    let style = if body.contains('=') {
        Style::Thick
    } else if body.contains('.') {
        Style::Dotted
    } else {
        Style::Solid
    };
    let mut label = Vec::new();
    let opens_text = matches!(body.as_str(), "--" | "==" | "-.");
    if end == Head::None && opens_text && p.peek().is_some_and(char::is_whitespace) {
        let close = if body == "-." { ".-" } else { body.as_str() };
        if let Some(k) = p.find(p.i, close) {
            label = label_lines(&p.slice(p.i, k), WRAP);
            p.i = k;
            p.take_while(link_body);
            end = end_head(p);
        }
    }
    let save = p.i;
    p.ws();
    if p.peek() == Some('|') {
        let k = p.find(p.i + 1, "|")?;
        label = label_lines(&p.slice(p.i + 1, k), WRAP);
        p.i = k + 1;
    } else {
        p.i = save;
    }
    if label.iter().all(String::is_empty) {
        label.clear();
    }
    Some(Link {
        style,
        start,
        end,
        label,
        invisible,
    })
}

// ---------------------------------------------------------------------------
// State diagram parser

fn parse_state(lines: &[String]) -> Result<Graph, String> {
    let mut g = Graph::new(Dir::TD, Shape::Round);
    let mut scopes: Vec<String> = Vec::new();
    let mut in_note = false;
    for line in lines.iter().skip(1) {
        let l = line.trim().trim_end_matches(';').trim();
        let lower = l.to_ascii_lowercase();
        if in_note {
            in_note = lower != "end note";
            continue;
        }
        if lower.starts_with("note ") {
            in_note = !l.contains(':');
            continue;
        }
        if l.is_empty() || l == "--" || l == "{" {
            continue;
        }
        if l == "}" {
            scopes.pop();
            continue;
        }
        let first = lower.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "classdef" | "class" | "style" | "hide" | "scale" | "acctitle" | "accdescr"
        ) || first.starts_with("acctitle:")
            || first.starts_with("accdescr")
        {
            continue;
        }
        if first == "direction" {
            if scopes.is_empty() {
                g.dir = parse_dir(l.split_whitespace().nth(1).unwrap_or("TD"));
            }
            continue;
        }
        if first == "state" {
            state_decl(&mut g, l[5..].trim(), &mut scopes);
            continue;
        }
        if let Some(k) = l.find("-->") {
            let lhs = l[..k].trim();
            let rest = &l[k + 3..];
            let (rhs, label) = match rest.find(':') {
                Some(c) => (rest[..c].trim(), rest[c + 1..].trim()),
                None => (rest.trim(), ""),
            };
            if lhs.is_empty() || rhs.is_empty() {
                return Err(format!("stateDiagram: cannot read `{}`", short(l)));
            }
            let a = state_ref(&mut g, lhs, &scopes, true);
            let b = state_ref(&mut g, rhs, &scopes, false);
            let label = if label.is_empty() {
                Vec::new()
            } else {
                label_lines(label, WRAP)
            };
            g.edge(FEdge {
                from: a,
                to: b,
                style: Style::Solid,
                start: Head::None,
                end: Head::Arrow,
                label,
                invisible: false,
            })?;
            continue;
        }
        if let Some(c) = l.find(':') {
            let id = strip_class(l[..c].trim());
            if !id.is_empty() && id.chars().all(|c| is_id(c) || c == '-' || c == '.') {
                describe(&mut g, id, l[c + 1..].trim());
                continue;
            }
        }
        let id = strip_class(l);
        if !id.is_empty() && id.chars().all(|c| is_id(c) || c == '-' || c == '.') {
            g.define(id, None);
            continue;
        }
        return Err(format!("stateDiagram: cannot read `{}`", short(l)));
    }
    if g.nodes.len() > MAX_NODES {
        return Err(TOO_LARGE.into());
    }
    Ok(g)
}

fn strip_class(id: &str) -> &str {
    id.split(":::").next().unwrap_or(id).trim()
}

/// `A : text` adds a line of description to `A`; the first replaces the id.
fn describe(g: &mut Graph, id: &str, text: &str) {
    let v = g.define(id, None);
    let lines = label_lines(text, WRAP);
    let node = &mut g.nodes[v];
    if node.labeled {
        node.lines.extend(lines);
    } else {
        node.lines = lines;
        node.labeled = true;
    }
}

/// A transition end: a state, or `[*]` as the start (a source) or the end
/// (a target) of the innermost composite state.
fn state_ref(g: &mut Graph, raw: &str, scopes: &[String], source: bool) -> usize {
    let id = strip_class(raw);
    if id == "[*]" {
        let scope = scopes.last().map(String::as_str).unwrap_or("");
        let (key, mark) = if source {
            (format!("[*]start {scope}"), '●')
        } else {
            (format!("[*]end {scope}"), '◉')
        };
        return g.define(&key, Some((vec![String::new()], Shape::Mark(mark))));
    }
    g.define(id, None)
}

/// `state "long name" as A`, `state A <<choice>>`, `state A {`.
fn state_decl(g: &mut Graph, rest: &str, scopes: &mut Vec<String>) {
    let opens = rest.ends_with('{');
    let rest = rest.trim_end_matches('{').trim();
    if let Some(quoted) = rest.strip_prefix('"') {
        let Some(q) = quoted.find('"') else { return };
        let name = &quoted[..q];
        let after = quoted[q + 1..].trim();
        let Some(id) = after.strip_prefix("as").map(str::trim) else {
            return;
        };
        let id = strip_class(id.split_whitespace().next().unwrap_or(""));
        if id.is_empty() {
            return;
        }
        g.define(id, Some((label_lines(name, WRAP), Shape::Round)));
        if opens {
            scopes.push(id.to_string());
        }
        return;
    }
    let (head, desc) = match rest.find(':') {
        Some(c) => (rest[..c].trim(), Some(rest[c + 1..].trim())),
        None => (rest, None),
    };
    let mut words = head.split_whitespace();
    let id = strip_class(words.next().unwrap_or(""));
    if id.is_empty() {
        return;
    }
    let kind = words.next().unwrap_or("");
    if opens {
        scopes.push(id.to_string());
        return;
    }
    match kind {
        "<<choice>>" => {
            g.define(id, Some((vec![String::new()], Shape::Mark('◇'))));
        }
        "<<fork>>" | "<<join>>" => {
            g.define(id, Some((vec![String::new()], Shape::Mark('▬'))));
        }
        _ => match desc {
            Some(d) => describe(g, id, d),
            None => {
                g.define(id, None);
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Flowchart layout

/// An edge as laid out: reversed for `BT` / `RL` and to break cycles, the
/// heads swapped along with it.
struct LEdge {
    u: usize,
    v: usize,
    start: Head,
    end: Head,
    style: Style,
    label: Vec<String>,
    invisible: bool,
    /// Reversed to break a cycle: its ports sit off the box centre so it
    /// does not share the forward edges' line.
    back: bool,
}

impl LEdge {
    fn reverse(&mut self) {
        std::mem::swap(&mut self.u, &mut self.v);
        std::mem::swap(&mut self.start, &mut self.end);
    }
    /// The label sits by the arrowhead: at the start when only the start
    /// has one.
    fn label_at_start(&self) -> bool {
        self.start != Head::None && self.end == Head::None
    }
}

/// A node in the layered layout: a real node or a one-cell dummy on a
/// long edge.
struct LNode {
    real: Option<usize>,
    /// The edge a dummy belongs to.
    edge: usize,
    rank: usize,
    /// Size along the main axis.
    main: i64,
    /// Size along the cross axis.
    size: i64,
    cross: i64,
}

impl LNode {
    fn port(&self) -> i64 {
        self.cross + self.size / 2
    }
}

/// One rank-to-next-rank piece of an edge.
struct Seg {
    a: usize,
    b: usize,
    edge: usize,
    first: bool,
    last: bool,
}

/// A group of segments that share one track in a gap.
struct Bundle {
    segs: Vec<usize>,
    lo: i64,
    hi: i64,
    srcs: Vec<i64>,
    tgts: Vec<i64>,
}

fn draw_flow(g: &Graph) -> Result<Canvas, String> {
    let n = g.nodes.len();
    if n == 0 {
        return Err("flowchart: nothing to draw".into());
    }
    let horizontal = matches!(g.dir, Dir::LR | Dir::RL);
    let flip = matches!(g.dir, Dir::BT | Dir::RL);

    // Labels, with a mark for a self-loop.
    let mut lines: Vec<Vec<String>> = g.nodes.iter().map(|n| n.lines.clone()).collect();
    for e in &g.edges {
        if e.from == e.to
            && !e.invisible
            && !matches!(g.nodes[e.from].shape, Shape::Mark(_))
            && let Some(l) = lines[e.from].last_mut()
            && !l.ends_with('↻')
        {
            l.push_str(" ↻");
        }
    }
    let mut sizes: Vec<(i64, i64)> = (0..n)
        .map(|v| match g.nodes[v].shape {
            Shape::Mark(_) => (1, 1),
            _ => {
                let w = lines[v].iter().map(|l| str_width(l)).max().unwrap_or(0) as i64;
                (w + 4, lines[v].len() as i64 + 2)
            }
        })
        .collect();
    let along = |(w, h): (i64, i64)| if horizontal { (w, h) } else { (h, w) };

    let mut le: Vec<LEdge> = g
        .edges
        .iter()
        .filter(|e| e.from != e.to)
        .map(|e| {
            let mut l = LEdge {
                u: e.from,
                v: e.to,
                start: e.start,
                end: e.end,
                style: e.style,
                label: e.label.clone(),
                invisible: e.invisible,
                back: false,
            };
            if flip {
                l.reverse();
            }
            l
        })
        .collect();

    // Break cycles: reverse every edge that closes one.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in le.iter().enumerate() {
        out[e.u].push(i);
    }
    let mut color = vec![0u8; n];
    let mut back = vec![false; le.len()];
    for root in 0..n {
        if color[root] != 0 {
            continue;
        }
        color[root] = 1;
        let mut stack = vec![(root, 0usize)];
        while let Some(top) = stack.last_mut() {
            let v = top.0;
            if top.1 < out[v].len() {
                let ei = out[v][top.1];
                top.1 += 1;
                let t = le[ei].v;
                match color[t] {
                    0 => {
                        color[t] = 1;
                        stack.push((t, 0));
                    }
                    1 => back[ei] = true,
                    _ => {}
                }
            } else {
                color[v] = 2;
                stack.pop();
            }
        }
    }
    for (e, b) in le.iter_mut().zip(&back) {
        if *b {
            e.reverse();
            e.back = true;
        }
    }
    // A box a back edge touches grows by two cells across the main axis,
    // so the back edge gets a port of its own beside the centre.
    let mut padded = vec![false; n];
    for e in le.iter().filter(|e| e.back) {
        padded[e.u] = true;
        padded[e.v] = true;
    }
    for v in (0..n).filter(|&v| padded[v]) {
        if !matches!(g.nodes[v].shape, Shape::Mark(_)) {
            if horizontal {
                sizes[v].1 += 2;
            } else {
                sizes[v].0 += 2;
            }
        }
    }

    // Ranks: longest path from the sources; a source then moves down to
    // just above its nearest successor.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for (i, e) in le.iter().enumerate() {
        out[e.u].push(i);
        indeg[e.v] += 1;
    }
    let sources: Vec<bool> = indeg.iter().map(|&d| d == 0).collect();
    let mut rank = vec![0usize; n];
    let mut queue: VecDeque<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    while let Some(v) = queue.pop_front() {
        for &ei in &out[v] {
            let t = le[ei].v;
            rank[t] = rank[t].max(rank[v] + 1);
            indeg[t] -= 1;
            if indeg[t] == 0 {
                queue.push_back(t);
            }
        }
    }
    for v in 0..n {
        if sources[v]
            && let Some(m) = out[v].iter().map(|&ei| rank[le[ei].v]).min()
        {
            rank[v] = m.saturating_sub(1);
        }
    }

    // Layered nodes and segments, with dummies on long edges.
    let mut ln: Vec<LNode> = (0..n)
        .map(|v| {
            let (main, size) = along(sizes[v]);
            LNode {
                real: Some(v),
                edge: 0,
                rank: rank[v],
                main,
                size,
                cross: 0,
            }
        })
        .collect();
    let mut segs: Vec<Seg> = Vec::new();
    for (ei, e) in le.iter().enumerate() {
        if e.invisible {
            continue;
        }
        let (ru, rv) = (rank[e.u], rank[e.v]);
        if rv <= ru {
            continue;
        }
        let mut prev = e.u;
        for r in ru + 1..rv {
            ln.push(LNode {
                real: None,
                edge: ei,
                rank: r,
                main: 1,
                size: 1,
                cross: 0,
            });
            let d = ln.len() - 1;
            segs.push(Seg {
                a: prev,
                b: d,
                edge: ei,
                first: prev == e.u,
                last: false,
            });
            prev = d;
            if ln.len() > MAX_NODES * 4 {
                return Err(TOO_LARGE.into());
            }
        }
        segs.push(Seg {
            a: prev,
            b: e.v,
            edge: ei,
            first: prev == e.u,
            last: true,
        });
    }
    let nr = ln.iter().map(|l| l.rank).max().unwrap_or(0) + 1;
    let mut order: Vec<Vec<usize>> = vec![Vec::new(); nr];
    for (i, l) in ln.iter().enumerate() {
        order[l.rank].push(i);
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); ln.len()];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); ln.len()];
    for s in &segs {
        preds[s.b].push(s.a);
        succs[s.a].push(s.b);
    }
    for e in le
        .iter()
        .filter(|e| e.invisible && rank[e.v] == rank[e.u] + 1)
    {
        preds[e.v].push(e.u);
        succs[e.u].push(e.v);
    }

    // Order within ranks: barycenter sweeps down and up.
    let mut pos = vec![0usize; ln.len()];
    let reindex = |order: &[usize], pos: &mut [usize]| {
        for (k, &v) in order.iter().enumerate() {
            pos[v] = k;
        }
    };
    for o in &order {
        reindex(o, &mut pos);
    }
    for _ in 0..4 {
        for (r, nbrs) in (1..nr)
            .map(|r| (r, &preds))
            .chain((0..nr.saturating_sub(1)).rev().map(|r| (r, &succs)))
        {
            let mut keyed: Vec<(f64, usize, usize)> = order[r]
                .iter()
                .map(|&v| {
                    let nb = &nbrs[v];
                    let key = if nb.is_empty() {
                        pos[v] as f64
                    } else {
                        nb.iter().map(|&u| pos[u] as f64).sum::<f64>() / nb.len() as f64
                    };
                    (key, pos[v], v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            order[r] = keyed.into_iter().map(|k| k.2).collect();
            reindex(&order[r], &mut pos);
        }
    }

    // Main-axis sizes: a rank is as deep as its deepest box.
    let rank_main: Vec<i64> = (0..nr)
        .map(|r| {
            order[r]
                .iter()
                .filter(|&&v| ln[v].real.is_some())
                .map(|&v| ln[v].main)
                .max()
                .unwrap_or(1)
        })
        .collect();
    for l in ln.iter_mut().filter(|l| l.real.is_none()) {
        l.main = rank_main[l.rank];
    }

    // Cross positions.
    let gap = if horizontal { 1 } else { 3 };
    for o in &order {
        let mut acc = 0;
        for &v in o {
            ln[v].cross = acc;
            acc += ln[v].size + gap;
        }
    }
    for down in [true, false, true] {
        let ranks: Vec<usize> = if down {
            (1..nr).collect()
        } else {
            (0..nr.saturating_sub(1)).rev().collect()
        };
        for r in ranks {
            let desired: Vec<f64> = order[r]
                .iter()
                .map(|&v| {
                    let nb = if down { &preds[v] } else { &succs[v] };
                    if nb.is_empty() {
                        ln[v].cross as f64
                    } else {
                        let mean =
                            nb.iter().map(|&u| ln[u].port() as f64).sum::<f64>() / nb.len() as f64;
                        mean - (ln[v].size / 2) as f64
                    }
                })
                .collect();
            let widths: Vec<i64> = order[r].iter().map(|&v| ln[v].size).collect();
            let xs = fit_in_order(&desired, &widths, gap);
            for (k, &v) in order[r].iter().enumerate() {
                ln[v].cross = xs[k];
            }
        }
    }
    let min = ln.iter().map(|l| l.cross).min().unwrap_or(0);
    for l in ln.iter_mut() {
        l.cross -= min;
    }
    if ln.iter().any(|l| l.cross + l.size > MAX_COLS.max(MAX_ROWS)) {
        return Err(TOO_LARGE.into());
    }

    // Where each segment leaves and enters, on the cross axis.
    // A back edge takes the outer side of a node at the end of its rank,
    // else the right side.
    let outer_left = |v: usize| pos[v] == 0 && order[ln[v].rank].len() > 1;
    let back_left: Vec<bool> = le
        .iter()
        .map(|e| e.back && (outer_left(e.u) || outer_left(e.v)))
        .collect();
    let ports: Vec<(i64, i64)> = segs
        .iter()
        .map(|s| {
            let port = |v: usize| {
                let l = &ln[v];
                let fits = |off: i64| l.port() + off <= l.cross + l.size - 2;
                let off = [(l.size / 4).max(2), 1]
                    .into_iter()
                    .find(|&o| fits(o))
                    .unwrap_or(0);
                if l.real.is_none() || !le[s.edge].back {
                    l.port()
                } else if back_left[s.edge] {
                    l.port() - off
                } else {
                    l.port() + off
                }
            };
            (port(s.a), port(s.b))
        })
        .collect();

    // Gaps: tracks for segments that jog, label space.
    let seg_label = |s: &Seg| -> Option<&Vec<String>> {
        let e = &le[s.edge];
        let here = if e.label_at_start() { s.first } else { s.last };
        (here && !e.label.is_empty()).then_some(&e.label)
    };
    let mut track = vec![0i64; segs.len()];
    let mut gap_tracks = vec![0i64; nr];
    let mut gap_label = vec![0i64; nr];
    for r in 0..nr.saturating_sub(1) {
        let ids: Vec<usize> = (0..segs.len())
            .filter(|&s| ln[segs[s].a].rank == r)
            .collect();
        gap_label[r] = ids
            .iter()
            .filter(|&&s| horizontal || !le[segs[s].edge].label_at_start())
            .filter_map(|&s| seg_label(&segs[s]))
            .map(|l| {
                if horizontal {
                    str_width(&l.join(" ")) as i64 + 3
                } else {
                    l.len() as i64
                }
            })
            .max()
            .unwrap_or(0);
        // Room for a line between the heads of a short two-headed edge.
        if ids.iter().any(|&s| {
            let e = &le[segs[s].edge];
            segs[s].first && segs[s].last && e.start != Head::None && e.end != Head::None
        }) {
            gap_label[r] = gap_label[r].max(1);
        }
        let jogs: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|&s| ports[s].0 != ports[s].1)
            .collect();
        let bundles = bundle(&jogs, &segs, &ports);
        let placed = order_bundles(&bundles);
        let mut tr = vec![0i64; bundles.len()];
        for (k, &b) in placed.iter().enumerate() {
            let overlaps =
                |o: usize| bundles[o].hi >= bundles[b].lo && bundles[b].hi >= bundles[o].lo;
            tr[b] = placed[..k]
                .iter()
                .filter(|&&o| overlaps(o))
                .map(|&o| tr[o] + 1)
                .max()
                .unwrap_or(0);
            for &s in &bundles[b].segs {
                track[s] = tr[b];
            }
        }
        gap_tracks[r] = tr.iter().map(|t| t + 1).max().unwrap_or(0);
    }
    let mut rank_start = vec![0i64; nr];
    let mut gap_start = vec![0i64; nr];
    for r in 0..nr {
        gap_start[r] = rank_start[r] + rank_main[r];
        if r + 1 < nr {
            rank_start[r + 1] = gap_start[r] + 2 + gap_tracks[r] + gap_label[r];
        }
    }
    if rank_start[nr - 1] + rank_main[nr - 1] > MAX_COLS.max(MAX_ROWS) {
        return Err(TOO_LARGE.into());
    }

    let at = |m: i64, c: i64| if horizontal { (m, c) } else { (c, m) };
    let mut cv = Canvas::default();

    // Lines: dummies straight through their rank, then every segment.
    for l in ln.iter().filter(|l| l.real.is_none()) {
        let (x0, y0) = at(rank_start[l.rank], l.port());
        let (x1, y1) = at(rank_start[l.rank] + rank_main[l.rank], l.port());
        cv.seg(x0, y0, x1, y1, le[l.edge].style);
    }
    let ends = |s: &Seg| {
        let r = ln[s.a].rank;
        let m0 = rank_start[r] + ln[s.a].main;
        let m1 = if ln[s.b].real.is_some() {
            rank_start[r + 1] - 1
        } else {
            rank_start[r + 1]
        };
        (r, m0, m1)
    };
    for (si, s) in segs.iter().enumerate() {
        let (r, m0, m1) = ends(s);
        let (cs, ct) = ports[si];
        let mut pts = vec![at(m0, cs)];
        if cs != ct {
            let tm = gap_start[r] + 1 + track[si];
            pts.push(at(tm, cs));
            pts.push(at(tm, ct));
        }
        pts.push(at(m1, ct));
        cv.path(&pts, le[s.edge].style);
    }

    // Boxes.
    for l in &ln {
        let Some(v) = l.real else { continue };
        let (x, y) = at(rank_start[l.rank], l.cross);
        let (w, h) = sizes[v];
        let set = match g.nodes[v].shape {
            Shape::Mark(c) => {
                cv.put(x, y, c, CellKind::Border);
                continue;
            }
            Shape::Rect => ['┌', '┐', '└', '┘', '─', '│'],
            Shape::Round => ['╭', '╮', '╰', '╯', '─', '│'],
            Shape::Diamond => ['╱', '╲', '╲', '╱', '─', '│'],
            Shape::Sub => ['╔', '╗', '╚', '╝', '═', '║'],
        };
        cv.frame_box((x, y, w, h), set, CellKind::Border, &lines[v]);
    }

    // Arrowheads, and junctions where a line meets a border.
    let (fwd, back_c, exit_from, exit_to, entry_to) = if horizontal {
        ('▶', '◀', '│', '├', '┤')
    } else {
        ('▼', '▲', '─', '┬', '┴')
    };
    let head_char = |h: Head, forward: bool| match h {
        Head::Arrow if forward => fwd,
        Head::Arrow => back_c,
        Head::Circle => '○',
        Head::Cross => '×',
        Head::None => ' ',
    };
    for (si, s) in segs.iter().enumerate() {
        let e = &le[s.edge];
        let (r, m0, m1) = ends(s);
        let (cs, ct) = ports[si];
        if s.first {
            let (x, y) = at(m0, cs);
            if e.start != Head::None {
                cv.put(x, y, head_char(e.start, false), CellKind::Arrow);
            } else {
                let (x, y) = at(rank_start[r] + ln[s.a].main - 1, cs);
                cv.swap_border(x, y, exit_from, exit_to);
            }
        }
        if s.last {
            let (x, y) = at(m1, ct);
            if e.end != Head::None {
                cv.put(x, y, head_char(e.end, true), CellKind::Arrow);
            } else {
                let (x, y) = at(m1 + 1, ct);
                cv.swap_border(x, y, exit_from, entry_to);
            }
        }
    }

    // Edge labels.
    for (si, s) in segs.iter().enumerate() {
        let Some(label) = seg_label(s) else { continue };
        let r = ln[s.a].rank;
        let mut ct = ports[si].1;
        let mut region = gap_start[r] + 1 + gap_tracks[r];
        if !horizontal && le[s.edge].label_at_start() {
            // Beside the arrowhead under the source.
            ct = ports[si].0;
            region = ends(s).1;
        }
        if horizontal {
            let text = label.join(" ");
            let w = str_width(&text) as i64;
            // Centred between the exit and the arrowhead, which sits at the
            // far end of the gap or, for a start head, at its near end.
            let lead = if le[s.edge].label_at_start() { 2 } else { 1 };
            let x = region + lead + (gap_label[r] - 3 - w).max(0) / 2;
            if (x..x + w).all(|cx| !cv.is_text(cx, ct)) {
                cv.text(x, ct, &text, CellKind::Label);
            } else if let Some(y) = [ct - 1, ct + 1]
                .into_iter()
                .find(|&y| y >= 0 && (x - 1..=x + w).all(|cx| cv.get(cx, y) == Cell::Empty))
            {
                cv.text(x, y, &text, CellKind::Label);
            }
        } else {
            let w = label.iter().map(|l| str_width(l)).max().unwrap_or(0) as i64;
            let h = label.len() as i64;
            let free = |x: i64| {
                x >= 0
                    && (0..h)
                        .all(|k| (x - 1..=x + w).all(|cx| cv.get(cx, region + k) == Cell::Empty))
            };
            let found = [ct + 2, ct - 1 - w]
                .into_iter()
                .chain((1..60).map(|k| ct + 2 + k))
                .find(|&x| free(x));
            for (k, line) in label.iter().enumerate() {
                let y = region + k as i64;
                match found {
                    Some(x) => {
                        cv.text(x, y, line, CellKind::Label);
                    }
                    None => cv.text_into_empty(ct + 2, y, line, CellKind::Label),
                }
            }
        }
    }
    Ok(cv)
}

/// Groups the jogging segments of one gap: a fan-out from one node shares
/// a track, then a fan-in to one node, then the rest one each.
fn bundle(jogs: &[usize], segs: &[Seg], ports: &[(i64, i64)]) -> Vec<Bundle> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut taken = vec![false; jogs.len()];
    for by_source in [true, false] {
        let key = |s: usize| {
            if by_source {
                (segs[s].a, ports[s].0)
            } else {
                (segs[s].b, ports[s].1)
            }
        };
        let mut keys: Vec<(usize, i64)> = Vec::new();
        for (k, &s) in jogs.iter().enumerate() {
            if !taken[k] && !keys.contains(&key(s)) {
                keys.push(key(s));
            }
        }
        for node in keys {
            let members: Vec<usize> = (0..jogs.len())
                .filter(|&k| !taken[k] && key(jogs[k]) == node)
                .collect();
            if members.len() >= 2 {
                for &k in &members {
                    taken[k] = true;
                }
                groups.push(members.iter().map(|&k| jogs[k]).collect());
            }
        }
    }
    for (k, &s) in jogs.iter().enumerate() {
        if !taken[k] {
            groups.push(vec![s]);
        }
    }
    let mut bundles: Vec<Bundle> = groups
        .into_iter()
        .map(|segs_in| {
            let srcs: Vec<i64> = segs_in.iter().map(|&s| ports[s].0).collect();
            let tgts: Vec<i64> = segs_in.iter().map(|&s| ports[s].1).collect();
            let all = srcs.iter().chain(&tgts);
            Bundle {
                lo: all.clone().copied().min().unwrap_or(0),
                hi: all.copied().max().unwrap_or(0),
                segs: segs_in,
                srcs,
                tgts,
            }
        })
        .collect();
    bundles.sort_by_key(|b| (b.lo, b.hi));
    bundles
}

/// The order bundles take tracks in, nearest the source first: each pick
/// is the bundle whose lines cross the fewest of the others' when it goes
/// above them.
fn order_bundles(bundles: &[Bundle]) -> Vec<usize> {
    if bundles.len() > 80 {
        return (0..bundles.len()).collect();
    }
    let inside = |c: i64, b: &Bundle| c >= b.lo && c <= b.hi;
    let cost = |x: &Bundle, y: &Bundle| {
        y.srcs.iter().filter(|&&c| inside(c, x)).count()
            + x.tgts.iter().filter(|&&c| inside(c, y)).count()
    };
    let mut rest: Vec<usize> = (0..bundles.len()).collect();
    let mut placed = Vec::with_capacity(rest.len());
    while !rest.is_empty() {
        let best = (0..rest.len())
            .min_by_key(|&k| {
                rest.iter()
                    .filter(|&&o| o != rest[k])
                    .map(|&o| cost(&bundles[rest[k]], &bundles[o]))
                    .sum::<usize>()
            })
            .unwrap_or(0);
        placed.push(rest.remove(best));
    }
    placed
}

/// Positions for boxes of `widths`, in this order, `gap` apart, as close
/// as possible to `desired` (least squares; pool-adjacent-violators on the
/// positions less their minimum offsets).
fn fit_in_order(desired: &[f64], widths: &[i64], gap: i64) -> Vec<i64> {
    let mut offsets = Vec::with_capacity(widths.len());
    let mut acc = 0;
    for w in widths {
        offsets.push(acc);
        acc += w + gap;
    }
    // Blocks of (sum, count).
    let mut blocks: Vec<(f64, usize)> = Vec::new();
    for (d, o) in desired.iter().zip(&offsets) {
        blocks.push((d - *o as f64, 1));
        while blocks.len() >= 2 {
            let (s1, c1) = blocks[blocks.len() - 1];
            let (s0, c0) = blocks[blocks.len() - 2];
            if s0 / c0 as f64 > s1 / c1 as f64 {
                blocks.pop();
                if let Some(last) = blocks.last_mut() {
                    *last = (s0 + s1, c0 + c1);
                }
            } else {
                break;
            }
        }
    }
    let mut out = Vec::with_capacity(widths.len());
    for (s, c) in blocks {
        let y = (s / c as f64).round();
        let y = if y.is_finite() { y as i64 } else { 0 };
        for _ in 0..c {
            out.push(y + offsets[out.len()]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Sequence diagrams

struct Part {
    lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SHead {
    None,
    Arrow,
    Cross,
    Open,
}

enum NotePos {
    Left(usize),
    Right(usize),
    Over(usize, usize),
}

enum Ev {
    Msg {
        from: usize,
        to: usize,
        lines: Vec<String>,
        dashed: bool,
        head: SHead,
        both: bool,
    },
    Note {
        pos: NotePos,
        lines: Vec<String>,
    },
    /// A block's dashed line; an empty label draws a plain one.
    Frame(String),
}

struct Seq {
    parts: Vec<Part>,
    index: HashMap<String, usize>,
    events: Vec<Ev>,
    title: Option<String>,
}

impl Seq {
    fn part(&mut self, id: &str) -> usize {
        if let Some(&i) = self.index.get(id) {
            return i;
        }
        self.parts.push(Part {
            lines: label_lines(id, SEQ_WRAP),
        });
        self.index.insert(id.to_string(), self.parts.len() - 1);
        self.parts.len() - 1
    }
}

/// Message arrows, longest first: (token, dashed, head, heads at both
/// ends).
const SEQ_ARROWS: &[(&str, bool, SHead, bool)] = &[
    ("<<-->>", true, SHead::Arrow, true),
    ("<<->>", false, SHead::Arrow, true),
    ("-->>", true, SHead::Arrow, false),
    ("->>", false, SHead::Arrow, false),
    ("--x", true, SHead::Cross, false),
    ("-x", false, SHead::Cross, false),
    ("--)", true, SHead::Open, false),
    ("-)", false, SHead::Open, false),
    ("-->", true, SHead::None, false),
    ("->", false, SHead::None, false),
];

fn parse_sequence(lines: &[String]) -> Result<Seq, String> {
    let mut seq = Seq {
        parts: Vec::new(),
        index: HashMap::new(),
        events: Vec::new(),
        title: None,
    };
    let mut number: Option<usize> = None;
    // Open blocks: `true` for a `box`, whose `end` draws nothing.
    let mut blocks: Vec<bool> = Vec::new();
    for line in lines.iter().skip(1) {
        let l = line.trim().trim_end_matches(';').trim();
        let mut words = l.splitn(2, char::is_whitespace);
        let first = words.next().unwrap_or("");
        let rest = words.next().unwrap_or("").trim();
        let lower = first.to_ascii_lowercase();
        match lower.as_str() {
            "participant" | "actor" => {
                participant(&mut seq, rest, lower == "actor");
                continue;
            }
            "create" => {
                let mut w = rest.splitn(2, char::is_whitespace);
                let kind = w.next().unwrap_or("");
                participant(&mut seq, w.next().unwrap_or("").trim(), kind == "actor");
                continue;
            }
            "autonumber" => {
                number = Some(1);
                continue;
            }
            "title" => {
                seq.title = Some(clean(rest.trim_start_matches(':').trim()));
                continue;
            }
            "activate" | "deactivate" | "destroy" | "link" | "links" | "properties" | "details" => {
                continue;
            }
            "box" => {
                blocks.push(true);
                continue;
            }
            "loop" | "alt" | "opt" | "par" | "critical" | "break" => {
                blocks.push(false);
                seq.events.push(Ev::Frame(clean(l)));
                continue;
            }
            "rect" => {
                blocks.push(false);
                seq.events.push(Ev::Frame(String::new()));
                continue;
            }
            "else" | "and" | "option" => {
                seq.events.push(Ev::Frame(clean(l)));
                continue;
            }
            "end" if rest.is_empty() => {
                if blocks.pop() == Some(false) {
                    seq.events.push(Ev::Frame(String::new()));
                }
                continue;
            }
            "note" => {
                note(&mut seq, rest)
                    .ok_or_else(|| format!("sequenceDiagram: cannot read `{}`", short(l)))?;
                continue;
            }
            _ => {}
        }
        if lower.starts_with("acctitle") || lower.starts_with("accdescr") {
            continue;
        }
        if lower.starts_with("title:") {
            seq.title = Some(clean(l[6..].trim()));
            continue;
        }
        message(&mut seq, l, &mut number)
            .ok_or_else(|| format!("sequenceDiagram: cannot read `{}`", short(l)))?;
        if seq.events.len() > MAX_EDGES || seq.parts.len() > MAX_NODES {
            return Err(TOO_LARGE.into());
        }
    }
    if seq.parts.is_empty() {
        return Err("sequenceDiagram: no participants".into());
    }
    Ok(seq)
}

/// `A`, `A as Alice`.
fn participant(seq: &mut Seq, rest: &str, actor: bool) {
    let (id, alias) = match rest.find(" as ") {
        Some(k) => (rest[..k].trim(), Some(rest[k + 4..].trim())),
        None => (rest.trim(), None),
    };
    if id.is_empty() {
        return;
    }
    let i = seq.part(id);
    let mut lines = label_lines(alias.unwrap_or(id), SEQ_WRAP);
    if actor && let Some(first) = lines.first_mut() {
        first.insert_str(0, "☺ ");
    }
    seq.parts[i].lines = lines;
}

/// `left of A: t`, `right of A: t`, `over A: t`, `over A,B: t`.
fn note(seq: &mut Seq, rest: &str) -> Option<()> {
    let (place, text) = rest.split_once(':')?;
    let place = place.trim();
    let lower = place.to_ascii_lowercase();
    let lines = label_lines(text, SEQ_WRAP);
    let pos = if lower.starts_with("left of") {
        NotePos::Left(seq.part(place[7..].trim()))
    } else if lower.starts_with("right of") {
        NotePos::Right(seq.part(place[8..].trim()))
    } else if lower.starts_with("over") {
        let who = place[4..].trim();
        match who.split_once(',') {
            Some((a, b)) => NotePos::Over(seq.part(a.trim()), seq.part(b.trim())),
            None => {
                let a = seq.part(who);
                NotePos::Over(a, a)
            }
        }
    } else {
        return None;
    };
    seq.events.push(Ev::Note { pos, lines });
    Some(())
}

/// `A->>B: text` and the other arrows, with `+`/`-` activation marks.
fn message(seq: &mut Seq, l: &str, number: &mut Option<usize>) -> Option<()> {
    let (head, text) = match l.find(':') {
        Some(k) => (&l[..k], l[k + 1..].trim()),
        None => (l, ""),
    };
    let (at, (token, dashed, kind, both)) = head.char_indices().find_map(|(i, _)| {
        SEQ_ARROWS
            .iter()
            .find(|a| head[i..].starts_with(a.0))
            .map(|a| (i, *a))
    })?;
    let from = head[..at].trim();
    let to = head[at + token.len()..]
        .trim()
        .trim_start_matches(['+', '-'])
        .trim();
    if from.is_empty() || to.is_empty() {
        return None;
    }
    let (from, to) = (seq.part(from), seq.part(to));
    let mut lines = if text.is_empty() {
        Vec::new()
    } else {
        label_lines(text, SEQ_WRAP)
    };
    if let Some(n) = number {
        match lines.first_mut() {
            Some(first) => first.insert_str(0, &format!("{n}. ")),
            None => lines.push(format!("{n}.")),
        }
        *n += 1;
    }
    seq.events.push(Ev::Msg {
        from,
        to,
        lines,
        dashed,
        head: kind,
        both,
    });
    Some(())
}

fn seq_head(h: SHead, rightward: bool) -> char {
    match (h, rightward) {
        (SHead::Arrow, true) => '▶',
        (SHead::Arrow, false) => '◀',
        (SHead::Cross, _) => '×',
        (SHead::Open, true) => '⟩',
        (SHead::Open, false) => '⟨',
        (SHead::None, _) => ' ',
    }
}

fn lines_width(lines: &[String]) -> i64 {
    lines.iter().map(|l| str_width(l)).max().unwrap_or(0) as i64
}

fn draw_sequence(seq: &Seq) -> Result<Canvas, String> {
    let n = seq.parts.len();
    let hw: Vec<i64> = seq
        .parts
        .iter()
        .map(|p| lines_width(&p.lines) + 4)
        .collect();
    let hh = seq.parts.iter().map(|p| p.lines.len()).max().unwrap_or(1) as i64 + 2;

    // Distances between neighbouring lifelines.
    let mut dist: Vec<i64> = (0..n.saturating_sub(1))
        .map(|i| (hw[i] - hw[i] / 2) + 2 + hw[i + 1] / 2)
        .collect();
    let widen = |dist: &mut Vec<i64>, lo: usize, hi: usize, need: i64| {
        if lo >= hi || hi > dist.len() {
            return;
        }
        let cur: i64 = dist[lo..hi].iter().sum();
        if cur < need {
            dist[hi - 1] += need - cur;
        }
    };
    for ev in &seq.events {
        match ev {
            Ev::Msg {
                from, to, lines, ..
            } => {
                let lw = lines_width(lines);
                if from == to {
                    widen(&mut dist, *from, from + 1, lw + 7);
                } else {
                    widen(&mut dist, *from.min(to), *from.max(to), lw + 4);
                }
            }
            Ev::Note { pos, lines } => {
                let nw = lines_width(lines) + 4;
                match pos {
                    NotePos::Right(i) => widen(&mut dist, *i, i + 1, nw + 4),
                    NotePos::Left(i) if *i > 0 => widen(&mut dist, i - 1, *i, nw + 4),
                    _ => {}
                }
            }
            Ev::Frame(_) => {}
        }
    }
    let mut cx = vec![0i64; n];
    for i in 1..n {
        cx[i] = cx[i - 1] + dist[i - 1];
    }
    let note_geom = |pos: &NotePos, lines: &[String], cx: &[i64]| -> (i64, i64) {
        let nw = lines_width(lines) + 4;
        match *pos {
            NotePos::Right(i) => (cx[i] + 2, nw),
            NotePos::Left(i) => (cx[i] - 1 - nw, nw),
            NotePos::Over(a, b) => {
                let (lo, hi) = (cx[a].min(cx[b]), cx[a].max(cx[b]));
                let w = nw.max(hi - lo + 4);
                ((lo + hi) / 2 - w / 2, w)
            }
        }
    };
    let mut min_x = (0..n).map(|i| cx[i] - hw[i] / 2).min().unwrap_or(0);
    for ev in &seq.events {
        if let Ev::Note { pos, lines } = ev {
            min_x = min_x.min(note_geom(pos, lines, &cx).0);
        }
    }
    for c in cx.iter_mut() {
        *c -= min_x;
    }
    if cx.last().is_some_and(|&c| c > MAX_COLS) {
        return Err(TOO_LARGE.into());
    }

    let mut cv = Canvas::default();
    let mut y = 0;
    if let Some(title) = &seq.title {
        cv.text(0, 0, title, CellKind::Frame);
        y = 2;
    }
    let header = |cv: &mut Canvas, y: i64, top: bool| {
        for (i, p) in seq.parts.iter().enumerate() {
            let x = cx[i] - hw[i] / 2;
            let set = ['┌', '┐', '└', '┘', '─', '│'];
            cv.frame_box((x, y, hw[i], hh), set, CellKind::Border, &p.lines);
            if top {
                cv.swap_border(cx[i], y + hh - 1, '─', '┬');
            } else {
                cv.swap_border(cx[i], y, '─', '┴');
            }
        }
    };
    header(&mut cv, y, true);
    let life_top = y + hh;
    y = life_top + 1;

    // Lifelines go first so arrows join them and frames leave them whole;
    // their length is known only at the end, so they are drawn long now
    // and the bottom headers cover the rest.
    let body_rows: i64 = seq
        .events
        .iter()
        .map(|ev| match ev {
            Ev::Msg {
                from, to, lines, ..
            } if from == to => (lines.len() as i64).max(2),
            Ev::Msg { lines, .. } => lines.len() as i64 + 1,
            Ev::Note { lines, .. } => lines.len() as i64 + 2,
            Ev::Frame(_) => 1,
        })
        .sum();
    let bottom = y + body_rows + 1;
    if bottom > MAX_ROWS {
        return Err(TOO_LARGE.into());
    }
    for &c in &cx {
        cv.seg(c, life_top, c, bottom - 1, Style::Solid);
    }

    let mut frames: Vec<(i64, &str)> = Vec::new();
    for ev in &seq.events {
        match ev {
            Ev::Msg {
                from,
                to,
                lines,
                dashed,
                head,
                both,
            } => {
                let style = if *dashed { Style::Dotted } else { Style::Solid };
                let (a, b) = (cx[*from], cx[*to]);
                if from == to {
                    let k = (lines.len() as i64).max(2);
                    let last = y + k - 1;
                    let back = if *head == SHead::None { a } else { a + 1 };
                    cv.path(&[(a, y), (a + 3, y), (a + 3, last), (back, last)], style);
                    if *head != SHead::None {
                        cv.put(a + 1, last, seq_head(*head, false), CellKind::Arrow);
                    }
                    for (r, line) in lines.iter().enumerate() {
                        cv.text(a + 5, y + r as i64, line, CellKind::Label);
                    }
                    y += k;
                    continue;
                }
                let lw = lines_width(lines);
                let (lo, hi) = (a.min(b), a.max(b));
                for (r, line) in lines.iter().enumerate() {
                    let x = (lo + hi + 1) / 2 - lw / 2;
                    cv.text(x.max(lo + 1), y + r as i64, line, CellKind::Label);
                }
                y += lines.len() as i64;
                let right = b > a;
                let step = if right { 1 } else { -1 };
                let tip = if *head == SHead::None { b } else { b - step };
                let tail = if *both { a + step } else { a };
                cv.seg(a, y, tip, y, style);
                if *head != SHead::None {
                    cv.put(b - step, y, seq_head(*head, right), CellKind::Arrow);
                }
                if *both && tail != a {
                    cv.put(tail, y, seq_head(*head, !right), CellKind::Arrow);
                }
                y += 1;
            }
            Ev::Note { pos, lines } => {
                let (x, w) = note_geom(pos, lines, &cx);
                let h = lines.len() as i64 + 2;
                let set = ['┌', '┐', '└', '┘', '─', '│'];
                cv.frame_box((x, y, w, h), set, CellKind::Frame, lines);
                y += h;
            }
            Ev::Frame(label) => {
                frames.push((y, label));
                y += 1;
            }
        }
    }
    header(&mut cv, bottom, false);

    let width = frames
        .iter()
        .map(|(_, l)| str_width(l) as i64 + 6)
        .chain([cv.width()])
        .max()
        .unwrap_or(0);
    for (fy, label) in frames {
        let mut x = 0;
        if !label.is_empty() {
            x = cv.text(0, fy, "┄┄ ", CellKind::Frame);
            x += cv.text(x, fy, label, CellKind::Frame);
            x += cv.text(x, fy, " ", CellKind::Frame);
        }
        while x < width {
            if !matches!(cv.get(x, fy), Cell::Line(..)) {
                cv.put(x, fy, '┄', CellKind::Frame);
            }
            x += 1;
        }
    }
    Ok(cv)
}

// ---------------------------------------------------------------------------
// Pie charts

/// The bar for 100 %.
const PIE_BAR: usize = 30;

fn draw_pie(lines: &[String]) -> Result<Canvas, String> {
    let mut title: Option<String> = None;
    let mut show_data = false;
    let mut slices: Vec<(String, f64)> = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let mut l = line.as_str();
        if k == 0 {
            l = l.strip_prefix("pie").unwrap_or(l).trim();
        }
        if let Some(rest) = l.strip_prefix("showData") {
            show_data = true;
            l = rest.trim();
        }
        if l.is_empty() || l.starts_with("accTitle") || l.starts_with("accDescr") {
            continue;
        }
        if let Some(t) = l.strip_prefix("title") {
            title = Some(clean(t.trim()));
            continue;
        }
        let bad = || format!("pie: cannot read `{}`", short(l));
        let (name, value) = l.rsplit_once(':').ok_or_else(bad)?;
        let value: f64 = value.trim().parse().map_err(|_| bad())?;
        if !value.is_finite() || value < 0.0 {
            return Err(bad());
        }
        let name = name.trim().trim_matches('"');
        slices.push((label_lines(name, WRAP).join(" "), value));
    }
    let total: f64 = slices.iter().map(|s| s.1).sum();
    if slices.is_empty() || total <= 0.0 || !total.is_finite() {
        return Err("pie: no data".into());
    }
    let mut cv = Canvas::default();
    let mut y = 0;
    if let Some(t) = &title {
        cv.text(0, 0, t, CellKind::Frame);
        y = 2;
    }
    let lw = slices.iter().map(|s| str_width(&s.0)).max().unwrap_or(0) as i64;
    for (name, value) in &slices {
        let share = value / total;
        cv.text(0, y, name, CellKind::Text);
        let cells = (share * PIE_BAR as f64).round() as usize;
        let cells = if *value > 0.0 { cells.max(1) } else { 0 };
        let bar = "█".repeat(cells.min(PIE_BAR));
        cv.text(lw + 2, y, &bar, CellKind::Line);
        let mut tail = format!("{:5.1}%", share * 100.0);
        if show_data {
            tail.push_str(&format!("  {}", format_number(*value)));
        }
        cv.text(lw + 2 + PIE_BAR as i64 + 1, y, &tail, CellKind::Label);
        y += 1;
    }
    Ok(cv)
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(d: &Diagram) -> String {
        d.rows
            .iter()
            .map(|r| r.iter().map(|(s, _)| s.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draws `src`, prints it (see `--nocapture`) and checks the row
    /// invariants every drawing keeps.
    fn draw(src: &str) -> String {
        let d = render(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
        check_rows(&d);
        let out = plain(&d);
        println!("{src}\n{out}\n");
        out
    }

    fn check_rows(d: &Diagram) {
        let mut widest = 0;
        for row in &d.rows {
            assert!(!matches!(row.last(), Some((_, CellKind::Blank))), "{row:?}");
            for pair in row.windows(2) {
                assert_ne!(pair[0].1, pair[1].1, "{row:?}");
            }
            assert!(row.iter().all(|(s, _)| !s.is_empty()));
            widest = widest.max(row.iter().map(|(s, _)| str_width(s)).sum::<usize>());
        }
        assert_eq!(d.width, widest);
    }

    fn assert_draws(src: &str, expected: &str) {
        assert_eq!(draw(src), expected.strip_prefix('\n').unwrap_or(expected));
    }

    #[test]
    fn two_nodes_top_down() {
        assert_draws(
            "graph TD\n    A[Plan] --> B[Build]",
            r"
┌──────┐
│ Plan │
└───┬──┘
    │
    ▼
┌───────┐
│ Build │
└───────┘",
        );
    }

    #[test]
    fn chain_left_to_right_with_a_label() {
        assert_draws(
            "graph LR\n    A[Write] -->|commit| B(Review) --> C([Merge])",
            r"
┌───────┐           ╭────────╮  ╭───────╮
│ Write ├──commit──▶│ Review ├─▶│ Merge │
└───────┘           ╰────────╯  ╰───────╯",
        );
    }

    #[test]
    fn fan_out_shares_one_track() {
        assert_draws(
            "graph TD\n    A[Root] --> B[Left]\n    A --> C[Right]",
            r"
      ┌──────┐
      │ Root │
      └───┬──┘
          │
    ┌─────┴────┐
    ▼          ▼
┌──────┐   ┌───────┐
│ Left │   │ Right │
└──────┘   └───────┘",
        );
    }

    #[test]
    fn decision_with_labels_and_a_loop_back() {
        assert_draws(
            "flowchart TD
    A[Start] --> B{OK?}
    B -->|yes| C[Ship]
    B -->|no| D[Fix]
    D --> B",
            r"
      ┌───────┐
      │ Start │
      └───┬───┘
          │
          ▼
      ╱───────╲
      │  OK?  │
      ╲───┬───╱
          │ ▲
          │ └────┐
    ┌─────┴────┐ │
    │ yes   no │ │
    ▼          ▼ │
┌──────┐   ┌─────┴─┐
│ Ship │   │  Fix  │
└──────┘   └───────┘",
        );
    }

    #[test]
    fn cycle_and_self_loop() {
        assert_draws(
            "graph TD\n  A[One] --> B[Two]\n  B --> C[Three]\n  C --> A\n  C --> C",
            r"
   ┌───────┐
   │  One  │
   └───┬───┘
       │ ▲
   ┌───┘ └┐
   ▼      │
┌─────┐   │
│ Two │   │
└──┬──┘   │
   │      │
   └──┐  ┌┘
      ▼  │
┌────────┴──┐
│  Three ↻  │
└───────────┘",
        );
    }

    #[test]
    fn bottom_to_top() {
        assert_draws(
            "graph BT\n    A[Plan] --> B[Build]",
            r"
┌───────┐
│ Build │
└───────┘
    ▲
    │
┌───┴──┐
│ Plan │
└──────┘",
        );
    }

    #[test]
    fn right_to_left() {
        assert_draws(
            "graph RL\n    A[Plan] -->|go| B[Build]",
            r"
┌───────┐       ┌──────┐
│ Build │◀──go──┤ Plan │
└───────┘       └──────┘",
        );
    }

    #[test]
    fn long_edge_passes_a_dummy() {
        assert_draws(
            "graph TD\n    A --> B --> C\n    A --> C",
            r"
   ┌───┐
   │ A │
   └─┬─┘
     │
  ┌──┴──┐
  ▼     │
┌───┐   │
│ B │   │
└─┬─┘   │
  │     │
  └──┬──┘
     ▼
   ┌───┐
   │ C │
   └───┘",
        );
    }

    #[test]
    fn link_styles_and_heads() {
        let out = draw("graph LR\n  A -.-> B ==> C --o D --x E <--> F --- G");
        for c in ['╌', '━', '○', '×', '◀', '├', '┤'] {
            assert!(out.contains(c), "{c} missing");
        }
        let out = draw("graph TD\n  A -- yes --> B\n  A -. maybe .-> C\n  A == no ==> D");
        for label in ["yes", "maybe", "no"] {
            assert!(out.contains(label), "{label} missing");
        }
    }

    #[test]
    fn shapes() {
        let out = draw(
            "graph LR\n  A[[sub]] --> B[(db)] --> C{{hex}} --> D>asym] --> E[/para/] --> F((c)) --> G([st]) --> H{d}",
        );
        for c in ['╔', '╭', '╱', '╲'] {
            assert!(out.contains(c), "{c} missing");
        }
        for t in ["sub", "db", "hex", "asym", "para", " c ", "st", " d "] {
            assert!(out.contains(t), "{t} missing");
        }
    }

    #[test]
    fn labels_wrap_and_break() {
        let out = draw(
            "graph TD\n  A[\"A label long enough that it has to wrap somewhere\"] --> B[one<br/>**two**]",
        );
        assert!(out.contains("│ A label long enough that it │"));
        assert!(out.contains("│ two │"));
    }

    #[test]
    fn wide_characters_keep_alignment() {
        let d = render("graph TD\n  A[日本語のラベル] --> B[emoji 🎉 ok]").unwrap();
        check_rows(&d);
        let out = plain(&d);
        let widths: Vec<usize> = out
            .lines()
            .filter(|l| l.contains("日本語") || l.contains("┌──") || l.contains("└──"))
            .map(str_width)
            .collect();
        assert!(widths.len() >= 3);
        let box_a: Vec<&str> = out.lines().take(3).collect();
        assert_eq!(str_width(box_a[0]), str_width(box_a[1]));
        assert_eq!(
            str_width(box_a[1].trim_end()),
            str_width(box_a[2].trim_end())
        );
    }

    #[test]
    fn statements_and_ignored_lines() {
        let out = draw(
            "%%{init: {}}%%\nflowchart TD;A-->B;\nsubgraph s1 [Group]\n  direction LR\n  B --> C\nend\nclassDef x fill:#f00\nclass A x\nstyle B fill:#0f0\nlinkStyle 0 stroke:red\nclick A \"http://x\"\nA:::x --> D\nC ~~~ E",
        );
        for t in ["A", "B", "C", "D", "E"] {
            assert!(out.contains(&format!("│ {t} │")), "{t} missing");
        }
    }

    #[test]
    fn state_diagram() {
        assert_draws(
            "stateDiagram-v2
    [*] --> Still
    Still --> Moving : push
    Moving --> Crash
    Crash --> [*]",
            r"
     ●
     │
     ▼
 ╭───────╮
 │ Still │
 ╰───┬───╯
     │
     │ push
     ▼
╭────────╮
│ Moving │
╰────┬───╯
     │
     ▼
 ╭───────╮
 │ Crash │
 ╰───┬───╯
     │
     ▼
     ◉",
        );
        let out = draw(
            "stateDiagram\n  state \"Waiting for input\" as W\n  W : blocks\n  [*] --> W\n  state Busy {\n    [*] --> Inner\n  }\n  note right of W : skipped\n  note left of W\n    multi\n  end note\n  W --> Busy",
        );
        assert!(out.contains("Waiting for input"));
        assert!(out.contains("blocks"));
        assert!(!out.contains("skipped") && !out.contains("multi"));
    }

    #[test]
    fn sequence_diagram() {
        assert_draws(
            "sequenceDiagram
    participant A as Alice
    participant B as Bob
    participant C as Carol
    A->>B: hello
    loop every minute
        B->>C: ping
        C-->>B: pong
    end
    Note right of C: idle
    B->>B: think
    B-xA: bye",
            r"
┌───────┐  ┌─────┐    ┌───────┐
│ Alice │  │ Bob │    │ Carol │
└───┬───┘  └──┬──┘    └───┬───┘
    │         │           │
    │  hello  │           │
    ├────────▶│           │
┄┄ loop every minute ┄┄┄┄┄│┄┄┄┄┄┄┄┄┄
    │         │   ping    │
    │         ├──────────▶│
    │         │   pong    │
    │         │◀╌╌╌╌╌╌╌╌╌╌┤
┄┄┄┄│┄┄┄┄┄┄┄┄┄│┄┄┄┄┄┄┄┄┄┄┄│┄┄┄┄┄┄┄┄┄
    │         │           │ ┌──────┐
    │         │           │ │ idle │
    │         │           │ └──────┘
    │         ├──┐ think  │
    │         │◀─┘        │
    │   bye   │           │
    │×────────┤           │
    │         │           │
┌───┴───┐  ┌──┴──┐    ┌───┴───┐
│ Alice │  │ Bob │    │ Carol │
└───────┘  └─────┘    └───────┘",
        );
    }

    #[test]
    fn sequence_arrows_actors_and_numbers() {
        let d = render(
            "sequenceDiagram\n  title Login\n  autonumber\n  actor U as User\n  U->>S: a\n  S-->>U: b\n  U->S: c\n  U-->S: d\n  U-)S: e\n  S--)U: f\n  U--xS: g\n  U<<->>S: h\n  Note over U,S: both<br>lines\n  alt ok\n    U->>S: i\n  else no\n    U->>S: j\n  end\n  box Aqua Group\n  participant S\n  end\n  rect rgb(0,0,0)\n  U->>S: k\n  end\n  activate S\n  deactivate S",
        )
        .unwrap();
        check_rows(&d);
        let out = plain(&d);
        println!("{out}");
        assert!(out.starts_with("Login"));
        assert!(out.contains("☺ User"));
        for t in [
            "1. a",
            "8. h",
            "11. k",
            "┄┄ alt ok",
            "┄┄ else no",
            "both",
            "lines",
        ] {
            assert!(out.contains(t), "{t} missing");
        }
        for c in ['⟩', '⟨', '×', '╌', '◀', '▶'] {
            assert!(out.contains(c), "{c} missing");
        }
        assert!(d.rows.iter().flatten().any(|(_, k)| *k == CellKind::Frame));
    }

    #[test]
    fn pie_chart() {
        assert_draws(
            "pie title Pets\n    \"Dogs\" : 386\n    \"Cats\" : 85\n    \"Rats\" : 15",
            r"
Pets

Dogs  ████████████████████████        79.4%
Cats  █████                           17.5%
Rats  █                                3.1%",
        );
        let out = draw("pie showData\n  title Split\n  \"a\" : 1.5\n  \"b\" : 1.5");
        assert!(out.contains("50.0%  1.5"));
        assert!(render("pie\n  \"a\" : -1").is_err());
        assert!(render("pie\n  title nothing").is_err());
    }

    #[test]
    fn kinds() {
        let d = render("graph TD\n  A -->|yes| B").unwrap();
        let kinds: Vec<CellKind> = d.rows.iter().flatten().map(|(_, k)| *k).collect();
        for k in [
            CellKind::Border,
            CellKind::Text,
            CellKind::Line,
            CellKind::Arrow,
            CellKind::Label,
            CellKind::Blank,
        ] {
            assert!(kinds.contains(&k), "{k:?} missing");
        }
        assert_eq!(d.kind, "graph");
    }

    #[test]
    fn unsupported_and_unreadable() {
        assert_eq!(
            render("classDiagram\n  A <|-- B"),
            Err("classDiagram: not drawn in the terminal".into())
        );
        for kind in [
            "erDiagram",
            "gantt",
            "journey",
            "gitGraph",
            "mindmap",
            "timeline",
        ] {
            assert_eq!(
                render(&format!("{kind}\n  x")),
                Err(format!("{kind}: not drawn in the terminal"))
            );
        }
        assert_eq!(render(""), Err("empty diagram".into()));
        assert_eq!(render("%% only a comment"), Err("empty diagram".into()));
        assert!(render("graph TD\n  A --> ").is_err());
        assert!(render("graph TD\n  [x]").is_err());
        assert!(render("sequenceDiagram\n  just words").is_err());
        assert!(render("stateDiagram\n  ??? !!!").is_err());
        assert!(render("---\ntitle: x\n---\ngraph TD\n  A --> B").is_ok());
    }

    #[test]
    fn never_panics() {
        let fixed = [
            "",
            "graph",
            "graph TD",
            "flowchart LR\n",
            "graph XX\n A",
            "graph TD\n A --> A",
            "graph TD\n A",
            "graph TD\n A -> B",
            "graph TD\n A ->> B",
            "graph TD\n A ==== B ---- C -.-.-> D",
            "graph TD\n A --> B --> C --> A --> C",
            "graph TD\n A[\"unterminated --> B",
            "graph TD\n A[ok] --> B(\"q\" junk)",
            "graph TD\n A{{",
            "graph TD\n A -->|never closed B",
            "graph TD\n A --- B --- A",
            "graph TD\n A & B & C --> D & E & F --> A",
            "graph TD\n A ~~~ B ~~~ A",
            "graph LR\n 🎉 --> 日本",
            "graph LR\n A[🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉] --> B",
            "graph TD\n A[a\u{301}e\u{200b}\tz] --> B",
            "graph TD\n A[<br><br><br>] --> B[``]",
            "graph BT\n A --> B\n B --> A\n A --> C\n C --> A",
            "graph RL\n A --> B --> C --> D --> B",
            "stateDiagram-v2",
            "stateDiagram-v2\n [*] --> [*]",
            "stateDiagram-v2\n state",
            "stateDiagram-v2\n state \"x",
            "stateDiagram-v2\n }\n }\n A --> B",
            "stateDiagram-v2\n A --> B : x : y",
            "sequenceDiagram",
            "sequenceDiagram\n A->>A: x",
            "sequenceDiagram\n end\n end\n A->>B: x",
            "sequenceDiagram\n Note over A: x",
            "sequenceDiagram\n Note left of A: x\n Note right of A: y",
            "sequenceDiagram\n ->>B: x",
            "sequenceDiagram\n A->>: x",
            "sequenceDiagram\n participant",
            "sequenceDiagram\n A->>B: 🎉 日本語 wide",
            "pie",
            "pie\n \"a\" : 1e400",
            "pie\n \"a\" : NaN",
            "pie\n : 1",
            "gantt",
            "---\n---",
            "---\nunterminated",
        ];
        let mut inputs: Vec<String> = fixed.iter().map(|s| s.to_string()).collect();
        let huge = "word ".repeat(400);
        inputs.push(format!("graph TD\n A[{huge}] --> B"));
        inputs.push(format!("sequenceDiagram\n A->>B: {huge}"));
        inputs.push(format!("pie\n \"{huge}\" : 1"));
        inputs.push(format!(
            "graph LR\n{}",
            (0..300)
                .map(|i| format!(" n{i} --> n{}\n", (i * 7) % 300))
                .collect::<String>()
        ));
        inputs.push(format!(
            "graph TD\n{}",
            (0..50)
                .map(|i| format!(" hub --> n{i}\n"))
                .collect::<String>()
        ));
        // Random statements from Mermaid-ish pieces, seeded.
        let pieces = [
            "A",
            "B",
            "c1",
            " ",
            "-->",
            "---",
            "-.->",
            "==>",
            "--o",
            "x--x",
            "<-->",
            "~~~",
            "|l|",
            "[t]",
            "(r)",
            "{d}",
            "((c))",
            "&",
            ";",
            "\n",
            "\"q\"",
            "-- t -->",
            "<br>",
            "日",
            "🎉",
            ":",
            "->>",
            "-->>",
            "Note over A",
            "loop",
            "end",
            "[*]",
            ":::k",
            "@{",
            "%%",
            "|",
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for i in 0..1500 {
            let header = [
                "graph TD\n",
                "graph LR\n",
                "graph BT\n",
                "stateDiagram\n",
                "sequenceDiagram\n",
            ][i % 5];
            let mut src = header.to_string();
            for _ in 0..(i % 23) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                src.push_str(pieces[(seed % pieces.len() as u64) as usize]);
            }
            inputs.push(src);
        }
        for src in &inputs {
            if let Ok(d) = render(src) {
                check_rows(&d);
            }
        }
    }

    #[test]
    fn deterministic() {
        let src = "graph TD\n A --> B & C & D\n B & C --> E\n D --> A";
        assert_eq!(render(src), render(src));
    }
}
