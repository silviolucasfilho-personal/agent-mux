//! One line for one step of a flow: its number, id, shape and who runs
//! it. The flow editor's chain and the Run screen's tree both draw a step
//! with `spans`, so a flow looks the same when it is built and when it
//! runs (spec 2026-11-13, section 3.3). The line is described by
//! `StepLine`, which knows neither screen's state.
//!
//! ```text
//! ✓ 2 find        ⇉ 4 in parallel   codex · gpt-5        4/4
//!   4 rank        ★ best of 3       codex                ! agy runs agents read-only
//! ```
//!
//! A line that does not fit sheds, in this order: the model, the long
//! shape for its short form, the agent, the shape, the harness; then the
//! id is cut. The state, the number, the counts and the `!` stay.

use crate::workflows::run_view::{Row, RowKind, State};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// What a step's line says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StepLine {
    /// The step's number in the flow, from 1; a row under a step (a
    /// session, the refuters, the judge) has none.
    pub number: Option<usize>,
    /// The step's id, or what a row under it is called (`[1]/vote1`,
    /// `└ 2 votes`).
    pub id: String,
    /// How it runs, in words: `⇉ 4 in parallel`. May be empty.
    pub shape: String,
    /// The same in a few characters, for a narrow line: `⇉ 4`.
    pub shape_short: String,
    pub harness: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// The state glyph and its style; the editor has none.
    pub state: Option<(&'static str, Style)>,
    /// Sessions done over sessions planned.
    pub counts: Option<(usize, usize)>,
    /// The step names no harness: this is the flow's, drawn dimmed.
    pub inherited: bool,
    /// Why the step will not run; the line shows `!` and the reason.
    pub problem: Option<String>,
}

impl StepLine {
    /// The line of a step or session row of the Run screen's tree.
    pub fn of_row(row: &Row) -> StepLine {
        let shape = row.shape.clone().unwrap_or_default();
        StepLine {
            number: (row.kind == RowKind::Step).then_some(row.number).flatten(),
            id: row.label.clone(),
            shape: shape.words,
            shape_short: shape.short,
            harness: row.who.harness.clone(),
            agent: row.who.agent.clone(),
            model: row.who.model.clone(),
            state: Some((row.state.glyph(), state_style(row.state))),
            counts: row.counts(),
            inherited: row.who.inherited,
            problem: None,
        }
    }
}

/// The colour of a state glyph, as the Swarm tree's.
pub fn state_style(state: State) -> Style {
    let fg = match state {
        State::Waiting | State::Skipped => Color::DarkGray,
        State::Running | State::PartlyDone => Color::Yellow,
        State::Done => Color::Green,
        State::Failed => Color::Red,
        State::NullAnswer => Color::Magenta,
        State::Replayed => Color::Cyan,
    };
    Style::default().fg(fg)
}

/// The widths of the id and shape columns, shared by the lines of one
/// list so their columns line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub id: usize,
    pub shape: usize,
    pub shape_short: usize,
}

/// A long id does not push the other columns off the line.
const ID_MAX: usize = 16;
const SHAPE_MAX: usize = 20;

