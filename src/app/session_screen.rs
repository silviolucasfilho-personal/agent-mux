//! The session run screen (`Mode::SessionRun`): one interactive session,
//! a skill's, a persona's or a plain harness's, in the Run screen's layout.
//! The tree has the session and its sub-agents; the main pane has the
//! selected row in tabs: a card that says who is running and how far it
//! got, the session's own terminal, its trace, and the files it touched.
//! Design:
//! docs/superpowers/specs/2026-11-14-skill-and-persona-run-screen-design.md
//!
//! A live session is read from the App (`Session`), a past one from the
//! trace store by its launch. This file holds the state, the keys and the
//! text of the tabs; `ui::session_screen` draws it.

use super::{App, Mode, Notice};
use crate::harness::Harness;
use crate::keymap::{Verb, verb};
use crate::status::Status;
use crate::tracing::store::query;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// How often the open screen reads the session again.
const REFRESH: Duration = Duration::from_millis(1000);

/// Where the screen was opened from, and `Esc` returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Agents,
    Runs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Card,
    Live,
    Trace,
    Changes,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Card, Tab::Live, Tab::Trace, Tab::Changes];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Card => "Card",
            Tab::Live => "Live",
            Tab::Trace => "Trace",
            Tab::Changes => "Changes",
        }
    }
}

/// Which session the screen is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A session of this App, by its id.
    Live(usize),
    /// A launch in the trace store.
    Stored(String),
}

/// What the session runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Skill,
    Persona,
    /// A plain harness session: no skill, no persona.
    Harness,
}

impl Kind {
    pub fn glyph(self) -> &'static str {
        match self {
            Kind::Skill => "⚡",
            Kind::Persona => "◇",
            Kind::Harness => "●",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Persona => "persona",
            Kind::Harness => "session",
        }
    }
}

/// Who is running and how far it got.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub kind: Kind,
    /// The skill's or persona's name; a plain session's profile.
    pub name: String,
    pub description: String,
    /// `claude`, `codex`.
    pub harness: String,
    pub profile: String,
    pub model: Option<String>,
    /// A persona's tool list; a skill and a plain session take the
    /// harness's own.
    pub tools: Option<String>,
    pub folder: String,
    /// The message the session was opened with.
    pub asked: Option<String>,
    /// `working`, `idle`, `attention`, `exit 0`, `ended`.
    pub state: String,
    pub ended: bool,
    pub turns: i64,
    pub tokens: Option<i64>,
    /// `$1.23`, `≈$1.23`, `?`.
    pub cost: Option<String>,
    pub running_tool: Option<String>,
    pub launch_id: Option<String>,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
}

impl Card {
    /// `12t · 41k`.
    pub fn short_stats(&self) -> String {
        let mut parts = Vec::new();
        if self.turns > 0 {
            parts.push(format!("{}t", self.turns));
        }
        if let Some(t) = self.tokens.filter(|t| *t > 0) {
            parts.push(crate::loops::format_tokens(t as u64));
        }
        parts.join(" ")
    }
}

/// A sub-agent of the session: a row under it.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgent {
    /// The observation's id, which the trace is narrowed to.
    pub id: String,
    pub name: String,
    pub running: bool,
    pub tokens: Option<i64>,
}

pub struct SessionScreen {
    pub source: Source,
    pub origin: Origin,
    pub card: Card,
    pub agents: Vec<SubAgent>,
    /// 0 is the session; `n` is sub-agent `n - 1`.
    pub cursor: usize,
    pub tab: Tab,
    /// The Live tab's terminal has the keyboard.
    pub pane_focused: bool,
    pub scroll: usize,
    pub max_scroll: Cell<usize>,
    /// The trace's lines, and the sub-agent they are narrowed to.
    pub trace: Option<(Option<String>, Vec<String>)>,
    /// The Changes tab: files touched, then the folder's diff.
    pub changes: Option<Vec<Line<'static>>>,
    pub last_refresh: Instant,
}

