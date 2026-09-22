//! The Loops view (`E`): every registered loop grouped by workspace on
//! the left; Runs, Inbox, Readiness, Budget and Files on the right. Reads
//! the registry snapshot it was opened with, the store through a read-only
//! connection, and the workspace files. Decisions (`a`/`x` in the inbox,
//! `r`/`p`) are performed by `App`, never here.

use crate::loops::registry::{LoopEntry, Registry};
use crate::loops::store::{self as lstore, LoopRun};
use crate::loops::{Level, Outcome, format_tokens, patterns};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopsPane {
    Loops,
    Detail,
}

/// `Report` is what the selected run found and who has to act; `Runs` is
/// the timeline it sits in. The other three are the loop's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopsTab {
    Report,
    Runs,
    Inbox,
    Readiness,
    Budget,
    Files,
}

impl LoopsTab {
    pub const ALL: [LoopsTab; 6] = [
        LoopsTab::Report,
        LoopsTab::Runs,
        LoopsTab::Inbox,
        LoopsTab::Readiness,
        LoopsTab::Budget,
        LoopsTab::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LoopsTab::Report => "Report",
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
    /// Where the per-run copies of the state file are kept.
    runtime: Option<std::path::PathBuf>,
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
        runtime: Option<&Path>,
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
            tab: LoopsTab::Report,
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
            runtime: runtime.map(Path::to_path_buf),
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
            // The Report follows the run selected in the Runs tab; here the
            // arrows move between runs so the reader can step back in time.
            LoopsTab::Report | LoopsTab::Runs => {
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
        if self.conn.is_none()
            || !matches!(
                self.tab,
                LoopsTab::Report | LoopsTab::Runs | LoopsTab::Inbox
            )
        {
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

    /// The state text this run wrote: its own snapshot, or the workspace's
    /// current file when the run is the newest one (the file is rewritten
    /// in place, so only the newest run's report is still on disk).
    fn state_text(&self, entry: &LoopEntry, run: Option<&LoopRun>) -> Option<(String, bool)> {
        if let (Some(rt), Some(r)) = (&self.runtime, run)
            && let Some(text) = crate::loops::state::read_snapshot(rt, &entry.id, &r.id)
        {
            return Some((text, true));
        }
        let newest = self.runs.first().map(|r| r.id.as_str());
        let is_newest = run.is_none() || run.map(|r| r.id.as_str()) == newest;
        if !is_newest {
            return None;
        }
        let p = patterns::find(&entry.pattern)?;
        std::fs::read_to_string(entry.workspace.join(&p.state_file))
            .ok()
            .map(|t| (t, false))
    }

    /// The Report tab: what the selected run found, who has to act, and
    /// what moved since the run before it.
    fn report_lines(&self, entry: &LoopEntry) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let head = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let mut out: Vec<Line<'static>> = Vec::new();
        let run = self
            .runs
            .get(self.selected_run)
            .or_else(|| self.runs.first());
        match run {
            None => {
                out.push(Line::styled("  no runs yet — [r] runs this loop now", dim));
                return out;
            }
            Some(r) => {
                out.extend(run_headline(r));
                if let Some(s) = r.detail_str("summary") {
                    out.push(Line::raw(format!("  {s}")));
                }
                if let Some(d) = run_delta(r) {
                    out.push(Line::from(vec![
                        Span::styled("  Since last run  ", dim),
                        Span::raw(d.summary()),
                    ]));
                } else if r.detail.get("quiet").is_some() {
                    out.push(Line::styled("  Since last run  nothing moved", dim));
                }
            }
        }
        out.push(Line::styled(
            "  ────────────────────────────────────────",
            dim,
        ));
        let Some((text, from_snapshot)) = self.state_text(entry, run) else {
            out.push(Line::styled(
                "  no report kept for this run — agent-mux keeps one copy of the state file per run",
                dim,
            ));
            if let Some(m) = run.and_then(|r| r.detail_str("final_message")) {
                out.push(Line::raw(""));
                out.push(Line::styled("  What the run said", head));
                for l in m.lines().take(30) {
                    out.push(Line::raw(format!("    {l}")));
                }
            }
            return out;
        };
        let report = crate::loops::state::parse(&text);
        if report.is_empty() {
            out.push(Line::styled(
                "  the state file does not follow the loop shape — showing it as written",
                dim,
            ));
            for l in text.lines().take(60) {
                out.push(Line::raw(format!("  {l}")));
            }
            return out;
        }
        for kind in [
            crate::loops::state::SectionKind::NeedsYou,
            crate::loops::state::SectionKind::Watching,
            crate::loops::state::SectionKind::Ignored,
            crate::loops::state::SectionKind::Other,
        ] {
            let Some(section) = report.of_kind(kind) else {
                continue;
            };
            let items = report.items_of(kind);
            if items.is_empty() && section.notes.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(Line::raw(""));
            }
            out.push(Line::styled(
                format!("  {} ({})", kind.label(), items.len()),
                head,
            ));
            let limit = if kind == crate::loops::state::SectionKind::NeedsYou {
                20
            } else {
                8
            };
            for item in items.iter().take(limit) {
                out.extend(item_lines(item, kind));
            }
            if items.len() > limit {
                out.push(Line::styled(
                    format!("    + {} more", items.len() - limit),
                    dim,
                ));
            }
            for n in section.notes.iter().take(4) {
                out.push(Line::styled(format!("    note  {n}"), dim));
            }
        }
        if let Some(m) = run.and_then(|r| r.detail_str("final_message")) {
            out.push(Line::raw(""));
            out.push(Line::styled("  What the run said", head));
            for l in m.lines().take(24) {
                out.push(Line::raw(format!("    {l}")));
            }
        }
        out.push(Line::raw(""));
        out.push(Line::styled(
            if from_snapshot {
                format!(
                    "  the report this run wrote · last run {}",
                    report.last_run.clone().unwrap_or_default()
                )
            } else {
                format!(
                    "  the workspace's state file · last run {}",
                    report.last_run.clone().unwrap_or_default()
                )
            },
            dim,
        ));
        out
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
            LoopsTab::Report => {
                lines.extend(self.report_lines(&entry));
            }
            LoopsTab::Runs => {
                if self.runs.is_empty() {
                    lines.push(Line::styled("  (no runs yet — [r] runs now)", dim));
                }
                // A fifteen-minute loop is mostly quiet runs: consecutive
                // quiet ones fold into one row so the runs that changed
                // something stay on the screen.
                let mut i = 0usize;
                while i < self.runs.len() {
                    let key = crate::loops::run_fold_key(&self.runs[i])
                        .filter(|_| i != self.selected_run);
                    if let Some(k) = key {
                        let mut j = i + 1;
                        while j < self.runs.len()
                            && j != self.selected_run
                            && crate::loops::run_fold_key(&self.runs[j]).as_deref()
                                == Some(k.as_str())
                        {
                            j += 1;
                        }
                        if j - i >= 2 {
                            let group = &self.runs[i..j];
                            let tokens: i64 =
                                group.iter().filter_map(|r| r.tokens).sum::<i64>().max(0);
                            let cost: f64 = group.iter().filter_map(|r| r.cost_usd).sum::<f64>();
                            let when = |r: &LoopRun| {
                                crate::workflows::report::format_when(
                                    r.started_ns.unwrap_or(r.scheduled_ns),
                                )
                            };
                            let mut text = format!(
                                "  {} … {}   {k} ×{}",
                                group.last().map(when).unwrap_or_default(),
                                group.first().map(when).unwrap_or_default(),
                                group.len()
                            );
                            if tokens > 0 {
                                text.push_str(&format!(
                                    "   {} tokens · {}",
                                    format_tokens(tokens as u64),
                                    crate::workflows::report::Cost::of(
                                        (cost > 0.0).then_some(cost),
                                        &group[0].harness
                                    )
                                    .text()
                                ));
                            }
                            lines.push(Line::styled(truncate(&text, 112), dim));
                            i = j;
                            continue;
                        }
                    }
                    let sel = i == self.selected_run;
                    if sel {
                        self.anchor_line = lines.len();
                    }
                    for l in run_card(&self.runs[i], sel) {
                        lines.push(l);
                    }
                    if sel {
                        for l in run_detail_lines(&self.runs[i]) {
                            lines.push(l);
                        }
                    }
                    i += 1;
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
                            // which model checks the work, as the file says
                            let declared = std::fs::read_to_string(&v)
                                .ok()
                                .and_then(|t| {
                                    t.lines().find_map(|l| {
                                        l.strip_prefix("model:").map(str::trim).map(str::to_string)
                                    })
                                })
                                .unwrap_or_default();
                            let want = entry.verifier_model.trim();
                            let note = match (declared.as_str(), want) {
                                ("", "") => String::new(),
                                (d, w) if w.is_empty() || d == w => format!("  model {d}"),
                                ("", w) => {
                                    format!("  ! the loop asks for {w}, the file declares none")
                                }
                                (d, w) => format!("  ! the loop asks for {w}, the file says {d}"),
                            };
                            lines.push(Line::raw(format!("  {glyph} {}{note}", v.display())));
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

/// One run as two lines: what happened, then what changed.
fn run_card(r: &LoopRun, selected: bool) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut out = run_headline(r);
    if selected {
        let text: String = out[0]
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect::<Vec<_>>()
            .join("");
        out[0] = Line::styled(text, Style::default().add_modifier(Modifier::REVERSED));
    }
    let second = r
        .detail_str("summary")
        .map(|s| truncate(s, 104))
        .or_else(|| run_delta(r).map(|d| d.summary()))
        .or_else(|| {
            r.detail
                .get("quiet")
                .is_some()
                .then(|| "nothing moved".to_string())
        });
    if let Some(t) = second {
        out.push(Line::styled(format!("    {t}"), dim));
    }
    if r.detail.get("verifier_missing").is_some() {
        out.push(Line::styled(
            "    ⚠ a fix with no verifier observation — treat it as unverified",
            Style::default().fg(Color::Yellow),
        ));
    }
    out
}

/// The expanded detail of the selected run: why the outcome is what it is,
/// what the run touched, what it said, and where to look next.
fn run_detail_lines(r: &LoopRun) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut row = |k: &str, v: String, style: Style| {
        out.push(Line::from(vec![
            Span::styled(format!("      {k:<9}"), key),
            Span::styled(v, style),
        ]));
    };
    let why = why_line(r);
    if !why.is_empty() {
        row("Why", why, Style::default());
    }
    if let Some(v) = r.detail_str("level_reason") {
        row("Capped", v.to_string(), dim);
    }
    if let Some(v) = r.detail_str("gate_violation") {
        row(
            "Gate",
            format!("VIOLATION {v}"),
            Style::default().fg(Color::Red),
        );
    }
    row("Verifier", verifier_text(r), dim);
    let files: Vec<String> = r
        .detail
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() {
        row("Touched", "no files".into(), dim);
    } else {
        let list: Vec<String> = files.iter().take(10).cloned().collect();
        row(
            "Touched",
            format!("{} · {}", files.len(), list.join(", ")),
            Style::default(),
        );
    }
    if let Some(b) = &r.branch {
        row(
            "Branch",
            format!("{b}  (merge it yourself; agent-mux never merges)"),
            Style::default(),
        );
    }
    if let Some(w) = &r.worktree {
        row("Worktree", w.clone(), dim);
    }
    if let Some(m) = r.detail_str("final_message") {
        let first: Vec<&str> = m.lines().filter(|l| !l.trim().is_empty()).take(3).collect();
        for (i, l) in first.iter().enumerate() {
            let label = if i == 0 { "Said" } else { "" };
            out.push(Line::from(vec![
                Span::styled(format!("      {label:<9}"), key),
                Span::raw(truncate(l, 100)),
            ]));
        }
    }
    let mut run_facts: Vec<String> = Vec::new();
    if let Some(l) = &r.launch_id {
        run_facts.push(format!("launch {}", l.get(..8).unwrap_or(l)));
    }
    if let Some(c) = r.detail.get("exit_code").and_then(|c| c.as_i64()) {
        run_facts.push(format!("exit {c}"));
    }
    if r.detail.get("timed_out").is_some() {
        run_facts.push("timed out".into());
    }
    if let Some(d) = r.decision.as_deref() {
        run_facts.push(format!("decided {d}"));
    }
    if !run_facts.is_empty() {
        out.push(Line::from(vec![
            Span::styled(format!("      {:<9}", "Run"), key),
            Span::styled(run_facts.join(" · "), dim),
        ]));
    }
    if let Some(stat) = r.detail_str("diff_stat") {
        out.push(Line::styled("      diff --stat", key));
        for l in stat.lines().take(12) {
            out.push(Line::styled(format!("        {l}"), dim));
        }
    }
    out
}

/// Why this run carries this outcome. Every clause names a fact agent-mux
/// observed; nothing is inferred beyond them.
fn why_line(r: &LoopRun) -> String {
    let mut parts: Vec<String> = Vec::new();
    match r.outcome {
        Outcome::Blocked => {
            return format!(
                "blocked before it started · {}",
                r.detail_str("reason").unwrap_or("no reason recorded")
            );
        }
        Outcome::Failed => {
            if let Some(reason) = r.detail_str("reason") {
                return reason.to_string();
            }
            if r.detail.get("timed_out").is_some() {
                return "the run passed its timeout and was killed".into();
            }
            if let Some(c) = r.detail.get("exit_code").and_then(|c| c.as_i64())
                && c != 0
            {
                return format!("the harness exited {c}");
            }
            return "the run did not finish cleanly".into();
        }
        Outcome::FixProposed => parts.push("the worktree carries a change".into()),
        Outcome::Escalated => {
            match verifier_verdict(r) {
                Some("ESCALATE_HUMAN") => parts.push("a checker asked for a human".into()),
                Some("REJECT") => parts.push("a checker rejected the change".into()),
                _ => {}
            }
            if r.detail_str("gate_violation").is_some() {
                parts.push("a touched path is on the denylist".into());
            }
            if parts.is_empty() {
                parts.push("the run asked for a decision".into());
            }
        }
        Outcome::ReportOnly => parts.push("the state file was rewritten, nothing else".into()),
        Outcome::NoOp => parts.push("nothing changed since the run before it".into()),
    }
    if let Some(d) = run_delta(r)
        && !d.is_empty()
    {
        parts.push(d.summary());
    }
    parts.join(" · ")
}

fn verifier_verdict(r: &LoopRun) -> Option<&str> {
    r.detail.get("verifier")?.get("verdict")?.as_str()
}

fn verifier_text(r: &LoopRun) -> String {
    let ran = r
        .detail
        .get("verifier")
        .and_then(|v| v.get("ran"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let label = r
        .detail
        .get("verifier")
        .and_then(|v| v.get("label"))
        .and_then(|l| l.as_str());
    match (ran, verifier_verdict(r), r.effective_level) {
        (true, Some(v), _) => label.unwrap_or(v).to_string(),
        (true, None, _) => "ran, no verdict line".into(),
        (false, _, Level::L1) => "not required at L1".into(),
        (false, _, _) => "did not run".into(),
    }
}

/// `Sep 17 17:36   NEEDS YOU   8 found · 2 for you · 417k · $1.18 · 1m 43s`
fn run_headline(r: &LoopRun) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let when = crate::workflows::report::format_when(r.started_ns.unwrap_or(r.scheduled_ns));
    let facts = match r.outcome {
        // A run that never started has no counts; the reason is the fact.
        Outcome::Blocked => r
            .detail_str("reason")
            .unwrap_or("no reason recorded")
            .to_string(),
        _ => {
            let mut facts: Vec<String> = Vec::new();
            if let Some(n) = r.items_found {
                facts.push(format!("{n} found"));
            }
            if let Some(n) = r.escalations.filter(|n| *n > 0) {
                facts.push(format!("{n} for you"));
            }
            if let Some(n) = r.actions_taken.filter(|n| *n > 0) {
                facts.push(format!("{n} action"));
            }
            if let Some(t) = r.tokens {
                facts.push(format!("{} tokens", format_tokens(t.max(0) as u64)));
            }
            if r.tokens.is_some_and(|t| t > 0) {
                facts.push(crate::workflows::report::Cost::of(r.cost_usd, &r.harness).text());
            }
            if let Some(d) = r.duration_s() {
                facts.push(crate::workflows::report::format_duration(d));
            }
            facts.join(" · ")
        }
    };
    vec![
        Line::from(vec![
            Span::raw(format!("  {when}   ")),
            Span::styled(
                r.outcome.word().to_uppercase(),
                Style::default()
                    .fg(outcome_color(r.outcome))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match r.readiness_score {
                    // The score the pre-flight audit computed for this run,
                    // not the workspace's score today (the Readiness tab).
                    Some(s) => format!("   {} · readiness {s}", r.effective_level.as_str()),
                    None => format!("   {}", r.effective_level.as_str()),
                },
                dim,
            ),
        ]),
        Line::styled(format!("  {facts}"), dim),
    ]
}

fn run_delta(r: &LoopRun) -> Option<crate::loops::state::Delta> {
    let v = r.detail.get("delta")?;
    serde_json::from_value(v.clone()).ok()
}

/// One item of a state-file section: what it is, what the loop did and
/// what the user has to decide.
fn item_lines(
    item: &crate::loops::state::Item,
    kind: crate::loops::state::SectionKind,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut out = Vec::new();
    let head = truncate(&item.headline(), 96);
    out.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            head,
            if kind == crate::loops::state::SectionKind::NeedsYou {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
    ]));
    if let Some(s) = &item.status {
        out.push(Line::styled(format!("      {}", truncate(s, 104)), dim));
    }
    if kind != crate::loops::state::SectionKind::NeedsYou {
        return out;
    }
    if let Some(d) = &item.human_decision {
        out.push(Line::from(vec![
            Span::styled("      Decide   ", Style::default().fg(Color::Yellow)),
            Span::raw(truncate(d, 92)),
        ]));
    }
    if let Some(a) = &item.loop_action {
        out.push(Line::from(vec![
            Span::styled("      Loop did  ", dim),
            Span::styled(truncate(a, 92), dim),
        ]));
    }
    out
}
