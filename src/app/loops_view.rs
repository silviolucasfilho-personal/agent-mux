//! The Loops view (`E`): every registered loop grouped by workspace on
//! the left; Runs, Inbox, Readiness, Budget and Files on the right. Reads
//! the registry snapshot it was opened with, the store through a read-only
//! connection, and the workspace files. Decisions (`a`/`x` in the inbox,
//! `r`/`p`) are performed by `App`, never here.

use crate::loops::registry::{LoopEntry, Registry};
use crate::loops::store::{self as lstore, LoopRun};
use crate::loops::{Level, Outcome, format_tokens, from_ns, patterns};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopsPane {
    Loops,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopsTab {
    Runs,
    Inbox,
    Readiness,
    Budget,
    Files,
}

impl LoopsTab {
    pub const ALL: [LoopsTab; 5] = [
        LoopsTab::Runs,
        LoopsTab::Inbox,
        LoopsTab::Readiness,
        LoopsTab::Budget,
        LoopsTab::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LoopsTab::Runs => "Runs",
            LoopsTab::Inbox => "Inbox",
            LoopsTab::Readiness => "Readiness",
            LoopsTab::Budget => "Budget",
            LoopsTab::Files => "Files",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopRow {
    Header(String),
    Loop(usize),
}

const LIVE_REFRESH: Duration = Duration::from_millis(1000);
const RUN_LIMIT: usize = 100;

pub struct LoopsViewState {
    conn: Option<rusqlite::Connection>,
    pub error: Option<String>,
    pub loops: Vec<LoopEntry>,
    pub pause_all: bool,
    pub rows: Vec<LoopRow>,
    pub selected: usize,
    pub focus: LoopsPane,
    pub tab: LoopsTab,
    pub runs: Vec<LoopRun>,
    pub selected_run: usize,
    pub inbox: Vec<LoopRun>,
    pub selected_inbox: usize,
    pub detail_lines: Vec<Line<'static>>,
    /// The line index of the selected run / inbox item, for auto-follow.
    pub anchor_line: usize,
    pub scroll_offset: usize,
    pub viewport_rows: std::cell::Cell<usize>,
    /// Session ids of live runs, by loop id (from the App).
    pub live: Vec<(String, usize)>,
    worktrees_dir: String,
    last_refresh: Instant,
}

impl std::fmt::Debug for LoopsViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopsViewState")
            .field("open", &self.conn.is_some())
            .field("loops", &self.loops.len())
            .field("selected", &self.selected)
            .field("tab", &self.tab)
            .field("runs", &self.runs.len())
            .field("inbox", &self.inbox.len())
            .finish()
    }
}

impl LoopsViewState {
    pub fn new(
        db_path: Option<&Path>,
        registry: &Registry,
        selected_id: Option<&str>,
        worktrees_dir: &str,
    ) -> Self {
        let (conn, error) = match db_path {
            None => (None, Some("tracing is off — no runs recorded".to_string())),
            Some(p) => match crate::tracing::store::open_ro(p) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            },
        };
        let mut state = LoopsViewState {
            conn,
            error,
            loops: Vec::new(),
            pause_all: registry.pause_all,
            rows: Vec::new(),
            selected: 0,
            focus: LoopsPane::Loops,
            tab: LoopsTab::Runs,
            runs: Vec::new(),
            selected_run: 0,
            inbox: Vec::new(),
            selected_inbox: 0,
            detail_lines: Vec::new(),
            anchor_line: 0,
            scroll_offset: 0,
            viewport_rows: std::cell::Cell::new(30),
            live: Vec::new(),
            worktrees_dir: worktrees_dir.to_string(),
            last_refresh: Instant::now(),
        };
        state.reload(registry);
        if let Some(id) = selected_id
            && let Some(i) = state
                .rows
                .iter()
                .position(|r| matches!(r, LoopRow::Loop(i) if state.loops[*i].id == id))
        {
            state.selected = i;
            state.load_runs();
            state.rebuild_detail();
        }
        state
    }

    /// Takes a fresh registry snapshot, keeping the selection by id.
    pub fn reload(&mut self, registry: &Registry) {
        let keep = self.selected_loop().map(|l| l.id.clone());
        self.pause_all = registry.pause_all;
        let mut loops = registry.loops.clone();
        loops.sort_by(|a, b| {
            a.workspace_name()
                .cmp(&b.workspace_name())
                .then_with(|| a.pattern.cmp(&b.pattern))
        });
        self.loops = loops;
        let mut rows = Vec::new();
        let mut last_ws: Option<String> = None;
        for (i, l) in self.loops.iter().enumerate() {
            let ws = l.workspace.to_string_lossy().into_owned();
            if last_ws.as_deref() != Some(&ws) {
                rows.push(LoopRow::Header(ws.clone()));
                last_ws = Some(ws);
            }
            rows.push(LoopRow::Loop(i));
        }
        self.rows = rows;
        self.selected = keep
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, LoopRow::Loop(i) if self.loops[*i].id == id))
            })
            .unwrap_or(0);
        self.ensure_selectable();
        self.load_runs();
        self.load_inbox();
        self.rebuild_detail();
    }

    fn ensure_selectable(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if matches!(self.rows[self.selected], LoopRow::Loop(_)) {
            return;
        }
        if let Some(next) =
            (self.selected..self.rows.len()).find(|i| matches!(self.rows[*i], LoopRow::Loop(_)))
        {
            self.selected = next;
        } else if let Some(prev) = (0..self.selected)
            .rev()
            .find(|i| matches!(self.rows[*i], LoopRow::Loop(_)))
        {
            self.selected = prev;
        }
    }

    pub fn selected_loop(&self) -> Option<&LoopEntry> {
        match self.rows.get(self.selected)? {
            LoopRow::Loop(i) => self.loops.get(*i),
            LoopRow::Header(_) => None,
        }
    }

    pub fn selected_run(&self) -> Option<&LoopRun> {
        match self.tab {
            LoopsTab::Runs => self.runs.get(self.selected_run),
            LoopsTab::Inbox => self.inbox.get(self.selected_inbox),
            _ => None,
        }
    }

    /// The live session of the selected loop, if the App reported one.
    pub fn live_session(&self, loop_id: &str) -> Option<usize> {
        self.live
            .iter()
            .find(|(id, _)| id == loop_id)
            .map(|(_, sid)| *sid)
    }

    pub fn step(&mut self, delta: isize) {
        if self.rows.is_empty() || delta == 0 {
            return;
        }
        let mut target = self.selected;
        for _ in 0..delta.unsigned_abs() {
            let mut i = target as isize;
            let next = loop {
                i += delta.signum();
                if i < 0 || i as usize >= self.rows.len() {
                    break None;
                }
                if matches!(self.rows[i as usize], LoopRow::Loop(_)) {
                    break Some(i as usize);
                }
            };
            match next {
                Some(n) => target = n,
                None => break,
            }
        }
        if target != self.selected {
            self.selected = target;
            self.selected_run = 0;
            self.scroll_offset = 0;
            self.load_runs();
            self.rebuild_detail();
        }
    }

    pub fn step_detail(&mut self, delta: isize) {
        match self.tab {
            LoopsTab::Runs => {
                if !self.runs.is_empty() {
                    let max = self.runs.len() as isize - 1;
                    self.selected_run = (self.selected_run as isize + delta).clamp(0, max) as usize;
                    self.rebuild_detail();
                    self.follow_anchor();
                }
            }
            LoopsTab::Inbox => {
                if !self.inbox.is_empty() {
                    let max = self.inbox.len() as isize - 1;
                    self.selected_inbox =
                        (self.selected_inbox as isize + delta).clamp(0, max) as usize;
                    self.rebuild_detail();
                    self.follow_anchor();
                }
            }
            _ => {
                self.scroll_offset = if delta < 0 {
                    self.scroll_offset.saturating_sub(delta.unsigned_abs())
                } else {
                    self.scroll_offset
                        .saturating_add(delta as usize)
                        .min(self.max_scroll())
                };
            }
        }
    }

    fn follow_anchor(&mut self) {
        let visible = self.viewport_rows.get().max(1);
        if self.anchor_line < self.scroll_offset {
            self.scroll_offset = self.anchor_line;
        } else if self.anchor_line >= self.scroll_offset + visible {
            self.scroll_offset = self.anchor_line + 1 - visible;
        }
    }

    pub fn next_tab(&mut self) {
        let pos = LoopsTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        self.tab = LoopsTab::ALL[(pos + 1) % LoopsTab::ALL.len()];
        self.scroll_offset = 0;
        self.rebuild_detail();
    }

    pub fn prev_tab(&mut self) {
        let pos = LoopsTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        self.tab = LoopsTab::ALL[(pos + LoopsTab::ALL.len() - 1) % LoopsTab::ALL.len()];
        self.scroll_offset = 0;
        self.rebuild_detail();
    }

    pub fn max_scroll(&self) -> usize {
        let visible = self.viewport_rows.get().max(1);
        self.detail_lines.len().saturating_sub(visible)
    }

    pub fn refresh_if_live(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_refresh) < LIVE_REFRESH {
            return;
        }
        self.last_refresh = now;
        if self.conn.is_none() || !matches!(self.tab, LoopsTab::Runs | LoopsTab::Inbox) {
            return;
        }
        self.load_runs();
        self.load_inbox();
        self.rebuild_detail();
    }

    fn load_runs(&mut self) {
        let Some(conn) = &self.conn else {
            self.runs.clear();
            return;
        };
        self.runs = self
            .selected_loop()
            .and_then(|l| lstore::recent_runs(conn, &l.id, RUN_LIMIT).ok())
            .unwrap_or_default();
        self.selected_run = self.selected_run.min(self.runs.len().saturating_sub(1));
    }

    fn load_inbox(&mut self) {
        let Some(conn) = &self.conn else {
            self.inbox.clear();
            return;
        };
        self.inbox = lstore::inbox(conn).unwrap_or_default();
        self.selected_inbox = self.selected_inbox.min(self.inbox.len().saturating_sub(1));
    }

    fn spend_today(&self, loop_id: &str) -> lstore::Spend {
        let Some(conn) = &self.conn else {
            return lstore::Spend::default();
        };
        let since = crate::loops::to_ns(crate::loops::utc_midnight(crate::loops::now()));
        lstore::spend_since(conn, loop_id, since).unwrap_or_default()
    }

    fn store_activity(&self, workspace: &Path) -> usize {
        let Some(conn) = &self.conn else {
            return 0;
        };
        let since = crate::loops::to_ns(crate::loops::now() - time::Duration::days(14));
        lstore::activity_count(conn, &workspace.to_string_lossy(), since).unwrap_or(0)
    }

    /// Lines of the right pane for the current tab.
    pub fn rebuild_detail(&mut self) {
        let dim = Style::default().fg(Color::DarkGray);
        let head = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let mut lines: Vec<Line<'static>> = Vec::new();
        self.anchor_line = 0;
        let Some(entry) = self.selected_loop().cloned() else {
            lines.push(Line::styled(
                "  no loops yet — [a] in the Loops section adds one",
                dim,
            ));
            self.detail_lines = lines;
            return;
        };
        if let Some(e) = &self.error
            && matches!(
                self.tab,
                LoopsTab::Runs | LoopsTab::Inbox | LoopsTab::Budget
            )
        {
            lines.push(Line::styled(
                format!("  {e}"),
                Style::default().fg(Color::Yellow),
            ));
            lines.push(Line::raw(""));
        }
        match self.tab {
            LoopsTab::Runs => {
                lines.push(Line::styled(
                    format!(
                        "  {:<20} {:<13} {:<3} {:>5} {:>4} {:>4} {:>7} {:>7} {:>6}  {}",
                        "run",
                        "outcome",
                        "lvl",
                        "found",
                        "act",
                        "esc",
                        "tokens",
                        "cost",
                        "dur",
                        "verifier / files"
                    ),
                    head,
                ));
                if self.runs.is_empty() {
                    lines.push(Line::styled("  (no runs yet — [r] runs now)", dim));
                }
                for (i, r) in self.runs.iter().enumerate() {
                    let sel = i == self.selected_run;
                    if sel {
                        self.anchor_line = lines.len();
                    }
                    lines.push(run_line(r, sel));
                    if sel {
                        for l in run_detail_lines(r) {
                            lines.push(l);
                        }
                    }
                }
            }
            LoopsTab::Inbox => {
                lines.push(Line::styled(
                    "  runs waiting on a decision (all loops) — [a] applied  [x] rejected",
                    head,
                ));
                if self.inbox.is_empty() {
                    lines.push(Line::styled("  (nothing waiting)", dim));
                }
                for (i, r) in self.inbox.iter().enumerate() {
                    let sel = i == self.selected_inbox;
                    if sel {
                        self.anchor_line = lines.len();
                    }
                    let ws = Path::new(&r.workspace)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let marker = if sel { "> " } else { "  " };
                    let style = if sel {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::styled(
                        format!(
                            "{marker}{} · {} · {} · {}",
                            r.id,
                            r.pattern,
                            ws,
                            r.outcome.as_str()
                        ),
                        style,
                    ));
                    if sel {
                        if let Some(b) = &r.branch {
                            lines.push(Line::from(vec![
                                Span::styled("    branch   ", dim),
                                Span::raw(b.clone()),
                                Span::styled("   (merge it yourself; agent-mux never merges)", dim),
                            ]));
                        }
                        if let Some(w) = &r.worktree {
                            lines.push(Line::from(vec![
                                Span::styled("    worktree ", dim),
                                Span::raw(w.clone()),
                            ]));
                        }
                        for l in run_detail_lines(r) {
                            lines.push(l);
                        }
                        if let Some(stat) = r.detail_str("diff_stat") {
                            lines.push(Line::styled("    diff --stat", dim));
                            for s in stat.lines().take(20) {
                                lines.push(Line::raw(format!("      {s}")));
                            }
                        }
                    }
                }
            }
            LoopsTab::Readiness => {
                let audit = crate::loops::readiness::audit(
                    &entry.workspace,
                    self.store_activity(&entry.workspace),
                );
                for l in audit.human_lines() {
                    lines.push(Line::raw(l));
                }
                lines.push(Line::raw(""));
                lines.push(Line::styled("Level gates", head));
                for level in [Level::L1, Level::L2, Level::L3] {
                    let (glyph, text) = match audit.allows(level) {
                        Ok(()) => ("✓", format!("{}: ok", level.as_str())),
                        Err(why) => ("✗", why),
                    };
                    lines.push(Line::raw(format!("  {glyph} {text}")));
                }
                if !audit.activity_evidence.is_empty() {
                    lines.push(Line::raw(""));
                    lines.push(Line::styled("Activity evidence", head));
                    for e in &audit.activity_evidence {
                        lines.push(Line::raw(format!("  · {e}")));
                    }
                }
            }
            LoopsTab::Budget => {
                lines.push(Line::styled(
                    format!(
                        "  {:<22} {:<16} {:>9} {:>12} {:>5}  {}",
                        "loop", "workspace", "runs", "tokens", "%", "mode"
                    ),
                    head,
                ));
                for l in &self.loops {
                    let s = self.spend_today(&l.id);
                    let pct = if l.max_tokens_per_day == 0 {
                        0
                    } else {
                        (s.tokens.max(0) as u128 * 100 / l.max_tokens_per_day as u128) as u32
                    };
                    let mode = if pct >= 100 {
                        "blocked"
                    } else if pct >= 80 {
                        "report-only"
                    } else {
                        "normal"
                    };
                    let sel = l.id == entry.id;
                    lines.push(Line::styled(
                        format!(
                            "  {:<22} {:<16} {:>9} {:>12} {:>4}%  {}",
                            truncate(&l.pattern, 22),
                            truncate(&l.workspace_name(), 16),
                            format!("{}/{}", s.runs, l.max_runs_per_day),
                            format!(
                                "{}/{}",
                                format_tokens(s.tokens.max(0) as u64),
                                format_tokens(l.max_tokens_per_day)
                            ),
                            pct,
                            mode
                        ),
                        if sel {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default()
                        },
                    ));
                }
                if let Some(p) = patterns::find(&entry.pattern) {
                    lines.push(Line::raw(""));
                    let est = crate::loops::cost::estimate(p, entry.interval_s, entry.level, true);
                    for l in crate::loops::cost::human_lines(&est, &p.name) {
                        lines.push(Line::raw(l));
                    }
                }
                if let Some(conn) = &self.conn {
                    lines.push(Line::raw(""));
                    lines.push(Line::styled("Last seven days", head));
                    let now = crate::loops::now();
                    for d in 0..7 {
                        let day = crate::loops::utc_midnight(now - time::Duration::days(d));
                        let next = day + time::Duration::days(1);
                        let tokens: i64 = conn
                            .query_row(
                                "SELECT COALESCE(SUM(tokens), 0) FROM loop_runs WHERE loop_id = ?1 AND started_ns >= ?2 AND started_ns < ?3",
                                rusqlite::params![entry.id, crate::loops::to_ns(day), crate::loops::to_ns(next)],
                                |r| r.get(0),
                            )
                            .unwrap_or(0);
                        lines.push(Line::raw(format!(
                            "  {}  {:>8}",
                            crate::loops::format_timestamp(day).get(..10).unwrap_or(""),
                            format_tokens(tokens.max(0) as u64)
                        )));
                    }
                }
            }
            LoopsTab::Files => {
                lines.push(Line::styled(
                    format!("  {}", entry.workspace.display()),
                    head,
                ));
                if let Some(p) = patterns::find(&entry.pattern) {
                    for f in crate::loops::scaffold::contract_files(&entry.workspace, p) {
                        let (glyph, note) = match (f.present, f.stale) {
                            (false, _) => ("✗", "missing"),
                            (true, true) => ("!", "stale (Last run older than 14 days)"),
                            (true, false) => ("✓", "present"),
                        };
                        lines.push(Line::raw(format!("  {glyph} {:<28} {note}", f.name)));
                    }
                    lines.push(Line::raw(""));
                    lines.push(Line::styled("Skills", head));
                    let harness = crate::harness::Harness::detect(&entry.harness);
                    if let Some(h) = harness {
                        if let Some(dir) =
                            crate::loops::scaffold::project_skills_dir(h, &entry.workspace)
                        {
                            for s in &p.skills {
                                let path = dir.join(s).join("SKILL.md");
                                let glyph = if path.is_file() { "✓" } else { "✗" };
                                lines.push(Line::raw(format!("  {glyph} {}", path.display())));
                            }
                        }
                        if p.verifier
                            && let Some(v) =
                                crate::loops::scaffold::verifier_path(h, &entry.workspace)
                        {
                            let glyph = if v.is_file() { "✓" } else { "✗" };
                            lines.push(Line::raw(format!("  {glyph} {}", v.display())));
                        }
                    }
                }
                let manifest = crate::loops::worktree::load_manifest(
                    &entry.workspace.join(&self.worktrees_dir),
                );
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("Worktrees ({})", manifest.worktrees.len()),
                    head,
                ));
                for w in &manifest.worktrees {
                    lines.push(Line::raw(format!(
                        "  {:<10} {} · {} · {}",
                        w.status, w.id, w.branch, w.path
                    )));
                }
            }
        }
        self.detail_lines = lines;
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    } else {
        s.to_string()
    }
}

