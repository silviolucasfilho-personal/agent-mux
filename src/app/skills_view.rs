//! The Skills view (`S`): every skill grouped by harness, where it is
//! installed, and when it ran. Reads packages from disk, install state from
//! each harness's skill directory, harness-native definitions from the
//! inventory, and executions from the trace store through a read-only
//! connection. The only writes are `install`/`uninstall` on the filesystem,
//! performed by `App`, never here.

use crate::harness::Harness;
use crate::skill::SkillDefinition;
use crate::skill::install::InstallStatus;
use crate::tracing::inventory::{Definition, Kind, SkillReport, inventory_all, skill_reports};
use crate::tracing::store::query::{self, SkillLaunch, SkillTurn};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsPane {
    Skills,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsTab {
    Details,
    Executions,
}

impl SkillsTab {
    pub fn label(self) -> &'static str {
        match self {
            SkillsTab::Details => "Details",
            SkillsTab::Executions => "Executions",
        }
    }
}

/// One row of the left pane. Headers are not selectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRow {
    Header(Harness),
    /// An agent-mux package under one of the harnesses it declares.
    Package {
        index: usize,
        harness: Harness,
    },
    /// A skill the harness found on its own (not an agent-mux package).
    Native {
        index: usize,
    },
}

impl SkillRow {
    pub fn harness(&self, native: &[Definition]) -> Option<Harness> {
        match self {
            SkillRow::Header(h) => Some(*h),
            SkillRow::Package { harness, .. } => Some(*harness),
            SkillRow::Native { index } => native.get(*index).map(|d| d.harness),
        }
    }
}

/// Install state as shown in the list's right-hand column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLabel {
    Current,
    Stale,
    NotManaged,
    NotInstalled,
}

impl InstallLabel {
    pub fn text(self) -> &'static str {
        match self {
            InstallLabel::Current => "installed ✓",
            InstallLabel::Stale => "stale",
            InstallLabel::NotManaged => "not managed",
            InstallLabel::NotInstalled => "not installed",
        }
    }

    pub fn of(status: &InstallStatus) -> Self {
        match (status.installed, status.managed, status.current) {
            (false, _, _) => InstallLabel::NotInstalled,
            (true, false, _) => InstallLabel::NotManaged,
            (true, true, true) => InstallLabel::Current,
            (true, true, false) => InstallLabel::Stale,
        }
    }
}

/// Provider name the store records for a harness.
pub fn provider_of(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
        Harness::Antigravity => "antigravity",
    }
}

const LIVE_REFRESH: Duration = Duration::from_millis(500);
const LAUNCH_LIMIT: usize = 100;
const TURN_LIMIT: usize = 200;

pub struct SkillsViewState {
    conn: Option<rusqlite::Connection>,
    /// Store problem, shown in the Executions tab; packages and install
    /// state stay usable without a store.
    pub error: Option<String>,
    pub packages: Vec<SkillDefinition>,
    pub native: Vec<Definition>,
    pub rows: Vec<SkillRow>,
    /// Index into `rows`; never a header while a selectable row exists.
    pub selected: usize,
    pub harness_filter: Option<Harness>,
    pub focus: SkillsPane,
    pub tab: SkillsTab,
    pub launches: Vec<SkillLaunch>,
    pub turns: Vec<SkillTurn>,
    /// Index into the Executions list: launches first, then turns.
    pub selected_execution: usize,
    pub scroll_offset: usize,
    /// Right-pane interior height, written back by the renderer.
    pub viewport_rows: std::cell::Cell<usize>,
    pub install: HashMap<(String, Harness), InstallStatus>,
    pub reports: Vec<SkillReport>,
    pub detail_lines: Vec<Line<'static>>,
    last_refresh: Instant,
    cwd: PathBuf,
    home: PathBuf,
    /// Where packages are installed (the real home unless a test overrides).
    install_home: PathBuf,
    skills_dir: Option<PathBuf>,
}

impl std::fmt::Debug for SkillsViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillsViewState")
            .field("open", &self.conn.is_some())
            .field("error", &self.error)
            .field("packages", &self.packages.len())
            .field("native", &self.native.len())
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .field("harness_filter", &self.harness_filter)
            .field("tab", &self.tab)
            .field("launches", &self.launches.len())
            .field("turns", &self.turns.len())
            .finish()
    }
}

