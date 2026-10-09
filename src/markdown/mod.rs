//! Markdown for the terminal: a CommonMark document (with GitHub tables,
//! task lists, strikethrough and YAML front matter) rendered into styled
//! lines at a given width, for the Markdown viewer (`app::markdown_view`,
//! `agent-mux md`).
//!
//! Prose wraps on spaces at the width; code blocks and Mermaid diagrams do
//! not wrap (the viewer scrolls sideways instead). A ```` ```mermaid ````
//! block is drawn as a diagram by `mermaid`; one it cannot draw shows its
//! source with the reason. Besides the lines, a render reports where each
//! link and heading landed, and which rendered line each source block
//! starts on, so the viewer can open links under the pointer, jump between
//! headings and keep a side-by-side preview level with the editor.

pub mod cli;
pub mod mermaid;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A link's place on a rendered line, in display columns (`end` exclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpot {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub url: String,
}

/// A heading's rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingSpot {
    pub line: usize,
    pub level: u8,
    pub text: String,
    /// The GitHub anchor, for `#fragment` links.
    pub slug: String,
}

/// Rows of a framed block (a diagram or code) wider than the window: the
/// viewer slides `start..end` sideways past their first `prefix` columns
/// (the containers' indentation and the frame's `│ `), and leaves every
/// other line where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wide {
    pub start: usize,
    pub end: usize,
    pub prefix: usize,
    /// The widest row, in display columns, prefix included.
    pub width: usize,
}

/// A rendered document.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkSpot>,
    pub headings: Vec<HeadingSpot>,
    /// `(source line, rendered line)` where each block starts; both ascend.
    pub blocks: Vec<(usize, usize)>,
    /// The widest line, in display columns.
    pub width: usize,
    /// The blocks that scroll sideways, in order.
    pub wide: Vec<Wide>,
}

impl Doc {
    /// The rendered line of the block that holds source line `src`.
    pub fn line_for_source(&self, src: usize) -> usize {
        let i = self.blocks.partition_point(|(s, _)| *s <= src);
        i.checked_sub(1).map(|i| self.blocks[i].1).unwrap_or(0)
    }

    /// The source line of the block shown at rendered line `line`.
    pub fn source_for_line(&self, line: usize) -> usize {
        let i = self.blocks.partition_point(|(_, l)| *l <= line);
        i.checked_sub(1).map(|i| self.blocks[i].0).unwrap_or(0)
    }

    /// The link at a rendered line and display column.
    pub fn link_at(&self, line: usize, col: usize) -> Option<&str> {
        self.links
            .iter()
            .find(|l| l.line == line && l.start <= col && col < l.end)
            .map(|l| l.url.as_str())
    }

    /// The sideways-scrolling block line `line` is in.
    pub fn wide_at(&self, line: usize) -> Option<&Wide> {
        self.wide.iter().find(|w| w.start <= line && line < w.end)
    }

    /// How far the wide blocks can slide in a view `cols` columns wide.
    pub fn max_hscroll(&self, cols: usize) -> usize {
        self.wide
            .iter()
            .map(|w| w.width.saturating_sub(cols))
            .max()
            .unwrap_or(0)
    }

    /// The document column under view column `col` of line `line`, with
    /// the wide blocks slid `hscroll` columns.
    pub fn column(&self, line: usize, col: usize, hscroll: usize) -> usize {
        match self.wide_at(line) {
            Some(w) if col >= w.prefix => col + hscroll,
            _ => col,
        }
    }

    /// The heading an anchor names.
    pub fn heading_by_slug(&self, slug: &str) -> Option<&HeadingSpot> {
        let slug = slug.to_lowercase();
        self.headings.iter().find(|h| h.slug == slug)
    }

    /// The lines as plain text, trailing spaces trimmed.
    pub fn plain(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
}

/// The GitHub anchor of a heading: lower case, punctuation dropped,
/// spaces as dashes.
pub fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Whether a path names a Markdown file, by its extension.
pub fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd"
        )
    })
}