impl Columns {
    /// Wide enough for every line of the list.
    pub fn of<'a>(lines: impl IntoIterator<Item = &'a StepLine>) -> Columns {
        let mut c = Columns {
            id: 0,
            shape: 0,
            shape_short: 0,
        };
        for l in lines {
            c.id = c.id.max(l.id.width().min(ID_MAX));
            c.shape = c.shape.max(l.shape.width().min(SHAPE_MAX));
            c.shape_short = c.shape_short.max(l.shape_short.width());
        }
        c
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// `s` cut to `max` columns, with an ellipsis when it was cut.
fn fit(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(ch);
        used += w;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

fn pad(s: &str, to: usize) -> String {
    format!("{s}{}", " ".repeat(to.saturating_sub(s.width())))
}

/// How much of a line is drawn.
#[derive(Clone, Copy)]
struct Detail {
    model: bool,
    long_shape: bool,
    agent: bool,
    shape: bool,
    harness: bool,
}

const DETAILS: [Detail; 6] = [
    Detail {
        model: true,
        long_shape: true,
        agent: true,
        shape: true,
        harness: true,
    },
    Detail {
        model: false,
        long_shape: true,
        agent: true,
        shape: true,
        harness: true,
    },
    Detail {
        model: false,
        long_shape: false,
        agent: true,
        shape: true,
        harness: true,
    },
    Detail {
        model: false,
        long_shape: false,
        agent: false,
        shape: true,
        harness: true,
    },
    Detail {
        model: false,
        long_shape: false,
        agent: false,
        shape: false,
        harness: true,
    },
    Detail {
        model: false,
        long_shape: false,
        agent: false,
        shape: false,
        harness: false,
    },
];

/// The line at one level of detail, the id cut to `id_width`.
fn build(line: &StepLine, cols: Columns, d: Detail, id_width: usize) -> Vec<Span<'static>> {
    let wide = d.long_shape;
    let gap = if wide { "  " } else { " " };
    let mut spans: Vec<Span<'static>> = Vec::new();
    if let Some((glyph, style)) = line.state {
        spans.push(Span::styled(format!("{glyph} "), style));
    }
    if let Some(n) = line.number {
        spans.push(Span::styled(format!("{n} "), dim()));
    }
    let who = d.harness && !line.harness.is_empty();
    let shape = d.shape && (cols.shape > 0 || cols.shape_short > 0);
    let id = fit(&line.id, id_width);
    let id_style = Style::default().add_modifier(Modifier::BOLD);
    if shape || who {
        spans.push(Span::styled(pad(&id, cols.id.min(id_width)), id_style));
    } else {
        spans.push(Span::styled(id, id_style));
    }
    if shape {
        let (text, width) = if wide {
            (fit(&line.shape, SHAPE_MAX), cols.shape)
        } else {
            (line.shape_short.clone(), cols.shape_short)
        };
        // a list where no line has a short shape keeps no column for it
        if width > 0 {
            spans.push(Span::raw(gap));
            spans.push(Span::raw(if who { pad(&text, width) } else { text }));
        }
    }
    if who {
        let sep = if wide { " · " } else { "·" };
        spans.push(Span::raw(gap));
        spans.push(Span::styled(
            line.harness.clone(),
            if line.inherited {
                dim()
            } else {
                Style::default()
            },
        ));
        if d.agent
            && let Some(a) = &line.agent
        {
            spans.push(Span::styled(sep, dim()));
            spans.push(Span::styled(a.clone(), Style::default().fg(Color::Magenta)));
        }
        if d.model
            && let Some(m) = &line.model
        {
            spans.push(Span::styled(sep, dim()));
            spans.push(Span::styled(m.clone(), dim()));
        }
    }
    if let Some((done, planned)) = line.counts {
        spans.push(Span::raw(gap));
        spans.push(Span::raw(format!("{done}/{planned}")));
    }
    if line.problem.is_some() {
        spans.push(Span::raw(gap));
        spans.push(Span::styled(
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

fn width_of(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// The line as spans, no wider than `width` columns. `cols` comes from
/// `Columns::of` over the list the line belongs to.
pub fn spans(line: &StepLine, cols: Columns, width: usize) -> Vec<Span<'static>> {
    let mut out = DETAILS
        .iter()
        .map(|d| build(line, cols, *d, ID_MAX))
        .find(|s| width_of(s) <= width)
        .unwrap_or_else(|| {
            // nothing but the id is left to shed: cut it
            let bare = DETAILS[DETAILS.len() - 1];
            let fixed = width_of(&build(line, cols, bare, 0));
            build(line, cols, bare, width.saturating_sub(fixed))
        });
    // the reason takes what is left of the line
    if let Some(reason) = &line.problem {
        let left = width.saturating_sub(width_of(&out) + 1);
        if left >= 4 {
            out.push(Span::styled(
                format!(" {}", fit(reason, left)),
                Style::default().fg(Color::Red),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::run_view::{Shape, Who};

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn line(
        n: usize,
        id: &str,
        shape: (&str, &str),
        who: (&str, Option<&str>, Option<&str>),
    ) -> StepLine {
        StepLine {
            number: Some(n),
            id: id.into(),
            shape: shape.0.into(),
            shape_short: shape.1.into(),
            harness: who.0.into(),
            agent: who.1.map(str::to_string),
            model: who.2.map(str::to_string),
            ..StepLine::default()
        }
    }

    /// The chain of spec 3.2.
    fn chain() -> Vec<StepLine> {
        vec![
            line(1, "scope", ("once", ""), ("claude", Some("planner"), None)),
            line(
                2,
                "find",
                ("⇉ 4 in parallel", "⇉ 4"),
                ("codex", None, Some("gpt-5")),
            ),
            line(
                3,
                "confirmed",
                ("↓ each · 2 votes", "↓"),
                ("claude", Some("skeptic"), None),
            ),
            line(4, "rank", ("★ best of 3", "★ 3"), ("codex", None, None)),
        ]
    }

    fn draw(lines: &[StepLine], width: usize) -> Vec<String> {
        let cols = Columns::of(lines);
        lines
            .iter()
            .map(|l| {
                let s = spans(l, cols, width);
                assert!(
                    width_of(&s) <= width,
                    "{:?} is wider than {width}",
                    text(&s)
                );
                text(&s)
            })
            .collect()
    }

    #[test]
    fn a_wide_line_names_the_shape_the_harness_the_agent_and_the_model() {
        assert_eq!(
            draw(&chain(), 80),
            [
                "1 scope      once              claude · planner",
                "2 find       ⇉ 4 in parallel   codex · gpt-5",
                "3 confirmed  ↓ each · 2 votes  claude · skeptic",
                "4 rank       ★ best of 3       codex",
            ]
        );
    }

    #[test]
    fn a_narrow_line_sheds_the_model_then_the_words_then_the_agent() {
        // 38 columns: the model goes, on the line it does not fit
        assert_eq!(
            draw(&chain(), 38)[1],
            "2 find       ⇉ 4 in parallel   codex"
        );
        // the sidebar's width: short shapes, compact who
        // the words give way to the short shapes and a compact who
        assert_eq!(
            draw(&chain(), 30),
            [
                "1 scope         claude·planner",
                "2 find      ⇉ 4 codex",
                "3 confirmed ↓   claude·skeptic",
                "4 rank      ★ 3 codex",
            ]
        );
        // inside the sidebar's borders the agent goes too
        assert_eq!(
            draw(&chain(), 28),
            [
                "1 scope         claude",
                "2 find      ⇉ 4 codex",
                "3 confirmed ↓   claude",
                "4 rank      ★ 3 codex",
            ]
        );
        assert_eq!(draw(&chain(), 22)[2], "3 confirmed ↓   claude");
        assert_eq!(draw(&chain(), 14)[2], "3 confirmed");
        assert_eq!(draw(&chain(), 8)[2], "3 confi…");
        assert_eq!(draw(&chain(), 2)[2], "3 ");
    }

    #[test]
    fn the_state_the_counts_and_the_problem_stay_on_a_narrow_line() {
        let mut l = line(
            3,
            "confirmed",
            ("↓ each · 2 votes", "↓"),
            ("claude", Some("skeptic"), None),
        );
        l.state = Some(("◷", state_style(State::PartlyDone)));
        l.counts = Some((2, 4));
        let cols = Columns::of([&l]);
        assert_eq!(
            text(&spans(&l, cols, 60)),
            "◷ 3 confirmed  ↓ each · 2 votes  claude · skeptic  2/4"
        );
        assert_eq!(text(&spans(&l, cols, 30)), "◷ 3 confirmed ↓ claude 2/4");
        assert_eq!(text(&spans(&l, cols, 18)), "◷ 3 confirmed 2/4");
        assert_eq!(text(&spans(&l, cols, 12)), "◷ 3 con… 2/4");

        l.problem = Some("agy runs custom agents read-only".into());
        let wide = spans(&l, cols, 100);
        assert_eq!(
            text(&wide),
            "◷ 3 confirmed  ↓ each · 2 votes  claude · skeptic  2/4  ! agy runs custom agents read-only"
        );
        let cut = text(&spans(&l, cols, 70));
        assert!(cut.ends_with("! agy runs cu…"), "{cut}");
        assert_eq!(cut.width(), 70);
        let narrow = text(&spans(&l, cols, 20));
        assert_eq!(narrow, "◷ 3 confirmed 2/4 !");
    }

    #[test]
    fn an_inherited_harness_is_dimmed_and_an_own_one_is_not() {
        let mut l = line(1, "scope", ("once", ""), ("claude", Some("planner"), None));
        let cols = Columns::of([&l]);
        let harness = |l: &StepLine| {
            spans(l, cols, 80)
                .into_iter()
                .find(|s| s.content == "claude")
                .unwrap()
                .style
        };
        assert_eq!(harness(&l), Style::default());
        l.inherited = true;
        assert_eq!(harness(&l), dim());
    }

    #[test]
    fn a_wide_character_is_never_split() {
        let l = line(
            1,
            "検索検索検索検索検索",
            ("once", ""),
            ("claude", None, None),
        );
        let cols = Columns::of([&l]);
        for width in 0..40 {
            let s = spans(&l, cols, width);
            assert!(width_of(&s) <= width.max(2), "{width}: {:?}", text(&s));
        }
    }

    #[test]
    fn a_row_of_the_run_tree_becomes_a_line() {
        let row = Row {
            kind: RowKind::Step,
            depth: 1,
            state: State::PartlyDone,
            number: Some(2),
            step_id: "find".into(),
            label: "find".into(),
            session: None,
            shape: Some(Shape {
                words: "⇉ 4 in parallel".into(),
                short: "⇉ 4".into(),
            }),
            who: Who {
                harness: "codex".into(),
                inherited: true,
                agent: None,
                model: Some("gpt-5".into()),
                effort: None,
            },
            done: 1,
            planned: 4,
            running: 1,
            tokens: 0,
            cost_usd: 0.0,
            started_ns: None,
            ended_ns: None,
            launch_id: None,
            null_reason: None,
        };
        let l = StepLine::of_row(&row);
        assert_eq!(
            text(&spans(&l, Columns::of([&l]), 80)),
            "◷ 2 find  ⇉ 4 in parallel  codex · gpt-5  1/4"
        );
        assert!(l.inherited);
        // a session row has no number of its own
        let session = Row {
            kind: RowKind::Session,
            depth: 2,
            state: State::Running,
            label: "[1]/vote1".into(),
            shape: None,
            ..row
        };
        let l = StepLine::of_row(&session);
        assert_eq!(
            text(&spans(&l, Columns::of([&l]), 80)),
            "▶ [1]/vote1  codex · gpt-5"
        );
    }
}
