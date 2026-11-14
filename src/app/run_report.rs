//! The Run screen's Result tab as lines (`app::run_screen`): the headline
//! every surface leads with (verdict, counts, cost) and the blocks of a
//! run's report (`workflows::report`), then the answer itself.

use crate::workflows::report::{self, Block, Report, Status};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub(crate) fn value_lines(text: &str) -> Vec<Line<'static>> {
    text.lines().map(|l| Line::raw(format!("  {l}"))).collect()
}

pub(crate) fn rule() -> Line<'static> {
    Line::styled(
        "  ────────────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    )
}

fn status_style(s: Status) -> Style {
    Style::default().fg(match s {
        Status::Ok => Color::Green,
        Status::Running => Color::Cyan,
        Status::Attention => Color::Yellow,
        Status::Failed => Color::Red,
    })
}

fn badge_style(badge: &str) -> Style {
    match badge.to_ascii_lowercase().as_str() {
        "high" | "critical" | "blocker" | "bug" => Style::default().fg(Color::Red),
        "medium" | "low" | "question" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::Cyan),
    }
}

/// The three lines every surface leads with: verdict, counts, cost.
pub(crate) fn headline_lines(rep: &Report) -> Vec<Line<'static>> {
    let h = &rep.headline;
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(vec![
            Span::styled(format!("  {} ", h.status.glyph()), status_style(h.status)),
            Span::styled(
                format!("{:<12} ", h.status_word),
                status_style(h.status).add_modifier(Modifier::BOLD),
            ),
            Span::raw(h.verdict.clone()),
        ]),
        Line::styled(format!("  {}", h.one_line()), dim),
    ]
}

/// The report's blocks as rows of the detail pane.
pub(crate) fn report_lines(rep: &Report) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let mut out: Vec<Line<'static>> = Vec::new();
    for block in &rep.blocks {
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        match block {
            Block::Outline {
                title,
                bytes: _,
                headings,
                lead,
            } => {
                out.push(Line::styled(format!("  {}", cap_first(title)), key));
                if headings.is_empty() && !lead.is_empty() {
                    out.push(Line::raw(format!("    {lead}")));
                }
                // The first heading is the verdict the headline already
                // shows; repeating it under the title says nothing new.
                let skip = usize::from(
                    headings
                        .first()
                        .is_some_and(|h| h.text.trim() == rep.headline.verdict.trim()),
                );
                for h in headings.iter().skip(skip).take(24) {
                    let indent = "  ".repeat(h.level.saturating_sub(1));
                    out.push(Line::raw(format!("    {indent}{}", h.text)));
                }
            }
            Block::Table {
                title,
                columns: _,
                rows,
            } => {
                out.push(Line::styled(format!("  {title} ({})", rows.len()), key));
                for r in rows.iter().take(60) {
                    out.extend(row_lines(r, false));
                    if let Some(d) = &r.detail {
                        out.push(Line::styled(
                            format!("      {}", report::short(d, 100)),
                            dim,
                        ));
                    }
                }
            }
            Block::Dropped { title, rows } => {
                out.push(Line::styled(format!("  {title}"), key));
                for r in rows.iter().take(40) {
                    out.extend(row_lines(r, true));
                    for reason in r.reasons.iter().take(3) {
                        out.push(Line::styled(
                            format!("      refuted: {}", report::short(reason, 96)),
                            dim,
                        ));
                    }
                }
            }
            Block::Evidence {
                step,
                badge,
                fields,
            } => {
                let mut head = vec![Span::styled(format!("  {} ", cap_first(step)), key)];
                if let Some(b) = badge {
                    head.push(Span::styled(b.to_uppercase(), badge_style(b)));
                }
                out.push(Line::from(head));
                for (name, values) in fields {
                    if values.is_empty() {
                        continue;
                    }
                    out.push(Line::styled(format!("    {name}"), dim));
                    for v in values.iter().take(6) {
                        out.push(Line::raw(format!("      · {}", report::short(v, 96))));
                    }
                    if values.len() > 6 {
                        out.push(Line::styled(
                            format!("      + {} more", values.len() - 6),
                            dim,
                        ));
                    }
                }
            }
            Block::List { title, rows } => {
                out.push(Line::styled(format!("  {title}"), key));
                for (name, said) in rows.iter().take(40) {
                    out.push(Line::from(vec![
                        Span::raw(format!("    {:<44} ", report::short(name, 44))),
                        Span::styled(report::short(said, 52), dim),
                    ]));
                }
            }
            Block::Notes(notes) => {
                out.push(Line::styled("  Notes", key));
                for n in notes.iter().take(20) {
                    out.push(Line::styled(format!("    · {n}"), dim));
                }
            }
            Block::Text(t) => {
                for l in t.lines().take(200) {
                    out.push(Line::raw(format!("  {l}")));
                }
            }
        }
    }
    out
}

/// One finding: the badge, the location and the votes on a short first
/// line, the title under it, so the row fits a narrow pane instead of
/// wrapping mid-column. A row with no location keeps the title on the
/// first line.
fn row_lines(r: &report::Row, dropped: bool) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::raw("    ")];
    if let Some(b) = &r.badge {
        spans.push(Span::styled(
            format!("{:<8} ", b.to_uppercase()),
            if dropped { dim } else { badge_style(b) },
        ));
    }
    let title_style = if dropped { dim } else { Style::default() };
    let title = report::short(&r.title, 96);
    let mut under = None;
    match &r.location {
        Some(l) => {
            spans.push(Span::styled(
                format!("{:<30}", report::short(l, 30)),
                if dropped {
                    dim
                } else {
                    Style::default().fg(Color::Cyan)
                },
            ));
            under = Some(Line::styled(format!("      {title}"), title_style));
        }
        None => spans.push(Span::styled(format!("{title:<30}"), title_style)),
    }
    if let Some(votes) = votes_text(r.votes) {
        spans.push(Span::styled(format!("  {votes}"), dim));
    }
    let mut out = vec![Line::from(spans)];
    out.extend(under);
    out
}

/// `refuted 1/3`: the votes against out of the votes cast, named, so the
/// fraction cannot be read as the votes in favour.
fn votes_text(votes: Option<(u64, u64)>) -> Option<String> {
    votes.map(|(against, cast)| format!("refuted {against}/{cast}"))
}

fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