/// Renders `text` for a viewport `width` columns wide.
pub fn render(text: &str, width: usize) -> Doc {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    let mut r = Renderer::new(text, width.max(20));
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        r.event(event, range.start);
    }
    r.flush_inline(None);
    r.doc
}

// ---- styles -------------------------------------------------------------

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The style of a heading of `level`, shared with the editor's colouring.
pub fn heading_style(level: u8) -> Style {
    let s = Style::default().add_modifier(Modifier::BOLD);
    match level {
        1 => s.fg(Color::Magenta),
        2 => s.fg(Color::Cyan),
        3 => s.fg(Color::Green),
        _ => s,
    }
}

fn link_style() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::UNDERLINED)
}

fn code_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn diagram_style(kind: mermaid::CellKind) -> Style {
    use mermaid::CellKind::*;
    match kind {
        Blank | Text => Style::default(),
        Border => Style::default().fg(Color::Cyan),
        Line => Style::default().fg(Color::Gray),
        Arrow => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Label => Style::default().fg(Color::Yellow),
        Frame => Style::default().fg(Color::Magenta),
    }
}

// ---- rendering ----------------------------------------------------------

/// A run of inline text with one style, maybe inside a link.
#[derive(Debug, Clone)]
struct Seg {
    text: String,
    style: Style,
    link: Option<usize>,
}

impl Seg {
    fn new(text: impl Into<String>, style: Style) -> Seg {
        Seg {
            text: text.into(),
            style,
            link: None,
        }
    }
}

enum Container {
    Quote,
    Item {
        marker: String,
        width: usize,
        /// The marker is not printed yet: the item's first line takes it.
        pending: bool,
    },
}

struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Seg>>>,
}

struct Renderer {
    width: usize,
    line_starts: Vec<usize>,
    doc: Doc,
    urls: Vec<String>,
    containers: Vec<Container>,
    lists: Vec<Option<u64>>,
    inline: Vec<Seg>,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<usize>,
    heading: Option<u8>,
    code: Option<(String, String)>,
    table: Option<Table>,
    image: Option<(usize, String)>,
    meta: Option<String>,
    need_gap: bool,
}

impl Renderer {
    fn new(text: &str, width: usize) -> Renderer {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Renderer {
            width,
            line_starts,
            doc: Doc::default(),
            urls: Vec::new(),
            containers: Vec::new(),
            lists: Vec::new(),
            inline: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            links: Vec::new(),
            heading: None,
            code: None,
            table: None,
            image: None,
            meta: None,
            need_gap: false,
        }
    }

    fn source_line(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }

    fn style(&self) -> Style {
        let mut s = Style::default();
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if !self.links.is_empty() {
            s = s.patch(link_style());
        }
        s
    }

    fn push(&mut self, mut seg: Seg) {
        if seg.link.is_none() {
            seg.link = self.links.last().copied();
        }
        if let Some(cell) = self
            .table
            .as_mut()
            .and_then(|t| t.rows.last_mut())
            .and_then(|r| r.last_mut())
        {
            cell.push(seg);
        } else {
            self.inline.push(seg);
        }
    }

    fn mark_block(&mut self, offset: usize) {
        let entry = (self.source_line(offset), self.doc.lines.len());
        match self.doc.blocks.last_mut() {
            Some(last) if last.1 == entry.1 => *last = entry,
            Some(last) if last.0 >= entry.0 => {}
            _ => self.doc.blocks.push(entry),
        }
    }

    /// A block begins: what was pending is written, then the gap.
    fn begin_block(&mut self, offset: usize) {
        self.flush_inline(None);
        if self.need_gap && !self.doc.lines.is_empty() {
            self.emit(Vec::new(), true);
        }
        self.need_gap = false;
        self.mark_block(offset);
    }

    fn prefix_width(&self) -> usize {
        self.containers
            .iter()
            .map(|c| match c {
                Container::Quote => 2,
                Container::Item { width, .. } => *width,
            })
            .sum()
    }

    fn avail(&self) -> usize {
        self.width.saturating_sub(self.prefix_width()).max(10)
    }