impl std::fmt::Debug for SessionScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionScreen")
            .field("source", &self.source)
            .field("tab", &self.tab)
            .finish()
    }
}

impl SessionScreen {
    /// The sub-agent under the cursor.
    pub fn selected_agent(&self) -> Option<&SubAgent> {
        self.cursor.checked_sub(1).and_then(|i| self.agents.get(i))
    }

    /// The pane's title: what the row is and where it stands.
    pub fn pane_title(&self) -> String {
        if let Some(a) = self.selected_agent() {
            return format!(
                "↳ {} · {}",
                a.name,
                if a.running { "running" } else { "done" }
            );
        }
        let c = &self.card;
        let mut parts = vec![c.name.clone(), c.harness.clone()];
        parts.extend(c.model.clone());
        parts.push(c.state.clone());
        parts.join(" · ")
    }

    fn rows(&self) -> usize {
        1 + self.agents.len()
    }

    fn move_cursor(&mut self, delta: isize) {
        let at = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows().saturating_sub(1));
        if at != self.cursor {
            self.cursor = at;
            self.scroll = 0;
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll.get());
    }
}

// ---- the text of the tabs -------------------------------------------------

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn field(k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {k:<9} "), Style::default().fg(Color::Yellow)),
        Span::raw(v),
    ])
}

/// The Card tab.
pub fn card_lines(c: &Card) -> Vec<Line<'static>> {
    let mut lines = vec![field(
        c.kind.word(),
        if c.description.is_empty() {
            c.name.clone()
        } else {
            format!("{} · {}", c.name, c.description)
        },
    )];
    lines.push(field(
        "harness",
        match &c.model {
            Some(m) => format!("{} ({}) · model {m}", c.harness, c.profile),
            None => format!("{} ({})", c.harness, c.profile),
        },
    ));
    lines.push(field(
        "tools",
        c.tools
            .clone()
            .unwrap_or_else(|| "the harness's own".into()),
    ));
    lines.push(field("folder", c.folder.clone()));
    if let Some(a) = &c.asked {
        let mut first = true;
        for l in a.lines().filter(|l| !l.trim().is_empty()).take(12) {
            lines.push(if first {
                field("asked", l.to_string())
            } else {
                Line::raw(format!("            {l}"))
            });
            first = false;
        }
    }
    let mut so_far = vec![format!("{} turn(s)", c.turns)];
    if let Some(t) = c.tokens.filter(|t| *t > 0) {
        so_far.push(format!("{} tokens", crate::loops::format_tokens(t as u64)));
    }
    so_far.extend(c.cost.clone());
    if let Some(t) = &c.running_tool {
        so_far.push(format!("⚙ {t}"));
    }
    lines.push(field("so far", so_far.join(" · ")));
    lines.push(field("state", c.state.clone()));
    if let Some(l) = &c.launch_id {
        lines.push(field("launch", l.clone()));
    }
    if c.launch_id.is_none() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "  Not traced: turns, tools and files are not recorded for it.",
            dim(),
        ));
    }
    lines
}

/// The last argument that is a message and not a flag: what a persona's
/// task or a skill's invocation opened the session with.
fn opening_message(args: &[String]) -> Option<String> {
    args.iter()
        .rev()
        .find(|a| !a.starts_with('-') && !a.trim().is_empty())
        .cloned()
}

impl App {
    /// The skill or persona named `name` as the card describes it: its
    /// description and, for a persona, its tools.
    fn session_owner(
        &self,
        skill: Option<&str>,
        agent: Option<&str>,
    ) -> (Kind, String, String, Option<String>) {
        if let Some(name) = agent {
            let spec = crate::agents::Catalog::load(&self.library_root(), None)
                .entry(name)
                .and_then(|e| e.spec.clone());
            return (
                Kind::Persona,
                name.to_string(),
                spec.as_ref()
                    .map(|s| s.description.clone())
                    .unwrap_or_default(),
                spec.map(|s| s.tools_label()),
            );
        }
        if let Some(id) = skill {
            let def = self.skills.iter().find(|s| s.id == id);
            return (
                Kind::Skill,
                def.map(|d| d.name.clone())
                    .unwrap_or_else(|| id.to_string()),
                def.map(|d| d.description.clone()).unwrap_or_default(),
                None,
            );
        }
        (Kind::Harness, String::new(), String::new(), None)
    }