impl SkillsViewState {
    /// `db_path` is `None` when tracing is off. `home` is where harness
    /// definitions and skill directories are read; `install_home` where
    /// packages are installed (the same unless a test overrides it).
    pub fn new(
        db_path: Option<&Path>,
        cwd: &Path,
        home: &Path,
        install_home: &Path,
        skills_dir: Option<&Path>,
    ) -> Self {
        let (conn, error) = match db_path {
            None => (
                None,
                Some("tracing is off — no executions recorded".to_string()),
            ),
            Some(path) => match crate::tracing::store::open_ro(path) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            },
        };
        let mut state = SkillsViewState {
            conn,
            error,
            packages: Vec::new(),
            native: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            harness_filter: None,
            focus: SkillsPane::Skills,
            tab: SkillsTab::Details,
            launches: Vec::new(),
            turns: Vec::new(),
            selected_execution: 0,
            scroll_offset: 0,
            viewport_rows: std::cell::Cell::new(30),
            install: HashMap::new(),
            reports: Vec::new(),
            detail_lines: Vec::new(),
            last_refresh: Instant::now(),
            cwd: cwd.to_path_buf(),
            home: home.to_path_buf(),
            install_home: install_home.to_path_buf(),
            skills_dir: skills_dir.map(Path::to_path_buf),
        };
        state.reload();
        state
    }

    pub fn install_home(&self) -> &Path {
        &self.install_home
    }

    /// Rescans packages, harness definitions, install state and store
    /// statistics, keeping the selection on the same row when it survives.
    pub fn reload(&mut self) {
        let keep = self.selected_key();
        let (packages, _) = crate::skill::load_skills(self.skills_dir.as_deref());
        // workflow step skills and the planner are hidden packages: not
        // agents, so not listed here either
        self.packages = packages.into_iter().filter(|p| !p.hidden).collect();
        self.native = inventory_all(&self.cwd, &self.home)
            .into_iter()
            .filter(|d| d.kind == Kind::Skill)
            .collect();
        self.install.clear();
        for def in &self.packages {
            for h in Harness::ALL {
                if def.harnesses.contains(&h) {
                    let status = crate::skill::install::status(def, h, &self.install_home);
                    self.install.insert((def.id.clone(), h), status);
                }
            }
        }
        let (stats, prompts) = match &self.conn {
            Some(conn) => (
                query::skill_stats(conn).unwrap_or_default(),
                query::prompt_rows(conn, 5000).unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        let defs = inventory_all(&self.cwd, &self.home);
        self.reports = skill_reports(&defs, &stats, &prompts);
        self.rebuild_rows();
        if let Some(key) = keep
            && let Some(idx) = self
                .rows
                .iter()
                .position(|r| self.row_key(r) == Some(key.clone()))
        {
            self.selected = idx;
        }
        self.ensure_selectable();
        self.load_executions();
        self.rebuild_detail();
    }

    /// A stable identity for a row: `(harness, skill name)`.
    fn row_key(&self, row: &SkillRow) -> Option<(Harness, String)> {
        match row {
            SkillRow::Header(_) => None,
            SkillRow::Package { index, harness } => {
                self.packages.get(*index).map(|p| (*harness, p.id.clone()))
            }
            SkillRow::Native { index } => {
                self.native.get(*index).map(|d| (d.harness, d.name.clone()))
            }
        }
    }

    fn selected_key(&self) -> Option<(Harness, String)> {
        self.rows.get(self.selected).and_then(|r| self.row_key(r))
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for h in Harness::ALL {
            if self.harness_filter.is_some_and(|f| f != h) {
                continue;
            }
            rows.push(SkillRow::Header(h));
            let mut pkgs: Vec<usize> = (0..self.packages.len())
                .filter(|i| self.packages[*i].harnesses.contains(&h))
                .collect();
            pkgs.sort_by(|a, b| {
                self.packages[*a]
                    .name
                    .to_lowercase()
                    .cmp(&self.packages[*b].name.to_lowercase())
            });
            for index in pkgs {
                rows.push(SkillRow::Package { index, harness: h });
            }
            let mut natives: Vec<usize> = (0..self.native.len())
                .filter(|i| {
                    let d = &self.native[*i];
                    d.harness == h
                        && !self
                            .packages
                            .iter()
                            .any(|p| p.id == d.name && p.harnesses.contains(&h))
                })
                .collect();
            natives.sort_by(|a, b| {
                self.native[*a]
                    .name
                    .to_lowercase()
                    .cmp(&self.native[*b].name.to_lowercase())
            });
            for index in natives {
                rows.push(SkillRow::Native { index });
            }
        }
        self.rows = rows;
    }

    /// Moves the selection off a header onto the nearest selectable row.
    fn ensure_selectable(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if !matches!(self.rows[self.selected], SkillRow::Header(_)) {
            return;
        }
        if let Some(next) =
            (self.selected..self.rows.len()).find(|i| !matches!(self.rows[*i], SkillRow::Header(_)))
        {
            self.selected = next;
        } else if let Some(prev) = (0..self.selected)
            .rev()
            .find(|i| !matches!(self.rows[*i], SkillRow::Header(_)))
        {
            self.selected = prev;
        }
    }

    pub fn selected_row(&self) -> Option<&SkillRow> {
        self.rows.get(self.selected)
    }

    /// The package and harness of the selected row, when it is a package.
    pub fn selected_package(&self) -> Option<(&SkillDefinition, Harness)> {
        match self.selected_row()? {
            SkillRow::Package { index, harness } => {
                self.packages.get(*index).map(|p| (p, *harness))
            }
            _ => None,
        }
    }

    /// Stable identity used when returning to the workbench after an edit,
    /// launch, or trace drill-down. Native definitions are intentionally not
    /// returned because agent-mux does not own their lifecycle.
    pub fn selected_identity(&self) -> Option<(String, Harness)> {
        self.selected_package()
            .map(|(package, harness)| (package.id.clone(), harness))
    }

    /// Restores a managed package row by stable identity. Clears a harness
    /// filter when it would hide the requested row.
    pub fn select_package(&mut self, id: &str, harness: Harness) -> bool {
        if self.harness_filter.is_some_and(|filter| filter != harness) {
            self.harness_filter = None;
            self.rebuild_rows();
        }
        let Some(selected) = self.rows.iter().position(|row| match row {
            SkillRow::Package {
                index,
                harness: row_harness,
            } => {
                *row_harness == harness
                    && self.packages.get(*index).is_some_and(|package| package.id == id)
            }
            _ => false,
        }) else {
            return false;
        };
        self.selected = selected;
        self.on_selection_changed();
        true
    }

    pub fn selected_native(&self) -> Option<&Definition> {
        match self.selected_row()? {
            SkillRow::Native { index } => self.native.get(*index),
            _ => None,
        }
    }

    /// `j`/`k` (and the wheel): `|delta|` selectable rows in that
    /// direction; headers are skipped, the ends clamp.
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
                if !matches!(self.rows[i as usize], SkillRow::Header(_)) {
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
            self.on_selection_changed();
        }
    }

    fn on_selection_changed(&mut self) {
        if !self.tabs().contains(&self.tab) {
            self.tab = SkillsTab::Details;
        }
        self.scroll_offset = 0;
        self.load_executions();
        self.rebuild_detail();
    }

    /// `1`/`2`/`3`: narrow to one harness; the same key again clears.
    pub fn toggle_filter(&mut self, harness: Harness) {
        self.harness_filter = if self.harness_filter == Some(harness) {
            None
        } else {
            Some(harness)
        };
        let keep = self.selected_key();
        self.rebuild_rows();
        self.selected = keep
            .and_then(|key| {
                self.rows
                    .iter()
                    .position(|r| self.row_key(r) == Some(key.clone()))
            })
            .unwrap_or(0);
        self.ensure_selectable();
        self.on_selection_changed();
    }

    /// Clears the filter; returns whether there was one.
    pub fn clear_filter(&mut self) -> bool {
        if self.harness_filter.is_none() {
            return false;
        }
        self.harness_filter = None;
        let keep = self.selected_key();
        self.rebuild_rows();
        self.selected = keep
            .and_then(|key| {
                self.rows
                    .iter()
                    .position(|r| self.row_key(r) == Some(key.clone()))
            })
            .unwrap_or(0);
        self.ensure_selectable();
        true
    }

    /// The tabs of the detail pane.
    pub fn tabs(&self) -> Vec<SkillsTab> {
        vec![SkillsTab::Details, SkillsTab::Executions]
    }

    pub fn next_tab(&mut self) {
        let tabs = self.tabs();
        let pos = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = tabs[(pos + 1) % tabs.len()];
        self.scroll_offset = 0;
    }

    pub fn prev_tab(&mut self) {
        let tabs = self.tabs();
        let pos = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = tabs[(pos + tabs.len() - 1) % tabs.len()];
        self.scroll_offset = 0;
    }

    /// Store statistics for the selected row, when the store has any.
    pub fn selected_report(&self) -> Option<&SkillReport> {
        let name = match self.selected_row()? {
            SkillRow::Package { index, .. } => self.packages.get(*index)?.id.as_str(),
            SkillRow::Native { index } => self.native.get(*index)?.name.as_str(),
            SkillRow::Header(_) => return None,
        };
        self.reports.iter().find(|r| r.name == name)
    }

    /// Re-queries the launches and turns of the selected row.
    pub fn load_executions(&mut self) {
        self.launches.clear();
        self.turns.clear();
        let Some(conn) = &self.conn else {
            self.selected_execution = 0;
            return;
        };
        match self.selected_row().cloned() {
            Some(SkillRow::Package { index, harness }) => {
                if let Some(def) = self.packages.get(index) {
                    let session_name = crate::skill::launch::session_name(def, harness);
                    self.launches = query::skill_launches(
                        conn,
                        &def.id,
                        &session_name,
                        Some(provider_of(harness)),
                        LAUNCH_LIMIT,
                    )
                    .unwrap_or_default();
                    self.turns = query::traces_with_skill_detail(conn, &def.id, TURN_LIMIT)
                        .unwrap_or_default();
                }
            }
            Some(SkillRow::Native { index }) => {
                if let Some(def) = self.native.get(index) {
                    let mut seen = std::collections::HashSet::new();
                    for name in def.store_names() {
                        for turn in query::traces_with_skill_detail(conn, &name, TURN_LIMIT)
                            .unwrap_or_default()
                        {
                            if seen.insert(turn.stat.id.clone()) {
                                self.turns.push(turn);
                            }
                        }
                    }
                    self.turns
                        .sort_by_key(|t| std::cmp::Reverse(t.stat.start_ns));
                    self.turns.truncate(TURN_LIMIT);
                }
            }
            _ => {}
        }
        self.selected_execution = self
            .selected_execution
            .min(self.execution_count().saturating_sub(1));
    }

    pub fn execution_count(&self) -> usize {
        self.launches.len() + self.turns.len()
    }

    /// The selected execution: a launch, or a turn.
    pub fn selected_execution(&self) -> Option<Execution<'_>> {
        if self.selected_execution < self.launches.len() {
            self.launches
                .get(self.selected_execution)
                .map(Execution::Launch)
        } else {
            self.turns
                .get(self.selected_execution - self.launches.len())
                .map(Execution::Turn)
        }
    }

    pub fn step_execution(&mut self, delta: isize) {
        let n = self.execution_count();
        if n == 0 {
            return;
        }
        self.selected_execution =
            (self.selected_execution as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Driven by the 250 ms tick: re-queries executions at most every
    /// 500 ms so live launches update. Never touches the filesystem.
    pub fn refresh_if_live(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_refresh) < LIVE_REFRESH {
            return;
        }
        self.last_refresh = now;
        if self.conn.is_none() || self.tab != SkillsTab::Executions {
            return;
        }
        self.load_executions();
    }

    pub fn max_scroll(&self) -> usize {
        let visible = self.viewport_rows.get().max(1);
        self.detail_lines.len().saturating_sub(visible)
    }

    /// Lines of the Details tab for the selected row.
    pub fn rebuild_detail(&mut self) {
        let key = Style::default().fg(Color::DarkGray);
        let head = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let row = |k: &str, v: String| {
            Line::from(vec![Span::styled(format!("  {k:<13}"), key), Span::raw(v)])
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        match self.selected_row().cloned() {
            Some(SkillRow::Package { index, harness }) => {
                let Some(def) = self.packages.get(index) else {
                    self.detail_lines = lines;
                    return;
                };
                lines.push(Line::styled("Package", head));
                let origin = if def.is_builtin {
                    "built-in (compiled into agent-mux)".to_string()
                } else {
                    def.dir
                        .as_ref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_else(|| "user package".into())
                };
                lines.push(row("Name", format!("{}  (id {})", def.name, def.id)));
                lines.push(row("Origin", origin));
                lines.push(row(
                    "Harnesses",
                    format!(
                        "{}  (default {})",
                        def.harnesses
                            .iter()
                            .map(|h| h.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        def.default_harness.as_str()
                    ),
                ));
                lines.push(row(
                    "Capabilities",
                    if def.capabilities.is_empty() {
                        "none declared".into()
                    } else {
                        def.capabilities.join(", ")
                    },
                ));
                lines.push(row("Description", def.description.clone()));
                if let Some(p) = &def.startup_prompt {
                    lines.push(row("Startup", p.clone()));
                }
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("Installed on {}", harness.display_name()),
                    head,
                ));
                match self.install.get(&(def.id.clone(), harness)) {
                    Some(st) => {
                        lines.push(row("Directory", st.dir.display().to_string()));
                        lines.push(row(
                            "State",
                            match InstallLabel::of(st) {
                                InstallLabel::Current => "installed, managed, current".into(),
                                InstallLabel::Stale => {
                                    "installed, managed, stale — `agent-mux skill install` refreshes it".into()
                                }
                                InstallLabel::NotManaged => {
                                    "present, not written by agent-mux (no manifest)".into()
                                }
                                InstallLabel::NotInstalled => "not installed".into(),
                            },
                        ));
                    }
                    None => lines.push(row("State", "not declared for this harness".into())),
                }
                lines.push(row("Hash", def.source_hash.chars().take(12).collect()));
                let mut files = vec!["SKILL.md".to_string()];
                files.extend(def.files.iter().map(|(p, _)| p.clone()));
                lines.push(row("Files", files.join(", ")));
            }
            Some(SkillRow::Native { index }) => {
                let Some(def) = self.native.get(index) else {
                    self.detail_lines = lines;
                    return;
                };
                lines.push(Line::styled(
                    format!("Native {} skill", def.harness.display_name()),
                    head,
                ));
                lines.push(row("Name", def.name.clone()));
                lines.push(row("Scope", def.scope.label()));
                lines.push(row("Path", def.path.display().to_string()));
                lines.push(row("Description", def.description.clone()));
                if !def.tools.is_empty() {
                    lines.push(row("Tools", def.tools.join(", ")));
                }
                if !def.triggers.is_empty() {
                    lines.push(row("Triggers", def.triggers.join(" · ")));
                }
                lines.push(row(
                    "Note",
                    "not an agent-mux package: the harness loads it directly".into(),
                ));
            }
            _ => {
                lines.push(Line::styled("no skills found", key));
                lines.push(Line::raw(""));
                lines.push(Line::styled("~/.agent-mux/skills/<id>/SKILL.md", key));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("Recorded activity", head));
        if let Some(e) = &self.error {
            lines.push(row("Store", e.clone()));
        }
        match self.selected_report() {
            Some(report) => {
                match &report.stat {
                    Some(stat) => {
                        lines.push(row(
                            "Turns",
                            format!(
                                "{} loaded · {} without attributed work · {} generations · {} tools",
                                stat.turns_loaded, stat.turns_unused, stat.generations, stat.tools
                            ),
                        ));
                        lines.push(row(
                            "Cost",
                            format!(
                                "{}  ({} tokens)",
                                crate::tracing::cli::fmt_cost(stat.cost),
                                crate::tracing::cli::fmt_tokens(stat.tokens)
                            ),
                        ));
                        lines.push(row(
                            "First / last",
                            format!(
                                "{}  →  {}",
                                crate::tracing::cli::fmt_time(stat.first_ns),
                                crate::tracing::cli::fmt_time(stat.last_ns)
                            ),
                        ));
                    }
                    None => lines.push(row("Turns", "never loaded in a recorded turn".into())),
                }
                if report.missed > 0 {
                    lines.push(row(
                        "Missed",
                        format!(
                            "{} prompt(s) quoted a trigger phrase without loading it",
                            report.missed
                        ),
                    ));
                }
            }
            None => match &self.error {
                Some(e) => lines.push(row("Store", e.clone())),
                None => lines.push(row("Turns", "no recorded activity".into())),
            },
        }
        self.detail_lines = lines;
    }
}

/// One row of the Executions tab.
#[derive(Debug, Clone, Copy)]
pub enum Execution<'a> {
    Launch(&'a SkillLaunch),
    Turn(&'a SkillTurn),
}

impl Execution<'_> {
    /// The session the execution belongs to, for the Trace Browser.
    pub fn session_key(&self) -> Option<&str> {
        match self {
            Execution::Launch(l) => l.session_key.as_deref(),
            Execution::Turn(t) => Some(t.stat.session_key.as_str()),
        }
    }

    pub fn trace_id(&self) -> Option<&str> {
        match self {
            Execution::Launch(_) => None,
            Execution::Turn(t) => Some(t.stat.id.as_str()),
        }
    }
}