    /// Writes one line under the current containers. A `blank` line leaves
    /// pending list markers for the line that has content.
    fn emit(&mut self, content: Vec<Seg>, blank: bool) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut col = 0;
        for c in self.containers.iter_mut() {
            match c {
                Container::Quote => {
                    spans.push(Span::styled("│ ", Style::default().fg(Color::Green)))
                }
                Container::Item {
                    marker,
                    width,
                    pending,
                } => {
                    if *pending && !blank {
                        spans.push(Span::styled(
                            marker.clone(),
                            Style::default().fg(Color::Cyan),
                        ));
                        *pending = false;
                    } else {
                        spans.push(Span::raw(" ".repeat(*width)));
                    }
                }
            }
            col += spans.last().map(|s| s.width()).unwrap_or(0);
        }
        let line = self.doc.lines.len();
        for seg in content {
            let w = seg.text.width();
            if let Some(i) = seg.link {
                let url = &self.urls[i];
                match self.doc.links.last_mut() {
                    Some(l) if l.line == line && l.end == col && &l.url == url => l.end += w,
                    _ => self.doc.links.push(LinkSpot {
                        line,
                        start: col,
                        end: col + w,
                        url: url.clone(),
                    }),
                }
            }
            col += w;
            match spans.last_mut() {
                Some(last) if last.style == seg.style => last.content.to_mut().push_str(&seg.text),
                _ => spans.push(Span::styled(seg.text, seg.style)),
            }
        }
        if blank {
            // trailing indentation on an empty line is noise
            while spans.last().is_some_and(|s| s.content.trim().is_empty()) {
                spans.pop();
            }
            col = spans.iter().map(|s| s.width()).sum();
        }
        self.doc.width = self.doc.width.max(col);
        self.doc.lines.push(Line::from(spans));
    }

    /// Wraps and writes the pending inline text.
    fn flush_inline(&mut self, style: Option<Style>) {
        if self.inline.is_empty() {
            return;
        }
        let mut segs = std::mem::take(&mut self.inline);
        if let Some(st) = style {
            for s in &mut segs {
                s.style = st.patch(s.style);
            }
        }
        for line in wrap(&segs, self.avail()) {
            self.emit(line, false);
        }
    }

    fn event(&mut self, event: Event, offset: usize) {
        if let Some((_, body)) = self.code.as_mut() {
            match event {
                Event::Text(t) => body.push_str(&t),
                Event::End(TagEnd::CodeBlock) => self.end_code(),
                _ => {}
            }
            return;
        }
        if let Some(meta) = self.meta.as_mut() {
            match event {
                Event::Text(t) => meta.push_str(&t),
                Event::End(TagEnd::MetadataBlock(_)) => self.end_meta(),
                _ => {}
            }
            return;
        }
        if let Some((_, alt)) = self.image.as_mut() {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (url, alt) = self.image.take().unwrap_or_default();
                    let text = if alt.trim().is_empty() {
                        "[image]".to_string()
                    } else {
                        format!("[image: {}]", alt.trim())
                    };
                    self.push(Seg {
                        text,
                        style: Style::default().fg(Color::Magenta),
                        link: Some(url),
                    });
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag, offset),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                let st = self.style();
                self.push(Seg::new(t.to_string(), st));
            }
            Event::Code(t) => {
                let st = self.style().patch(code_style());
                self.push(Seg::new(t.to_string(), st));
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                let st = self.style().patch(code_style());
                self.push(Seg::new(t.to_string(), st));
            }
            Event::SoftBreak => self.push(Seg::new(" ", Style::default())),
            Event::HardBreak => self.push(Seg::new("\n", Style::default())),
            Event::InlineHtml(h) => {
                let tag = h.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                    self.push(Seg::new("\n", Style::default()));
                } else {
                    self.push(Seg::new(h.to_string(), dim()));
                }
            }
            Event::Html(h) => {
                for line in h.lines() {
                    let line = line.trim_end().to_string();
                    self.emit(vec![Seg::new(line, dim())], false);
                }
            }
            Event::Rule => {
                self.begin_block(offset);
                let w = self.avail();
                self.emit(vec![Seg::new("─".repeat(w), dim())], false);
                self.need_gap = true;
            }
            Event::TaskListMarker(done) => {
                let seg = if done {
                    Seg::new("[x] ", Style::default().fg(Color::Green))
                } else {
                    Seg::new("[ ] ", dim())
                };
                self.push(seg);
            }
            Event::FootnoteReference(name) => {
                self.push(Seg::new(format!("[^{name}]"), dim()));
            }
        }
    }

    fn start(&mut self, tag: Tag, offset: usize) {
        match tag {
            Tag::Paragraph => self.begin_block(offset),
            Tag::Heading { level, .. } => {
                self.begin_block(offset);
                self.heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
            }
            Tag::BlockQuote(_) => {
                self.begin_block(offset);
                self.containers.push(Container::Quote);
            }
            Tag::CodeBlock(kind) => {
                self.begin_block(offset);
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => self.begin_block(offset),
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.begin_block(offset);
                } else {
                    self.flush_inline(None);
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_inline(None);
                self.mark_block(offset);
                let depth = self
                    .containers
                    .iter()
                    .filter(|c| matches!(c, Container::Item { .. }))
                    .count();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => format!("{} ", ["•", "◦", "▪", "‣"][depth % 4]),
                };
                self.containers.push(Container::Item {
                    width: marker.width(),
                    marker,
                    pending: true,
                });
            }
            Tag::Table(aligns) => {
                self.begin_block(offset);
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(Vec::new());
                }
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                self.urls.push(dest_url.to_string());
                self.links.push(self.urls.len() - 1);
            }
            Tag::Image { dest_url, .. } => {
                self.urls.push(dest_url.to_string());
                self.image = Some((self.urls.len() - 1, String::new()));
            }
            Tag::MetadataBlock(_) => {
                self.begin_block(offset);
                self.meta = Some(String::new());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline(None);
                self.need_gap = true;
            }
            TagEnd::Heading(_) => self.end_heading(),
            TagEnd::BlockQuote(_) => {
                self.flush_inline(None);
                self.containers.pop();
                self.need_gap = true;
            }
            TagEnd::HtmlBlock => self.need_gap = true,
            TagEnd::List(_) => {
                self.flush_inline(None);
                self.lists.pop();
                if self.lists.is_empty() {
                    self.need_gap = true;
                }
            }
            TagEnd::Item => {
                self.flush_inline(None);
                if matches!(
                    self.containers.last(),
                    Some(Container::Item { pending: true, .. })
                ) {
                    self.emit(Vec::new(), false);
                }
                self.containers.pop();
            }
            TagEnd::Table => self.end_table(),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            _ => {}
        }
    }

    fn end_heading(&mut self) {
        let level = self.heading.take().unwrap_or(1);
        let text: String = self.inline.iter().map(|s| s.text.as_str()).collect();
        let line = self.doc.lines.len();
        let style = heading_style(level);
        if level >= 4 {
            self.inline.insert(
                0,
                Seg::new(format!("{} ", "#".repeat(level as usize)), dim()),
            );
        }
        let width = self.inline.iter().map(|s| s.text.width()).sum::<usize>();
        self.flush_inline(Some(style));
        match level {
            1 => {
                let w = width.min(self.avail());
                self.emit(vec![Seg::new("━".repeat(w), style)], false);
            }
            2 => {
                let w = width.min(self.avail());
                self.emit(vec![Seg::new("─".repeat(w), dim())], false);
            }
            _ => {}
        }
        self.doc.headings.push(HeadingSpot {
            line,
            level,
            slug: slug(&text),
            text,
        });
        self.need_gap = true;
    }

    /// A framed block: a top rule with a label, `│ `-prefixed rows, a
    /// bottom rule. Rows do not wrap.
    fn framed(&mut self, label: &str, rows: Vec<Vec<Seg>>) {
        let w = self.avail();
        let widest = rows
            .iter()
            .map(|r| r.iter().map(|s| s.text.width()).sum::<usize>() + 2)
            .max()
            .unwrap_or(0);
        let hint = if widest > w { " · ←/→ scroll" } else { "" };
        let head = format!("╭─ {label}{hint} ");
        let fill = w.saturating_sub(head.width());
        self.emit(
            vec![Seg::new(format!("{head}{}", "─".repeat(fill)), dim())],
            false,
        );
        let start = self.doc.lines.len();
        for row in rows {
            let mut line = vec![Seg::new("│ ", dim())];
            line.extend(row);
            self.emit(line, false);
        }
        if widest > w {
            let prefix = self.prefix_width() + 2;
            self.doc.wide.push(Wide {
                start,
                end: self.doc.lines.len(),
                prefix,
                width: prefix - 2 + widest,
            });
        }
        self.emit(
            vec![Seg::new(
                format!("╰{}", "─".repeat(w.saturating_sub(1))),
                dim(),
            )],
            false,
        );
    }

    fn end_code(&mut self) {
        let Some((lang, body)) = self.code.take() else {
            return;
        };
        let body = body
            .strip_suffix('\n')
            .unwrap_or(&body)
            .replace('\t', "    ");
        if lang.eq_ignore_ascii_case("mermaid") {
            match mermaid::render(&body) {
                Ok(d) => {
                    let label = format!("mermaid · {}", d.kind);
                    let rows = d
                        .rows
                        .into_iter()
                        .map(|row| {
                            row.into_iter()
                                .map(|(t, k)| Seg::new(t, diagram_style(k)))
                                .collect()
                        })
                        .collect();
                    self.framed(&label, rows);
                }
                Err(reason) => {
                    let rows = body
                        .lines()
                        .map(|l| vec![Seg::new(l, code_style())])
                        .collect();
                    self.framed(&format!("mermaid · {reason}"), rows);
                }
            }
        } else {
            let label = if lang.is_empty() {
                "code"
            } else {
                lang.as_str()
            };
            let rows = body
                .lines()
                .map(|l| vec![Seg::new(l, code_style())])
                .collect();
            self.framed(label, rows);
        }
        self.need_gap = true;
    }

    fn end_meta(&mut self) {
        let Some(meta) = self.meta.take() else {
            return;
        };
        let rows = meta
            .trim_end()
            .lines()
            .map(|l| match l.split_once(':') {
                Some((k, v)) if !k.starts_with(' ') && !k.contains(' ') => vec![
                    Seg::new(format!("{k}:"), Style::default().fg(Color::Cyan)),
                    Seg::new(v.to_string(), Style::default()),
                ],
                _ => vec![Seg::new(l, Style::default())],
            })
            .collect();
        self.framed("front matter", rows);
        self.need_gap = true;
    }

    fn end_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let ncols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let mut widths = vec![3usize; ncols];
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                let w = cell.iter().map(|s| s.text.width()).sum::<usize>();
                widths[i] = widths[i].max(w);
            }
        }
        let avail = self.avail();
        loop {
            let total = widths.iter().sum::<usize>() + 3 * ncols + 1;
            if total <= avail {
                break;
            }
            let (i, w) = widths
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|(_, w)| *w)
                .unwrap_or((0, 0));
            if w <= 3 {
                break;
            }
            widths[i] -= 1;
        }
        let border = |l: &str, m: &str, r: &str| {
            let mut s = l.to_string();
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if i + 1 == ncols { r } else { m });
            }
            vec![Seg::new(s, dim())]
        };
        self.emit(border("┌", "┬", "┐"), false);
        for (r, row) in table.rows.iter().enumerate() {
            let bold = Style::default().add_modifier(Modifier::BOLD);
            let cells: Vec<Vec<Vec<Seg>>> = (0..ncols)
                .map(|i| {
                    let mut cell = row.get(i).cloned().unwrap_or_default();
                    if r == 0 {
                        for s in &mut cell {
                            s.style = bold.patch(s.style);
                        }
                    }
                    wrap(&cell, widths[i])
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for h in 0..height {
                let mut line = vec![Seg::new("│", dim())];
                for (i, cell) in cells.iter().enumerate() {
                    let content = cell.get(h).cloned().unwrap_or_default();
                    let w = content.iter().map(|s| s.text.width()).sum::<usize>();
                    let pad = widths[i].saturating_sub(w);
                    let (left, right) = match table.aligns.get(i) {
                        Some(Alignment::Right) => (pad, 0),
                        Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                        _ => (0, pad),
                    };
                    line.push(Seg::new(" ".repeat(left + 1), Style::default()));
                    line.extend(content);
                    line.push(Seg::new(" ".repeat(right + 1), Style::default()));
                    line.push(Seg::new("│", dim()));
                }
                self.emit(line, false);
            }
            if r == 0 && table.rows.len() > 1 {
                self.emit(border("├", "┼", "┤"), false);
            }
        }
        self.emit(border("└", "┴", "┘"), false);
        self.need_gap = true;
    }
}

