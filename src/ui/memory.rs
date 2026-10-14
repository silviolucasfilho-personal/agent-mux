//! Drawing the Memory screen (`crate::app::memory_view`): ai-memory's
//! setup, one row per part, then the steps waiting for `y` or the log of
//! the ones running, and the keys.

use super::flow::{dim, head, hints};
use crate::app::memory_view::{InputKind, MemoryView, Pending};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// The state's rows with their marks coloured.
pub fn state_lines(v: &MemoryView) -> Vec<Line<'static>> {
    crate::memory::cli::status_lines(&v.state)
        .into_iter()
        .map(|l| {
            let (mark, rest) = l.split_at(l.char_indices().nth(1).map(|(i, _)| i).unwrap_or(0));
            let style = match mark {
                "✓" => Style::default().fg(Color::Green),
                "✗" => Style::default().fg(Color::Red),
                "–" => dim(),
                _ => return Line::styled(format!("  {l}"), dim()),
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(mark.to_string(), style),
                Span::raw(rest.to_string()),
            ])
        })
        .collect()
}

pub fn draw(f: &mut Frame, area: Rect, v: &MemoryView) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(" Memory · ai-memory ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [body, foot] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    let mut lines = vec![
        Line::styled(
            "  One memory per project for Claude Code, Codex and Antigravity, kept by ai-memory",
            dim(),
        ),
        Line::styled(
            "  (github.com/akitaonrails/ai-memory): its hooks capture each session, its MCP server answers.",
            dim(),
        ),
        Line::raw(""),
    ];
    lines.extend(state_lines(v));
    lines.push(Line::styled(
        format!("  {}", crate::memory::cli::verdict(&v.state, "i")),
        if v.state.ready() {
            Style::default().fg(Color::Green)
        } else {
            dim()
        },
    ));
    lines.push(Line::from(vec![
        Span::styled("  project     ", dim()),
        Span::raw(crate::memory::install::shown(&v.dir)),
    ]));
    lines.push(Line::raw(""));
    if let Some(input) = &v.input {
        let label = match input.kind {
            InputKind::Search => "  search: ".to_string(),
            InputKind::Ask(h) => format!("  ask {}: ", crate::memory::label(h)),
        };
        lines.push(Line::from(vec![
            Span::styled(label, head()),
            Span::raw(input.text.clone()),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::raw(""));
    }
    if let Some(e) = &v.error {
        lines.push(Line::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        ));
        lines.push(Line::raw(""));
    }
    if let Some((what, steps)) = &v.pending {
        lines.push(Line::styled(
            match what {
                Pending::Install => "  agent-mux will:".to_string(),
                Pending::Uninstall { purge: true } => {
                    "  agent-mux will (and the memory is deleted):".to_string()
                }
                Pending::Uninstall { purge: false } => {
                    "  agent-mux will (the memory stays):".to_string()
                }
            },
            head(),
        ));
        // the commands too when they fit, else the titles alone
        let room = (body.height as usize).saturating_sub(lines.len());
        let commands = steps.len() * 2 <= room;
        for (i, s) in steps.iter().enumerate() {
            lines.push(Line::raw(format!("  {}. {}", i + 1, s.title)));
            if commands && !s.command.is_empty() {
                lines.push(Line::styled(format!("     {}", s.command), dim()));
            }
        }
    } else if let Some((query, hits)) = &v.hits {
        lines.push(Line::styled(
            format!("  {} page(s) for {query:?}", hits.len()),
            head(),
        ));
        let width = (body.width as usize).saturating_sub(6);
        let cut = |t: String| -> String {
            if t.chars().count() > width {
                t.chars()
                    .take(width.saturating_sub(1))
                    .chain(['…'])
                    .collect()
            } else {
                t
            }
        };
        // two lines a result; keep the selected one in sight
        let room = ((body.height as usize).saturating_sub(lines.len()) / 2).max(1);
        let first = v.selected.saturating_sub(room - 1);
        for (i, h) in hits.iter().enumerate().skip(first).take(room) {
            let sel = i == v.selected;
            let title = h.title.clone().unwrap_or_else(|| h.path.clone());
            lines.push(Line::from(vec![
                Span::styled(
                    if sel { "  ▸ " } else { "    " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    cut(format!("{title}  {}", h.path)),
                    if sel {
                        Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    },
                ),
            ]));
            lines.push(Line::styled(
                format!("      {}", cut(h.snippet_line())),
                dim(),
            ));
        }
    } else if !v.log.is_empty() {
        let room = (body.height as usize).saturating_sub(lines.len());
        let end = v.log.len().saturating_sub(v.scroll);
        let start = end.saturating_sub(room);
        let width = (body.width as usize).saturating_sub(3);
        for l in &v.log[start..end] {
            let l: String = if l.chars().count() > width {
                l.chars()
                    .take(width.saturating_sub(1))
                    .chain(['…'])
                    .collect()
            } else {
                l.clone()
            };
            let style = if l.starts_with('[') {
                head()
            } else if l.starts_with("stopped") {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            lines.push(Line::styled(format!("  {l}"), style));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    let keys: Vec<(&str, &str)> = match &v.pending {
        _ if matches!(v.input.as_ref().map(|i| &i.kind), Some(InputKind::Ask(_))) => vec![
            ("Enter", "ask in a new session"),
            ("Tab", "other harness"),
            ("Esc", "cancel"),
        ],
        _ if v.input.is_some() => vec![("Enter", "search"), ("Esc", "cancel")],
        None if v.hits.is_some() => vec![
            ("↑/↓", "select"),
            ("Enter", "open page"),
            ("/", "search again"),
            ("a", "ask"),
            ("Esc", "back"),
        ],
        Some((Pending::Uninstall { purge }, _)) => vec![
            ("y", "run"),
            (
                "p",
                if *purge {
                    "keep the memory"
                } else {
                    "also delete the memory"
                },
            ),
            ("n", "cancel"),
        ],
        Some(_) => vec![("y", "run"), ("n", "cancel")],
        None if v.running() => vec![("…", "running"), ("↑/↓", "log")],
        None => vec![
            ("/", "search"),
            ("a", "ask"),
            ("w", "web view"),
            ("i", "install or repair"),
            ("u", "uninstall"),
            ("Ctrl+R", "refresh"),
            ("Esc", "close"),
        ],
    };
    f.render_widget(Paragraph::new(hints(&keys)), foot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use crate::memory::{State, Wiring};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(v: &MemoryView) -> String {
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        t.draw(|f| draw(f, f.area(), v)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..30)
            .map(|y| (0..110).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_screen_shows_each_part_and_the_steps_before_they_run() {
        let state = State {
            binary: None,
            initialized: false,
            server: false,
            launchd: false,
            feed: true,
            harnesses: vec![Wiring {
                harness: Harness::Codex,
                cli: true,
                mcp: false,
                hooks: false,
            }],
        };
        let mut v = MemoryView::with_state(state);
        let s = screen(&v);
        assert!(s.contains("Memory · ai-memory"));
        assert!(s.contains("✗ ai-memory   not installed"));
        assert!(s.contains("✗ Codex       MCP ✗ · hooks ✗"));
        assert!(s.contains("i install or repair"));
        assert!(s.contains("/ search"));
        v.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let s = screen(&v);
        if cfg!(target_os = "macos") {
            assert!(s.contains("agent-mux will:"), "{s}");
            assert!(s.contains("1. Download ai-memory"));
            assert!(s.contains("install-hooks --agent codex --apply"));
            assert!(s.contains("y run"));
        }
    }
}