    /// The card of a session of this App.
    fn live_session_card(&self, id: usize) -> Option<Card> {
        let s = self.sessions.iter().find(|s| s.id == id)?;
        let (kind, name, description, tools) =
            self.session_owner(s.skill_id.as_deref(), s.agent.as_deref());
        let status = s.status(Instant::now());
        let stats = s.trace_stats.as_ref();
        Some(Card {
            kind,
            name: if kind == Kind::Harness {
                s.profile.name.clone()
            } else {
                name
            },
            description,
            harness: Harness::detect(&s.profile.command)
                .map(|h| h.as_str().to_string())
                .unwrap_or_else(|| s.profile.command.clone()),
            profile: s.profile.name.clone(),
            model: crate::ui::session_model(s),
            tools,
            folder: s.dir.display().to_string(),
            asked: opening_message(&s.opening_args),
            state: crate::ui::status_label_style(status).0,
            ended: matches!(status, Status::Exited(_)),
            turns: stats.map_or(0, |t| t.turns),
            tokens: stats.and_then(|t| t.total_tokens),
            cost: stats.and_then(|t| t.cost_label()),
            running_tool: stats
                .and_then(|t| t.running_tool.clone())
                .filter(|_| matches!(status, Status::Working)),
            launch_id: s.trace.as_ref().map(|t| t.launch_id.clone()),
            started_ns: None,
            ended_ns: None,
        })
    }