/// A piece of inline text, for wrapping.
enum Atom {
    /// One space, with the style and link of the whitespace it stands for.
    Space(Seg),
    Break,
    /// Adjacent non-space text, possibly across styles.
    Word(Vec<Seg>),
}

fn atoms(segs: &[Seg]) -> Vec<Atom> {
    let mut out = Vec::new();
    let mut word: Vec<Seg> = Vec::new();
    let flush = |word: &mut Vec<Seg>, out: &mut Vec<Atom>| {
        if !word.is_empty() {
            out.push(Atom::Word(std::mem::take(word)));
        }
    };
    for seg in segs {
        let mut cur = String::new();
        for ch in seg.text.chars() {
            if ch == '\n' || ch.is_whitespace() {
                if !cur.is_empty() {
                    word.push(Seg {
                        text: std::mem::take(&mut cur),
                        ..seg.clone()
                    });
                }
                flush(&mut word, &mut out);
                if ch == '\n' {
                    out.push(Atom::Break);
                } else if !matches!(out.last(), Some(Atom::Space(_))) {
                    out.push(Atom::Space(Seg {
                        text: " ".into(),
                        ..seg.clone()
                    }));
                }
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            word.push(Seg {
                text: cur,
                ..seg.clone()
            });
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Greedy word wrap of styled segments to `width` columns. A word wider
/// than the line breaks between characters.
fn wrap(segs: &[Seg], width: usize) -> Vec<Vec<Seg>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Seg>> = vec![Vec::new()];
    let mut col = 0usize;
    let mut space: Option<Seg> = None;
    for atom in atoms(segs) {
        match atom {
            Atom::Break => {
                lines.push(Vec::new());
                col = 0;
                space = None;
            }
            Atom::Space(sp) => {
                if col > 0 {
                    space = Some(sp);
                }
            }
            Atom::Word(parts) => {
                let w: usize = parts.iter().map(|s| s.text.width()).sum();
                let gap = usize::from(space.is_some());
                if col > 0 && col + gap + w > width {
                    lines.push(Vec::new());
                    col = 0;
                    space = None;
                }
                if let Some(sp) = space.take() {
                    lines.last_mut().unwrap().push(sp);
                    col += 1;
                }
                for part in parts {
                    if col + part.text.width() <= width {
                        col += part.text.width();
                        lines.last_mut().unwrap().push(part);
                        continue;
                    }
                    // hard break inside a word too long for any line
                    let mut cur = String::new();
                    for ch in part.text.chars() {
                        let cw = ch.width().unwrap_or(0);
                        if col + cw > width && col > 0 {
                            if !cur.is_empty() {
                                lines.last_mut().unwrap().push(Seg {
                                    text: std::mem::take(&mut cur),
                                    ..part.clone()
                                });
                            }
                            lines.push(Vec::new());
                            col = 0;
                        }
                        cur.push(ch);
                        col += cw;
                    }
                    if !cur.is_empty() {
                        lines.last_mut().unwrap().push(Seg { text: cur, ..part });
                    }
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(md: &str, width: usize) -> Vec<String> {
        render(md, width).plain()
    }

    #[test]
    fn a_block_wider_than_the_width_is_marked_to_slide() {
        let md = format!(
            "- item\n\n  ```\n  {}\n  ok\n  ```\n\n```\nfits\n```\n",
            "y".repeat(50)
        );
        let doc = render(&md, 30);
        assert_eq!(doc.wide.len(), 1, "only the wide block: {:?}", doc.wide);
        let w = &doc.wide[0];
        assert_eq!(w.end - w.start, 2, "its two rows, not its rules");
        assert_eq!(w.prefix, 4, "the item's indent and the frame's │");
        assert_eq!(w.width, 4 + 50);
        assert_eq!(doc.max_hscroll(30), 54 - 30);
        assert_eq!(doc.column(w.start, 10, 5), 15);
        assert_eq!(doc.column(w.start, 2, 5), 2, "the gutter does not move");
        assert_eq!(doc.column(0, 10, 5), 10, "nor does prose");
    }

    #[test]
    fn paragraphs_wrap_on_spaces_and_keep_a_gap() {
        let out = plain("one two three four five six\n\nnext", 20);
        assert_eq!(out, vec!["one two three four", "five six", "", "next"]);
    }

    #[test]
    fn a_word_wider_than_the_line_breaks_between_characters() {
        let out = plain("abcdefghijklmnopqrstuvwxyz0123", 20);
        assert_eq!(out, vec!["abcdefghijklmnopqrst", "uvwxyz0123"]);
    }

    #[test]
    fn styled_runs_inside_one_word_wrap_together() {
        let out = plain("aaaaaaaaaaaaaaa **bold**ness", 20);
        assert_eq!(out, vec!["aaaaaaaaaaaaaaa", "boldness"]);
    }

    #[test]
    fn headings_are_styled_and_recorded_with_their_anchor() {
        let doc = render("# Title\n\ntext\n\n## Getting started!\n\n#### Deep", 40);
        assert_eq!(
            doc.plain(),
            vec![
                "Title",
                "━━━━━",
                "",
                "text",
                "",
                "Getting started!",
                "────────────────",
                "",
                "#### Deep"
            ]
        );
        assert_eq!(doc.headings.len(), 3);
        assert_eq!(doc.headings[1].line, 5);
        assert_eq!(doc.headings[1].slug, "getting-started");
        assert_eq!(doc.heading_by_slug("Getting-Started").unwrap().level, 2);
        let style = doc.lines[0].spans[0].style;
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn lists_nest_with_markers_and_hanging_indents() {
        let md = "- first item that wraps around\n- second\n  - nested\n\n1. one\n2. two\n";
        assert_eq!(
            plain(md, 20),
            vec![
                "• first item that",
                "  wraps around",
                "• second",
                "  ◦ nested",
                "",
                "1. one",
                "2. two"
            ]
        );
    }

    #[test]
    fn loose_list_items_keep_their_gap_and_tasks_show_boxes() {
        let md = "- a\n\n- b\n\n---\n\n- [x] done\n- [ ] todo\n";
        let out = plain(md, 30);
        assert_eq!(out[..3], ["• a", "", "• b"]);
        assert!(out.contains(&"• [x] done".to_string()), "{out:?}");
        assert!(out.contains(&"• [ ] todo".to_string()), "{out:?}");
    }

    #[test]
    fn quotes_carry_a_bar_on_every_line() {
        let out = plain("> quoted text that wraps here\n>\n> again", 20);
        assert_eq!(
            out,
            vec!["│ quoted text that", "│ wraps here", "│", "│ again"]
        );
    }

    #[test]
    fn code_blocks_are_framed_and_do_not_wrap() {
        let doc = render("```rust\nfn main() { println!(\"a long line\"); }\n```", 24);
        let out = doc.plain();
        assert_eq!(out[0], "╭─ rust · ←/→ scroll ───");
        assert_eq!(out[1], "│ fn main() { println!(\"a long line\"); }");
        assert_eq!(out[2], format!("╰{}", "─".repeat(23)));
        assert!(doc.width > 24, "the viewer scrolls sideways");
    }

    #[test]
    fn a_mermaid_block_is_a_diagram_or_its_source_with_the_reason() {
        let out = plain("```mermaid\ngantt\n  title x\n```", 40);
        assert!(out[0].starts_with("╭─ mermaid · gantt"), "{out:?}");
        assert!(out.iter().any(|l| l.contains("title x")));
    }

    #[test]
    fn tables_are_boxed_aligned_and_fit_the_width() {
        let md = "| Name | Qty |\n|:-----|----:|\n| apple | 3 |\n| kiwi | 12 |\n";
        assert_eq!(
            plain(md, 40),
            vec![
                "┌───────┬─────┐",
                "│ Name  │ Qty │",
                "├───────┼─────┤",
                "│ apple │   3 │",
                "│ kiwi  │  12 │",
                "└───────┴─────┘"
            ]
        );
        let wide = "| a | b |\n|---|---|\n| one two three four five six | x |\n";
        for line in plain(wide, 20) {
            assert!(line.width() <= 20, "{line:?}");
        }
    }

    #[test]
    fn links_are_recorded_where_they_land() {
        let doc = render("> see [the docs](docs/a.md) and ![logo](x.png)", 60);
        let first = &doc.links[0];
        assert_eq!((first.line, first.start, first.end), (0, 6, 14));
        assert_eq!(doc.link_at(0, 7), Some("docs/a.md"));
        assert_eq!(doc.link_at(0, 2), None);
        assert_eq!(doc.links[1].url, "x.png");
        assert!(doc.plain()[0].contains("[image: logo]"));
    }

    #[test]
    fn a_link_wrapped_over_two_lines_is_clickable_on_both() {
        let doc = render("aaaa bbbb cccc [one two three](u) b", 20);
        let spots: Vec<(usize, usize, usize)> =
            doc.links.iter().map(|l| (l.line, l.start, l.end)).collect();
        assert_eq!(spots, vec![(0, 15, 18), (1, 0, 9)]);
        assert!(doc.links.iter().all(|l| l.url == "u"));
    }

    #[test]
    fn front_matter_is_framed() {
        let out = plain("---\nname: heimdall\ndescription: brief\n---\n\n# H", 40);
        assert!(out[0].starts_with("╭─ front matter"));
        assert_eq!(out[1], "│ name: heimdall");
        assert_eq!(out[3].chars().next(), Some('╰'));
        assert_eq!(out[5], "H");
    }

    #[test]
    fn blocks_map_source_lines_to_rendered_lines() {
        let md = "# A\n\npara one\nstill one\n\n- x\n- y\n";
        let doc = render(md, 40);
        // "A", rule, gap, "para one still one", gap, "• x", "• y"
        assert_eq!(doc.line_for_source(0), 0);
        assert_eq!(doc.line_for_source(3), 3);
        assert_eq!(doc.line_for_source(6), 6);
        assert_eq!(doc.source_for_line(5), 5);
        assert_eq!(doc.source_for_line(1), 0);
    }

    #[test]
    fn hard_breaks_and_br_start_new_lines() {
        assert_eq!(plain("a  \nb<br>c", 40), vec!["a", "b", "c"]);
    }

    #[test]
    fn nothing_panics_on_odd_input() {
        for md in [
            "",
            "\n\n\n",
            "#",
            "- ",
            "1.",
            "> ",
            "|a|\n|-|",
            "```",
            "```mermaid",
            "[x]()",
            "![]()",
            "***",
            "<div>\n</div>",
            "| a | b |\n|---|---|\n| 1 |",
            "- [ ]",
            "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀",
            "> - > - > deep\n>   1. x",
        ] {
            for w in [0, 1, 20, 80] {
                let _ = render(md, w);
            }
        }
    }

    #[test]
    fn markdown_paths_are_known_by_extension() {
        use std::path::Path;
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("a/b.MARKDOWN")));
        assert!(!is_markdown_path(Path::new("a.rs")));
        assert!(!is_markdown_path(Path::new("md")));
    }
}