fn outcome_color(o: Outcome) -> Color {
    match o {
        Outcome::ReportOnly => Color::White,
        Outcome::FixProposed => Color::Magenta,
        Outcome::Escalated => Color::Yellow,
        Outcome::NoOp => Color::DarkGray,
        Outcome::Blocked => Color::Yellow,
        Outcome::Failed => Color::Red,
    }
}

fn run_line(r: &LoopRun, selected: bool) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    let when = r
        .started_ns
        .map(|ns| crate::loops::format_timestamp(from_ns(ns)))
        .unwrap_or_else(|| r.id.clone());
    let num = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
    let verifier = r
        .detail
        .get("verifier")
        .map(|v| {
            let ran = v.get("ran").and_then(|b| b.as_bool()).unwrap_or(false);
            let verdict = v.get("verdict").and_then(|s| s.as_str());
            match (ran, verdict) {
                (true, Some(v)) => v.to_string(),
                (true, None) => "ran".into(),
                (false, _) => "—".into(),
            }
        })
        .unwrap_or_else(|| "—".into());
    let files = r
        .detail
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let text = format!(
        "{marker}{:<20} {:<13} {:<3} {:>5} {:>4} {:>4} {:>7} {:>7} {:>6}  {} / {} file(s){}",
        when,
        r.outcome.as_str(),
        r.effective_level.as_str(),
        num(r.items_found),
        num(r.actions_taken),
        num(r.escalations),
        r.tokens
            .map(|t| format_tokens(t.max(0) as u64))
            .unwrap_or_else(|| "-".into()),
        r.cost_usd
            .map(|c| format!("${c:.2}"))
            .unwrap_or_else(|| "-".into()),
        r.duration_s()
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "-".into()),
        verifier,
        files,
        if r.detail.get("verifier_missing").is_some() {
            " ⚠ no verifier"
        } else {
            ""
        }
    );
    let style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(outcome_color(r.outcome))
    };
    Line::styled(text, style)
}

fn run_detail_lines(r: &LoopRun) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut out = Vec::new();
    let mut row = |k: &str, v: String| {
        out.push(Line::from(vec![
            Span::styled(format!("    {k:<10}"), dim),
            Span::raw(v),
        ]));
    };
    if let Some(v) = r.detail_str("reason") {
        row("blocked", v.to_string());
    }
    if let Some(v) = r.detail_str("level_reason") {
        row("capped", v.to_string());
    }
    if let Some(v) = r.detail_str("summary") {
        row("summary", v.to_string());
    }
    if let Some(v) = r.detail_str("gate_violation") {
        row("gate", format!("VIOLATION {v}"));
    }
    if let Some(files) = r.detail.get("files").and_then(|f| f.as_array())
        && !files.is_empty()
    {
        let list: Vec<String> = files
            .iter()
            .filter_map(|f| f.as_str().map(str::to_string))
            .take(12)
            .collect();
        row("files", list.join(", "));
    }
    if let Some(l) = &r.launch_id {
        row("launch", l.clone());
    }
    if let Some(d) = r.decision.as_deref() {
        row("decision", d.to_string());
    }
    out
}