    /// The card of a launch in the trace store.
    fn stored_session_card(&self, launch_id: &str) -> Result<Card, String> {
        let conn = self
            .trace_db_path
            .as_deref()
            .ok_or("no trace store: a past run is read from it")
            .and_then(|p| {
                crate::tracing::store::open_ro(p).map_err(|_| "the trace store could not be read")
            })?;
        let l = query::launch_card(&conn, launch_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("launch {launch_id} is not in the store"))?;
        let stats = query::launch_stats(&conn, launch_id).ok();
        let (kind, name, description, tools) =
            self.session_owner(l.skill_id.as_deref(), l.agent.as_deref());
        Ok(Card {
            kind,
            name: if kind == Kind::Harness {
                l.profile.clone()
            } else {
                name
            },
            description,
            harness: l.provider.clone(),
            profile: l.profile.clone(),
            model: l.model.clone(),
            tools,
            folder: l.cwd.clone(),
            asked: None,
            state: match (l.live, l.exit_code) {
                (true, _) => "running elsewhere".into(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "ended".into(),
            },
            ended: !l.live,
            turns: stats.as_ref().map_or(0, |t| t.turns),
            tokens: stats.as_ref().and_then(|t| t.total_tokens),
            cost: stats.as_ref().and_then(|t| t.cost_label()),
            running_tool: None,
            launch_id: Some(l.id),
            started_ns: Some(l.started_ns),
            ended_ns: l.ended_ns,
        })
    }

    /// The index of the screen's session in `sessions`, while it is listed.
    pub fn session_screen_index(&self, st: &SessionScreen) -> Option<usize> {
        match &st.source {
            Source::Live(id) => self.sessions.iter().position(|s| s.id == *id),
            Source::Stored(_) => None,
        }
    }

    /// Reads the session again: its card, its sub-agents, and the open
    /// tab's trace or changes.
    fn fill_session_screen(&mut self, st: &mut SessionScreen) {
        let card = match &st.source {
            Source::Live(id) => self.live_session_card(*id).ok_or(()),
            Source::Stored(l) => self.stored_session_card(l).map_err(|_| ()),
        };
        match card {
            Ok(c) => st.card = c,
            // the session left the list: its launch is still in the store
            Err(()) => {
                if let (Source::Live(_), Some(l)) = (&st.source, st.card.launch_id.clone()) {
                    st.source = Source::Stored(l.clone());
                    st.pane_focused = false;
                    if let Ok(c) = self.stored_session_card(&l) {
                        st.card = c;
                    }
                } else {
                    st.card.ended = true;
                    st.pane_focused = false;
                }
            }
        }
        // the terminal's keys go to the App's selected session
        if let Some(i) = self.session_screen_index(st) {
            self.selected = i;
        }
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let launch = st.card.launch_id.clone();
        st.agents = match (&conn, &launch) {
            (Some(c), Some(l)) => crate::tracing::swarm_report::sub_agents(c, l)
                .unwrap_or_default()
                .into_iter()
                .map(|a| SubAgent {
                    running: a.end_ns.is_none() && !st.card.ended,
                    id: a.id,
                    name: a.name,
                    tokens: a.tokens,
                })
                .collect(),
            _ => Vec::new(),
        };
        st.cursor = st.cursor.min(st.agents.len());
        let agent = st.selected_agent().map(|a| a.id.clone());
        // a sub-agent's row is its part of the trace, whatever the tab
        if st.tab == Tab::Trace || agent.is_some() {
            st.trace = match (&conn, &launch) {
                (Some(c), Some(l)) => Some((
                    agent.clone(),
                    crate::tracing::swarm_report::launch_trace_lines(c, l, agent.as_deref())
                        .unwrap_or_else(|e| vec![format!("  {e}")]),
                )),
                _ => None,
            };
        }
        if st.tab == Tab::Changes && agent.is_none() {
            let mut lines: Vec<Line<'static>> = Vec::new();
            let files = match (&conn, &launch) {
                (Some(c), Some(l)) => crate::loops::store::files_touched(c, l).unwrap_or_default(),
                _ => Vec::new(),
            };
            lines.push(Line::styled(
                format!("  Files its tools wrote ({})", files.len()),
                Style::default().fg(Color::Yellow),
            ));
            if files.is_empty() {
                lines.push(Line::styled("    none recorded", dim()));
            }
            lines.extend(files.iter().map(|f| Line::raw(format!("    {f}"))));
            // the folder's own state: it may hold changes this session
            // did not make
            let diff = std::process::Command::new("git")
                .arg("-C")
                .arg(&st.card.folder)
                .args(["diff", "--stat"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            if let Some(d) = diff {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "  The folder's uncommitted changes (git diff --stat; not only this session's)",
                    Style::default().fg(Color::Yellow),
                ));
                if d.trim().is_empty() {
                    lines.push(Line::styled("    none", dim()));
                }
                lines.extend(d.lines().map(|l| Line::raw(format!("   {l}"))));
            }
            st.changes = Some(lines);
        }
        st.last_refresh = Instant::now();
    }

    fn open_session_screen_on(&mut self, source: Source, origin: Origin) -> bool {
        let card = match &source {
            Source::Live(id) => self
                .live_session_card(*id)
                .ok_or_else(|| "that session is gone".to_string()),
            Source::Stored(l) => self.stored_session_card(l),
        };
        match card {
            Ok(card) => {
                let mut st = SessionScreen {
                    source,
                    origin,
                    card,
                    agents: Vec::new(),
                    cursor: 0,
                    tab: Tab::Card,
                    pane_focused: false,
                    scroll: 0,
                    max_scroll: Cell::new(usize::MAX),
                    trace: None,
                    changes: None,
                    last_refresh: Instant::now(),
                };
                self.fill_session_screen(&mut st);
                self.mode = Mode::SessionRun(Box::new(st));
                true
            }
            Err(e) => {
                self.notice = Some(Notice::warn(e));
                false
            }
        }
    }

    /// `i` on a session row: the screen on the selected session.
    pub fn open_session_screen(&mut self) -> bool {
        match self.sessions.get(self.selected).map(|s| s.id) {
            Some(id) => self.open_session_screen_on(Source::Live(id), Origin::Agents),
            None => {
                self.notice = Some(Notice::info("i opens a session's run screen"));
                false
            }
        }
    }

    /// `Enter` on a persona's live session in the runs view: the screen
    /// on the selected session, returning to the runs view.
    pub fn open_session_screen_from_runs(&mut self) -> bool {
        match self.sessions.get(self.selected).map(|s| s.id) {
            Some(id) => self.open_session_screen_on(Source::Live(id), Origin::Runs),
            None => false,
        }
    }

    /// The screen on a past run, read from the store by its launch.
    pub fn open_session_screen_stored(&mut self, launch_id: &str, origin: Origin) -> bool {
        // a launch that is a session of this App is read live
        let live = self
            .sessions
            .iter()
            .find(|s| s.trace.as_ref().is_some_and(|t| t.launch_id == launch_id))
            .map(|s| s.id);
        match live {
            Some(id) => self.open_session_screen_on(Source::Live(id), origin),
            None => self.open_session_screen_on(Source::Stored(launch_id.to_string()), origin),
        }
    }

    /// Ticks the open screen: a session that is going moves.
    pub fn refresh_session_screen(&mut self, now: Instant) {
        let due = matches!(&self.mode, Mode::SessionRun(st)
            if !st.card.ended && now.saturating_duration_since(st.last_refresh) >= REFRESH);
        if !due {
            return;
        }
        if let Mode::SessionRun(mut st) = std::mem::replace(&mut self.mode, Mode::Main) {
            self.fill_session_screen(&mut st);
            self.mode = Mode::SessionRun(st);
        }
    }

    pub fn handle_session_screen_key(&mut self, key: &KeyEvent) {
        let Mode::SessionRun(mut st) = std::mem::replace(&mut self.mode, Mode::Main) else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let before = (st.cursor, st.tab);
        let mut stay = true;
        match key.code {
            KeyCode::Char(c @ '1'..='4') if st.cursor == 0 => {
                st.tab = Tab::ALL[c as usize - '1' as usize];
                st.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => st.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => st.move_cursor(-1),
            // the session's terminal takes the keyboard
            KeyCode::Enter => {
                if self.session_screen_index(&st).is_some() && !st.card.ended {
                    st.cursor = 0;
                    st.tab = Tab::Live;
                    st.pane_focused = true;
                } else {
                    self.notice = Some(Notice::info(
                        "there is no terminal to type into: the session has ended, or runs elsewhere",
                    ));
                }
            }
            KeyCode::Char('i') if !ctrl => stay = false,
            _ => match verb(&crate::keymap::list_alias(key)) {
                Some(Verb::Back) => stay = false,
                Some(Verb::Top) => st.scroll = 0,
                Some(Verb::Bottom) => st.scroll = st.max_scroll.get(),
                Some(Verb::PageDown) => st.scroll_by(10),
                Some(Verb::PageUp) => st.scroll_by(-10),
                Some(Verb::Reload) => self.fill_session_screen(&mut st),
                _ => {}
            },
        }
        if !stay {
            // back to where it was opened from: the session's row, or the
            // runs view on the run
            if st.origin == Origin::Runs {
                // the key the runs view gives the run (`RunRef::key`)
                let key = match (&st.card.launch_id, &st.source) {
                    (Some(l), _) => format!("persona:{l}"),
                    (None, Source::Live(id)) => format!("persona-session:{id}"),
                    (None, Source::Stored(l)) => format!("persona:{l}"),
                };
                self.open_runs_view_on(&key, super::runs_view::RunTab::Report);
            }
            return;
        }
        if (st.cursor, st.tab) != before {
            self.fill_session_screen(&mut st);
        }
        self.mode = Mode::SessionRun(st);
    }
}
