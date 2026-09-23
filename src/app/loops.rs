//! The Loops section of the App: the registry, the scheduler pass on the
//! tick, pre-flight, launch composition, post-run accounting, the preview
//! cards and the add/edit dialog. Pure decisions live in `crate::loops`;
//! this file is where they meet sessions, the store and the files.

use super::dir_picker::PickerEvent;
use super::{App, Mode, Notice, SidebarSection};
use crate::config::Profile;
use crate::harness::Harness;
use crate::loops::registry::{self, LoopEntry};
use crate::loops::run::{self, Preflight, PreflightInput};
use crate::loops::scaffold::ContractFile;
use crate::loops::store::{self as lstore, LoopRun};
use crate::loops::worktree::{self, Worktree};
use crate::loops::{Level, LoopLaunch, LoopPolicy, Outcome, patterns};
use crate::status::Status;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use time::OffsetDateTime;

/// How long after the session exits the post-run accounting waits for the
/// store writer to flush the last turn.
const POST_RUN_SETTLE: Duration = Duration::from_millis(1200);
/// The readiness audit is cached per workspace this long.
const AUDIT_TTL: Duration = Duration::from_secs(60);
const CARD_REFRESH: Duration = Duration::from_millis(1000);
const SCHEDULE_EVERY: Duration = Duration::from_millis(1000);

/// A loop run that has a live (or just exited) session.
#[derive(Debug)]
pub struct LiveLoopRun {
    pub session_id: usize,
    pub loop_id: String,
    pub run_id: String,
    pub pattern: String,
    pub harness: String,
    pub launch_id: Option<String>,
    pub started: Instant,
    pub started_at: OffsetDateTime,
    pub configured_level: Level,
    pub effective_level: Level,
    pub level_reason: Option<String>,
    pub workspace: PathBuf,
    pub worktree: Option<Worktree>,
    /// The state file inside the run's cwd.
    pub state_path: PathBuf,
    pub state_before: Option<String>,
    pub context_path: PathBuf,
    pub timeout: Duration,
    pub readiness_score: Option<u32>,
    /// `Pattern::digest()` as the pattern stood when this run started;
    /// the library can edit it while the run is in flight.
    pub pattern_hash: Option<String>,
    pub exited_at: Option<Instant>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    Scheduled,
    Running,
    Paused,
    NeedsHuman,
    Failed,
}

impl LoopStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            LoopStatus::Scheduled => "○",
            LoopStatus::Running => "●",
            LoopStatus::Paused => "‖",
            LoopStatus::NeedsHuman => "!",
            LoopStatus::Failed => "✗",
        }
    }

    pub fn color(self) -> Color {
        match self {
            LoopStatus::Scheduled => Color::DarkGray,
            LoopStatus::Running => Color::Green,
            LoopStatus::Paused => Color::Yellow,
            LoopStatus::NeedsHuman => Color::Magenta,
            LoopStatus::Failed => Color::Red,
        }
    }
}

/// What the preview and the sidebar show for one loop.
#[derive(Debug, Clone)]
pub struct LoopCard {
    pub loop_id: String,
    pub status: LoopStatus,
    /// Seconds to the next run; negative when overdue.
    pub next_in_s: Option<i64>,
    pub last_run: Option<LoopRun>,
    pub recent: Vec<LoopRun>,
    pub runs_today: i64,
    pub tokens_today: i64,
    pub cost_today: f64,
    pub percent: u32,
    /// `None`: the pattern has no breaker.
    pub breaker: Option<String>,
    pub kill_switch_in_files: bool,
    pub inbox: usize,
    pub files: Vec<ContractFile>,
    pub store_error: Option<String>,
    pub ceiling: Level,
    /// What pre-flight would decide for a run now: blocked and why, or the
    /// level it would run at and why that is below the configured one.
    pub preflight: Option<Preflight>,
    /// The level above the configured one and what stands in its way;
    /// `None` at L3.
    pub step_up: Option<(Level, Vec<String>)>,
}

impl LoopCard {
    /// `(allowed, capped because)` for the "Allowed to" line: the level a
    /// run would get now, and why it is below `configured`.
    pub fn allowed(&self, configured: Level) -> (Level, Option<String>) {
        match &self.preflight {
            Some(Preflight::Go {
                effective_level,
                level_reason,
                ..
            }) => (*effective_level, level_reason.clone()),
            _ => (configured, None),
        }
    }

    /// Why the next run would not start, when it would not.
    pub fn blocked(&self) -> Option<&str> {
        match &self.preflight {
            Some(Preflight::Blocked { reason, .. }) => Some(reason.as_str()),
            _ => None,
        }
    }

    pub fn budget_mode(&self) -> &'static str {
        if self.percent >= 100 {
            "blocked"
        } else if self.percent >= 80 {
            "report-only"
        } else {
            "normal"
        }
    }

    /// The right-hand column of the sidebar row.
    pub fn right_label(&self) -> String {
        match self.status {
            LoopStatus::Running => "now".into(),
            LoopStatus::Paused => "—".into(),
            LoopStatus::NeedsHuman => self.inbox.to_string(),
            _ => match self.next_in_s {
                Some(s) if s <= 0 => "due".into(),
                Some(s) => short_duration(s as u64),
                None => "—".into(),
            },
        }
    }
}

/// `6h`, `12m`, `45s`, `3d`.
pub fn short_duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// `6 h 12 m`, `45 s`.
pub fn long_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} h {} m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{} m {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopField {
    Workspace,
    Pattern,
    Profile,
    /// The model the run's session uses. Blank leaves the profile's own.
    Model,
    /// The model the `loop-verifier` sub-agent uses. Blank means it
    /// inherits the run's, which is what the shipped agent file says.
    VerifierModel,
    Every,
    Level,
    MaxRuns,
    MaxTokens,
    MaxCost,
    Scaffold,
}

impl LoopField {
    pub const ALL: [LoopField; 11] = [
        LoopField::Workspace,
        LoopField::Pattern,
        LoopField::Profile,
        LoopField::Model,
        LoopField::VerifierModel,
        LoopField::Every,
        LoopField::Level,
        LoopField::MaxRuns,
        LoopField::MaxTokens,
        LoopField::MaxCost,
        LoopField::Scaffold,
    ];
}

/// The add / edit dialog.
#[derive(Debug, Clone)]
pub struct LoopDialogState {
    /// The loop being edited, if any.
    pub editing: Option<String>,
    pub field: LoopField,
    pub workspace: String,
    /// The subfolder list and search under the Workspace field: the same
    /// picker as the New session dialog's Directory.
    pub dir_picker: super::dir_picker::DirPicker,
    pub pattern_idx: usize,
    /// `(profile name, harness)` of the eligible profiles.
    pub profiles: Vec<(String, Harness)>,
    pub profile_idx: usize,
    pub model: String,
    pub verifier_model: String,
    pub every: String,
    pub level: Level,
    pub max_runs: String,
    pub max_tokens: String,
    pub max_cost: String,
    pub scaffold: bool,
    pub error: Option<String>,
    /// What the readiness audit says about the workspace and the levels.
    pub audit_note: String,
    pub level_notes: [Option<String>; 3],
    /// True when no profile runs Claude Code or Codex.
    pub no_profiles: bool,
}

impl LoopDialogState {
    pub fn new(profiles: &[Profile], workspaces: Vec<String>) -> Self {
        let eligible: Vec<(String, Harness)> = profiles
            .iter()
            .filter_map(|p| {
                Harness::detect(&p.command)
                    .filter(|h| matches!(h, Harness::Claude | Harness::Codex))
                    .map(|h| (p.name.clone(), h))
            })
            .collect();
        let no_profiles = eligible.is_empty();
        let pattern = patterns::find("daily-triage").or_else(|| patterns::all().first());
        let workspace = workspaces.into_iter().next().unwrap_or_default();
        let mut d = LoopDialogState {
            editing: None,
            field: LoopField::Workspace,
            dir_picker: super::dir_picker::DirPicker::for_path(&workspace),
            workspace,
            pattern_idx: patterns::all()
                .iter()
                .position(|p| p.id == "daily-triage")
                .unwrap_or(0),
            profiles: eligible,
            profile_idx: 0,
            model: String::new(),
            verifier_model: String::new(),
            every: String::new(),
            level: Level::L1,
            max_runs: String::new(),
            max_tokens: String::new(),
            max_cost: String::new(),
            scaffold: true,
            error: None,
            audit_note: String::new(),
            level_notes: [None, None, None],
            no_profiles,
        };
        if let Some(p) = pattern {
            d.apply_pattern_defaults(p);
        }
        d
    }

    pub fn from_entry(profiles: &[Profile], workspaces: Vec<String>, entry: &LoopEntry) -> Self {
        let mut d = Self::new(profiles, workspaces);
        d.editing = Some(entry.id.clone());
        d.workspace = entry.workspace.to_string_lossy().into_owned();
        d.dir_picker.refresh(&d.workspace);
        if let Some(i) = patterns::all().iter().position(|p| p.id == entry.pattern) {
            d.pattern_idx = i;
        }
        if let Some(i) = d
            .profiles
            .iter()
            .position(|(n, h)| n == &entry.profile || h.as_str() == entry.harness)
        {
            d.profile_idx = i;
        }
        d.model = entry.model.clone();
        d.verifier_model = entry.verifier_model.clone();
        d.every = crate::loops::format_interval(entry.interval_s);
        d.level = entry.level;
        d.max_runs = entry.max_runs_per_day.to_string();
        d.max_tokens = entry.max_tokens_per_day.to_string();
        d.max_cost = entry
            .max_cost_usd_per_run
            .map(|c| format!("{c}"))
            .unwrap_or_default();
        d.scaffold = false;
        d
    }

    pub fn pattern(&self) -> Option<&'static crate::loops::Pattern> {
        patterns::all().get(self.pattern_idx)
    }

    fn apply_pattern_defaults(&mut self, p: &crate::loops::Pattern) {
        self.every = crate::loops::format_interval(p.default_interval_s);
        self.max_runs = p.max_runs_per_day.to_string();
        self.max_tokens = p.max_tokens_per_day.to_string();
        // A pattern may suggest the models its work deserves; an edited
        // loop keeps what it already had.
        if self.editing.is_none() {
            self.model = p.model.clone().unwrap_or_default();
            self.verifier_model = p.verifier_model.clone().unwrap_or_default();
        }
    }

    pub fn harness(&self) -> Option<Harness> {
        self.profiles.get(self.profile_idx).map(|(_, h)| *h)
    }

    pub fn profile_name(&self) -> String {
        self.profiles
            .get(self.profile_idx)
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    }

    fn step_field(&mut self, delta: isize) {
        let at = LoopField::ALL
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0) as isize;
        let len = LoopField::ALL.len() as isize;
        self.field = LoopField::ALL[((at + delta).rem_euclid(len)) as usize];
    }

    fn cycle(&mut self, delta: isize) {
        match self.field {
            LoopField::Pattern => {
                let len = patterns::all().len() as isize;
                if len > 0 {
                    self.pattern_idx =
                        ((self.pattern_idx as isize + delta).rem_euclid(len)) as usize;
                    if let Some(p) = self.pattern() {
                        self.apply_pattern_defaults(p);
                    }
                }
            }
            LoopField::Profile => {
                let len = self.profiles.len() as isize;
                if len > 0 {
                    self.profile_idx =
                        ((self.profile_idx as isize + delta).rem_euclid(len)) as usize;
                }
            }
            LoopField::Level => {
                let levels = [Level::L1, Level::L2, Level::L3];
                let at = levels.iter().position(|l| *l == self.level).unwrap_or(0) as isize;
                self.level = levels[((at + delta).rem_euclid(3)) as usize];
            }
            LoopField::Scaffold => self.scaffold = !self.scaffold,
            _ => {}
        }
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            LoopField::Model => Some(&mut self.model),
            LoopField::VerifierModel => Some(&mut self.verifier_model),
            LoopField::Every => Some(&mut self.every),
            LoopField::MaxRuns => Some(&mut self.max_runs),
            LoopField::MaxTokens => Some(&mut self.max_tokens),
            LoopField::MaxCost => Some(&mut self.max_cost),
            _ => None,
        }
    }

    /// Validates the fields into an entry-shaped tuple.
    pub fn validate(&self) -> Result<ValidatedLoop, String> {
        let workspace = PathBuf::from(shellexpand_home(self.workspace.trim()));
        if self.workspace.trim().is_empty() || !workspace.is_dir() {
            return Err("workspace: not a directory".into());
        }
        let pattern = self.pattern().ok_or("pattern: none selected")?;
        let (profile, harness) = self
            .profiles
            .get(self.profile_idx)
            .cloned()
            .ok_or("profile: no Claude Code or Codex profile is configured")?;
        let interval_s = crate::loops::parse_interval(&self.every)
            .ok_or("every: use <n>m, <n>h or <n>d, at least 5m")?;
        let max_runs: u32 = self
            .max_runs
            .trim()
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or("runs/day: a positive number")?;
        let max_tokens: u64 = self
            .max_tokens
            .trim()
            .replace(['_', ','], "")
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or("tokens/day: a positive number")?;
        let max_cost = match self.max_cost.trim() {
            "" => None,
            t => Some(
                t.parse::<f64>()
                    .ok()
                    .filter(|c| c.is_finite() && *c > 0.0)
                    .ok_or("USD/run: a positive number or blank")?,
            ),
        };
        if let Some(note) = &self.level_notes[level_index(self.level)]
            && self.level > Level::L1
        {
            return Err(format!("{}: {note}", self.level.short()));
        }
        Ok(ValidatedLoop {
            workspace,
            pattern,
            profile,
            harness,
            model: self.model.trim().to_string(),
            verifier_model: self.verifier_model.trim().to_string(),
            interval_s,
            level: self.level,
            max_runs,
            max_tokens,
            max_cost,
            scaffold: self.scaffold,
        })
    }
}

fn level_index(level: Level) -> usize {
    match level {
        Level::L1 => 0,
        Level::L2 => 1,
        Level::L3 => 2,
    }
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        let home = crate::skill::install::home_dir();
        return home.join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

#[derive(Debug, Clone)]
pub struct ValidatedLoop {
    pub workspace: PathBuf,
    pub pattern: &'static crate::loops::Pattern,
    pub profile: String,
    pub harness: Harness,
    /// Blank: the profile's own model, then the CLI's default.
    pub model: String,
    /// Blank: the verifier inherits the run's model.
    pub verifier_model: String,
    pub interval_s: u64,
    pub level: Level,
    pub max_runs: u32,
    pub max_tokens: u64,
    pub max_cost: Option<f64>,
    pub scaffold: bool,
}

/// The per-harness invocation of a skill in the opening prompt.
fn invocation(skill: &str, harness: Harness) -> String {
    match harness {
        Harness::Claude | Harness::Antigravity => format!("/{skill}"),
        Harness::Codex => format!("${skill}"),
    }
}

impl App {
    // ----- registry -----------------------------------------------------

    /// Loads `~/.agent-mux/loops.json`, applies the catch-up rule and
    /// sweeps old context snapshots. Called once at startup.
    pub fn load_loop_registry(&mut self) {
        let path = self.loops_file.clone().or_else(registry::registry_path);
        if let Some(p) = &path {
            self.loop_registry = registry::load(p);
        }
        let moved = crate::loops::schedule::apply_catch_up(
            &mut self.loop_registry,
            self.loops.catch_up_once,
            crate::loops::now(),
        );
        if moved > 0 {
            let _ = self.save_loop_registry();
        }
        crate::loops::context::sweep(&self.loops_runtime_dir(), Duration::from_secs(24 * 3600));
        self.selected_loop = self
            .selected_loop
            .min(self.loop_registry.loops.len().saturating_sub(1));
        self.loops_loaded = true;
    }

    pub fn save_loop_registry(&self) -> std::io::Result<()> {
        let Some(path) = self.loops_file.clone().or_else(registry::registry_path) else {
            return Ok(());
        };
        registry::save(&path, &self.loop_registry)
    }

    pub fn loops_runtime_dir(&self) -> PathBuf {
        self.runtime_dir
            .clone()
            .unwrap_or_else(crate::tracing::analysis::default_snapshot_dir)
    }

    pub fn selected_loop(&self) -> Option<&LoopEntry> {
        self.loop_registry.loops.get(self.selected_loop)
    }

    /// The live run of a loop, if any.
    pub fn live_run_for(&self, loop_id: &str) -> Option<&LiveLoopRun> {
        self.live_loop_runs
            .iter()
            .find(|r| r.loop_id == loop_id && r.exited_at.is_none())
    }

    pub fn toggle_kill_switch(&mut self) {
        self.loop_registry.pause_all = !self.loop_registry.pause_all;
        let _ = self.save_loop_registry();
        self.notice = Some(Notice::warn(if self.loop_registry.pause_all {
            "LOOPS PAUSED — every loop stays idle until K is pressed again"
        } else {
            "loops resumed"
        }));
        self.refresh_loop_cards(Instant::now());
    }

    pub fn toggle_selected_loop_pause(&mut self) {
        let Some(entry) = self.selected_loop().cloned() else {
            return;
        };
        if entry.paused() {
            let reason = self.loop_registry.resume(&entry.id).flatten();
            if reason
                .as_deref()
                .is_some_and(|r| r.starts_with("circuit breaker"))
            {
                self.reset_breaker(&entry);
            }
            self.notice = Some(Notice::info(format!("{} resumed", entry.pattern)));
        } else {
            self.loop_registry.pause(&entry.id);
            self.notice = Some(Notice::info(format!("{} paused", entry.pattern)));
        }
        let _ = self.save_loop_registry();
        self.refresh_loop_cards(Instant::now());
    }

    fn reset_breaker(&mut self, entry: &LoopEntry) {
        let path = entry.workspace.join(crate::loops::LEDGER_JSON);
        if let Ok(mut ledger) = crate::loops::breaker::load(&path) {
            crate::loops::breaker::human_reset(&mut ledger);
            let _ = crate::loops::breaker::save(&path, &ledger);
        }
    }

    pub fn remove_selected_loop(&mut self) {
        let Some(entry) = self.selected_loop().cloned() else {
            return;
        };
        self.loop_registry.remove(&entry.id);
        self.loop_cards.remove(&entry.id);
        self.selected_loop = self
            .selected_loop
            .min(self.loop_registry.loops.len().saturating_sub(1));
        let _ = self.save_loop_registry();
        self.notice = Some(Notice::info(format!(
            "{} removed from the registry; the files in {} stay",
            entry.pattern,
            entry.workspace_name()
        )));
    }

    /// `r`: the next scheduler pass starts the loop (pre-flight still runs).
    pub fn run_selected_loop_now(&mut self) {
        let Some(entry) = self.selected_loop().cloned() else {
            return;
        };
        if self.live_run_for(&entry.id).is_some() {
            self.notice = Some(Notice::info(format!(
                "{} is already running",
                entry.pattern
            )));
            return;
        }
        let now = crate::loops::now();
        if let Some(l) = self.loop_registry.find_mut(&entry.id) {
            l.set_next_run(now);
        }
        let _ = self.save_loop_registry();
        if self.loop_registry.pause_all {
            self.notice = Some(Notice::warn("loops are paused (K to resume)"));
            return;
        }
        if entry.paused() {
            self.notice = Some(Notice::warn(format!(
                "{} is paused (p to resume)",
                entry.pattern
            )));
            return;
        }
        self.last_schedule_pass = None;
        self.scheduler_pass(Instant::now());
    }

    // ----- cards --------------------------------------------------------

    pub fn refresh_loop_cards_if_needed(&mut self, now: Instant) {
        if self.loop_registry.loops.is_empty() {
            return;
        }
        let due = self
            .last_loop_card_refresh
            .is_none_or(|t| now.saturating_duration_since(t) >= CARD_REFRESH);
        if due {
            self.refresh_loop_cards(now);
        }
    }

    pub fn refresh_loop_cards(&mut self, now: Instant) {
        self.last_loop_card_refresh = Some(now);
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let wall = crate::loops::now();
        let entries = self.loop_registry.loops.clone();
        let inbox: Vec<LoopRun> = conn
            .as_ref()
            .and_then(|c| lstore::inbox(c).ok())
            .unwrap_or_default();
        for entry in entries {
            let card = self.build_card(&entry, conn.as_ref(), &inbox, wall);
            self.loop_cards.insert(entry.id.clone(), card);
        }
    }

    fn audit_for(&mut self, workspace: &Path, now: Instant) -> crate::loops::readiness::Audit {
        if let Some((at, audit)) = self.loop_audits.get(workspace)
            && now.saturating_duration_since(*at) < AUDIT_TTL
        {
            return audit.clone();
        }
        let store_runs = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok())
            .map(|c| {
                let since = crate::loops::to_ns(crate::loops::now() - time::Duration::days(14));
                lstore::activity_count(&c, &workspace.to_string_lossy(), since).unwrap_or(0)
            })
            .unwrap_or(0);
        let audit = crate::loops::readiness::audit(workspace, store_runs);
        self.loop_audits
            .insert(workspace.to_path_buf(), (now, audit.clone()));
        audit
    }

    /// Forgets the cached audit of a workspace (after a scaffold or a run).
    fn invalidate_audit(&mut self, workspace: &Path) {
        self.loop_audits.remove(workspace);
    }

    fn build_card(
        &mut self,
        entry: &LoopEntry,
        conn: Option<&rusqlite::Connection>,
        inbox: &[LoopRun],
        wall: OffsetDateTime,
    ) -> LoopCard {
        let pattern = patterns::find(&entry.pattern);
        let (spend, recent, store_error) = match conn {
            Some(c) => {
                let since = crate::loops::to_ns(crate::loops::utc_midnight(wall));
                (
                    lstore::spend_since(c, &entry.id, since).unwrap_or_default(),
                    lstore::recent_runs(c, &entry.id, 5).unwrap_or_default(),
                    None,
                )
            }
            None => (
                Default::default(),
                Vec::new(),
                Some("tracing is off — no run history".to_string()),
            ),
        };
        let percent = if entry.max_tokens_per_day == 0 {
            0
        } else {
            ((spend.tokens.max(0) as u128 * 100) / entry.max_tokens_per_day as u128) as u32
        };
        let verdict = pattern.filter(|p| p.breaker).and_then(|_| {
            crate::loops::breaker::load(&entry.workspace.join(crate::loops::LEDGER_JSON))
                .ok()
                .map(|l| {
                    crate::loops::breaker::check(
                        &l,
                        &crate::loops::breaker::BreakerConfig::default(),
                    )
                })
        });
        let breaker = pattern.filter(|p| p.breaker).map(|_| match &verdict {
            Some(v) => {
                if v.tripped() {
                    format!("TRIPPED: {}", v.reason)
                } else if let Some(t) = v.near_trip {
                    format!(
                        "ok · {} attempts · one attempt from tripping ({})",
                        v.iterations,
                        t.as_str()
                    )
                } else {
                    format!("ok · {} attempts", v.iterations)
                }
            }
            None => "no ledger yet".to_string(),
        });
        let kill_switch_in_files = self.kill_switch_in_files(entry);
        let files = pattern
            .map(|p| crate::loops::scaffold::contract_files(&entry.workspace, p))
            .unwrap_or_default();
        let audit = entry
            .workspace
            .is_dir()
            .then(|| self.audit_for(&entry.workspace, Instant::now()));
        let inbox_count = inbox.iter().filter(|r| r.loop_id == entry.id).count();
        let last_run = recent.first().cloned();
        let running = self.live_run_for(&entry.id).is_some();
        let status = if running {
            LoopStatus::Running
        } else if self.loop_registry.pause_all || entry.paused() || kill_switch_in_files {
            LoopStatus::Paused
        } else if inbox_count > 0 {
            LoopStatus::NeedsHuman
        } else if last_run
            .as_ref()
            .is_some_and(|r| matches!(r.outcome, Outcome::Failed | Outcome::Blocked))
        {
            LoopStatus::Failed
        } else {
            LoopStatus::Scheduled
        };
        let next_in_s = entry.next_run().map(|t| (t - wall).whole_seconds());
        let profile = self.loop_profile(entry);
        let preflight = pattern.map(|p| {
            let mut plan = run::preflight(
                entry,
                &self.preflight_input(entry, p, &profile, &spend, audit.as_ref(), verdict.as_ref()),
            );
            // The run log keeps pre-flight's words; the card says what is
            // missing in the ones the Setup tab uses.
            if let Preflight::Go {
                level_reason: Some(why),
                ..
            } = &mut plan
                && why.starts_with("readiness:")
                && let Some(a) = &audit
            {
                let missing = a.missing_for(entry.level);
                if !missing.is_empty() {
                    *why = format!("needs {}", missing.join("; "));
                }
            }
            plan
        });
        let ceiling = self.harness_ceiling(entry);
        let step_up = entry.level.next().map(|up| {
            let mut missing = Vec::new();
            if up > ceiling {
                missing.push(match Harness::detect(&entry.harness) {
                    Some(Harness::Codex) => {
                        "a path guard: run `agent-mux trace hooks install codex`".to_string()
                    }
                    _ => "a path guard for this harness".to_string(),
                });
            }
            if !worktree::is_git_repo(&entry.workspace) {
                missing.push("a git repository for the worktree".into());
            }
            match &audit {
                Some(a) => missing.extend(a.missing_for(up)),
                None => missing.push("the workspace".into()),
            }
            (up, missing)
        });
        LoopCard {
            loop_id: entry.id.clone(),
            status,
            next_in_s,
            last_run,
            recent,
            runs_today: spend.runs,
            tokens_today: spend.tokens,
            cost_today: spend.cost_usd,
            percent,
            breaker,
            kill_switch_in_files,
            inbox: inbox_count,
            files,
            store_error,
            ceiling,
            preflight,
            step_up,
        }
    }

    fn kill_switch_in_files(&self, entry: &LoopEntry) -> bool {
        let state = patterns::state_file_for(&entry.pattern);
        [state, crate::loops::LOOP_MD].iter().any(|f| {
            std::fs::read_to_string(entry.workspace.join(f))
                .is_ok_and(|t| run::kill_switch_active(&t))
        })
    }

    /// Whether a path guard exists for the loop's harness (section 9.5).
    fn guard_available(&self, harness: Harness) -> bool {
        match harness {
            Harness::Claude => crate::tracing::hooks::register::current_exe().is_some(),
            Harness::Codex => {
                let exe = crate::tracing::hooks::register::current_exe();
                let st = crate::tracing::hooks::install::codex_status(
                    &self.skill_home(),
                    exe.as_deref(),
                );
                st.installed && !st.stale
            }
            Harness::Antigravity => false,
        }
    }

    pub fn harness_ceiling(&self, entry: &LoopEntry) -> Level {
        match Harness::detect(&entry.harness) {
            Some(h) if self.guard_available(h) => Level::L3,
            _ => Level::L1,
        }
    }

    // ----- scheduler ----------------------------------------------------

    /// Runs at most once a second: times out overdue runs, finalizes
    /// exited ones, then starts what is due.
    pub fn scheduler_pass(&mut self, now: Instant) {
        if !self.loops_loaded {
            return;
        }
        if self
            .last_schedule_pass
            .is_some_and(|t| now.saturating_duration_since(t) < SCHEDULE_EVERY)
        {
            return;
        }
        self.last_schedule_pass = Some(now);
        self.timeout_loop_runs(now);
        self.finalize_loop_runs(now);
        if !self.loops.enabled || self.loop_registry.loops.is_empty() {
            return;
        }
        let mut live = crate::loops::schedule::Live::default();
        for r in self.live_loop_runs.iter().filter(|r| r.exited_at.is_none()) {
            live.loop_ids.insert(r.loop_id.clone());
            live.workspaces.insert(r.workspace.clone());
        }
        let due = crate::loops::schedule::due(
            &self.loop_registry,
            &live,
            self.loops.max_concurrent as usize,
            crate::loops::now(),
        );
        for id in due {
            self.start_loop_run(&id);
        }
    }

    fn timeout_loop_runs(&mut self, now: Instant) {
        let overdue: Vec<usize> = self
            .live_loop_runs
            .iter()
            .filter(|r| {
                r.exited_at.is_none() && now.saturating_duration_since(r.started) > r.timeout
            })
            .map(|r| r.session_id)
            .collect();
        for sid in overdue {
            if let Some(i) = self.sessions.iter().position(|s| s.id == sid) {
                self.sessions[i].kill();
            }
            if let Some(r) = self.live_loop_runs.iter_mut().find(|r| r.session_id == sid) {
                r.timed_out = true;
                r.exited_at = Some(now);
            }
        }
    }

    /// Called from `handle_pty_exit`: the accounting waits for the writer.
    pub fn finish_loop_run_for_session(&mut self, session_id: usize) {
        if let Some(r) = self
            .live_loop_runs
            .iter_mut()
            .find(|r| r.session_id == session_id && r.exited_at.is_none())
        {
            r.exited_at = Some(Instant::now());
        }
    }

    fn finalize_loop_runs(&mut self, now: Instant) {
        let ready: Vec<usize> = self
            .live_loop_runs
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.exited_at
                    .is_some_and(|t| now.saturating_duration_since(t) >= POST_RUN_SETTLE)
            })
            .map(|(i, _)| i)
            .collect();
        for i in ready.into_iter().rev() {
            let live = self.live_loop_runs.remove(i);
            self.post_run(live);
        }
    }

    // ----- one run ------------------------------------------------------

    /// The facts pre-flight decides on, shared by a launch and the card
    /// (which shows what the next run would be allowed to do).
    fn preflight_input(
        &self,
        entry: &LoopEntry,
        pattern: &crate::loops::Pattern,
        profile: &Profile,
        spend: &lstore::Spend,
        audit: Option<&crate::loops::readiness::Audit>,
        breaker: Option<&crate::loops::breaker::Verdict>,
    ) -> PreflightInput {
        let harness = Harness::detect(&entry.harness);
        PreflightInput {
            pause_all: self.loop_registry.pause_all,
            kill_switch_in_files: self.kill_switch_in_files(entry),
            workspace_exists: entry.workspace.is_dir(),
            workspace_is_repo: worktree::is_git_repo(&entry.workspace),
            runs_today: spend.runs,
            tokens_today: spend.tokens,
            breaker_trip: breaker.filter(|v| v.tripped()).map(|v| v.reason.clone()),
            breaker_near_trip: breaker.and_then(|v| v.near_trip_reason()),
            audit_allows_configured: audit
                .map(|a| a.allows(entry.level))
                .unwrap_or(Err("no readiness audit".into())),
            audit_allows_l2: audit
                .map(|a| a.allows(Level::L2))
                .unwrap_or(Err("no readiness audit".into())),
            audit_score: audit.map(|a| a.score),
            state_stale: audit.is_some_and(|a| a.state_stale),
            guard_available: harness.is_some_and(|h| self.guard_available(h)),
            harness_resolves: harness.is_some()
                && crate::session::resolve_command(&profile.command).is_some(),
            skill_installed: harness.is_some_and(|h| {
                crate::loops::scaffold::project_skills_dir(h, &entry.workspace)
                    .map(|d| d.join(pattern.triage_skill()).join("SKILL.md").is_file())
                    .unwrap_or(false)
            }),
            concurrency_blocked: None,
        }
    }

    /// The whole of section 8: pre-flight, isolation, context, launch.
    pub fn start_loop_run(&mut self, loop_id: &str) -> Option<String> {
        let entry = self.loop_registry.find(loop_id).cloned()?;
        let Some(pattern) = patterns::find(&entry.pattern) else {
            self.notice = Some(Notice::error(format!("unknown pattern {}", entry.pattern)));
            return None;
        };
        let wall = crate::loops::now();
        let now = Instant::now();
        let harness = Harness::detect(&entry.harness);
        let profile = self.loop_profile(&entry);

        // facts for pre-flight
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let spend = conn
            .as_ref()
            .and_then(|c| {
                lstore::spend_since(
                    c,
                    &entry.id,
                    crate::loops::to_ns(crate::loops::utc_midnight(wall)),
                )
                .ok()
            })
            .unwrap_or_default();
        let breaker_verdict = if pattern.breaker {
            crate::loops::breaker::load(&entry.workspace.join(crate::loops::LEDGER_JSON))
                .ok()
                .map(|l| {
                    crate::loops::breaker::check(
                        &l,
                        &crate::loops::breaker::BreakerConfig::default(),
                    )
                })
        } else {
            None
        };
        let audit = if entry.workspace.is_dir() {
            Some(self.audit_for(&entry.workspace, now))
        } else {
            None
        };
        let input = self.preflight_input(
            &entry,
            pattern,
            &profile,
            &spend,
            audit.as_ref(),
            breaker_verdict.as_ref(),
        );
        let run_id = self.fresh_run_id(conn.as_ref(), wall);
        let mut row = LoopRun::new(
            &run_id,
            &entry.id,
            entry.workspace.to_string_lossy().into_owned(),
            &entry.pattern,
            &entry.harness,
            entry.level,
            crate::loops::to_ns(wall),
        );
        row.readiness_score = audit.as_ref().map(|a| i64::from(a.score));
        // Pin the pattern text this run executes: the library can replace
        // it before the next run of the same id.
        row.pattern_hash = Some(pattern.digest());
        drop(conn);

        match run::preflight(&entry, &input) {
            Preflight::Blocked { reason, pause } => {
                row.outcome = Outcome::Blocked;
                row.set_detail("reason", reason.clone().into());
                self.write_run_row(&row);
                if let Some(l) = self.loop_registry.find_mut(&entry.id) {
                    l.set_next_run(crate::loops::schedule::next_after(&entry, wall));
                    l.last_run_id = Some(run_id.clone());
                    if pause {
                        l.paused_reason = Some(reason.clone());
                    }
                }
                let _ = self.save_loop_registry();
                self.notice = Some(Notice::warn(format!("{} blocked: {reason}", entry.pattern)));
                self.refresh_loop_cards(now);
                None
            }
            Preflight::Go {
                effective_level,
                level_reason,
                budget_percent,
            } => {
                match self.launch_loop_run(
                    &entry,
                    pattern,
                    profile,
                    harness.unwrap_or(Harness::Claude),
                    row,
                    effective_level,
                    level_reason,
                    budget_percent,
                    audit.as_ref(),
                    spend,
                    wall,
                    now,
                ) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        self.notice = Some(Notice::error(format!(
                            "{} failed to start: {e}",
                            entry.pattern
                        )));
                        None
                    }
                }
            }
        }
    }

    fn fresh_run_id(&self, conn: Option<&rusqlite::Connection>, wall: OffsetDateTime) -> String {
        let base = crate::loops::format_timestamp(wall);
        let taken = |id: &str| {
            self.live_loop_runs.iter().any(|r| r.run_id == id)
                || conn.is_some_and(|c| lstore::get_run(c, id).ok().flatten().is_some())
        };
        if !taken(&base) {
            return base;
        }
        (2..100)
            .map(|n| format!("{base}-{n}"))
            .find(|id| !taken(id))
            .unwrap_or(base)
    }

    fn write_run_row(&mut self, row: &LoopRun) {
        let Some(db) = self.trace_db_path.clone() else {
            return;
        };
        match crate::tracing::store::open_aux(&db) {
            Ok(conn) => {
                if let Err(e) = lstore::upsert_run(&conn, row) {
                    self.notice = Some(Notice::error(format!("loop_runs: {e}")));
                }
            }
            Err(e) => self.notice = Some(Notice::error(format!("loop_runs: {e}"))),
        }
    }

    /// The profile a loop runs with: by name, else the first for its
    /// harness, else a bare command.
    fn loop_profile(&self, entry: &LoopEntry) -> Profile {
        let harness = Harness::detect(&entry.harness);
        self.profiles
            .iter()
            .find(|p| !entry.profile.is_empty() && p.name.eq_ignore_ascii_case(&entry.profile))
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|p| Harness::detect(&p.command) == harness)
            })
            .cloned()
            .unwrap_or_else(|| Profile {
                name: entry.harness.clone(),
                command: entry.harness.clone(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_loop_run(
        &mut self,
        entry: &LoopEntry,
        pattern: &crate::loops::Pattern,
        mut profile: Profile,
        harness: Harness,
        mut row: LoopRun,
        effective_level: Level,
        level_reason: Option<String>,
        budget_percent: u32,
        audit: Option<&crate::loops::readiness::Audit>,
        spend: lstore::Spend,
        wall: OffsetDateTime,
        now: Instant,
    ) -> Result<String, String> {
        let run_id = row.id.clone();
        let created = crate::loops::format_timestamp(wall);
        // isolation
        let worktree = if effective_level > Level::L1 {
            Some(worktree::create(
                &entry.workspace,
                &self.loops.worktrees_dir,
                &run_id,
                &entry.pattern,
                &created,
            )?)
        } else {
            None
        };
        if let Some(w) = &worktree {
            // the scaffolded skills are usually untracked, so a fresh
            // worktree lacks them; copy them in (ignored by change checks)
            worktree::seed_loop_files(&entry.workspace, &w.path);
        }
        let cwd = worktree
            .as_ref()
            .map(|w| w.path.clone())
            .unwrap_or_else(|| entry.workspace.clone());
        let state_file = pattern.state_file.clone();
        // the state file, run log and ledger are the loop's memory: they
        // live in the workspace whatever directory the run executes in
        let state_path = entry.workspace.join(&state_file);
        let state_before = std::fs::read_to_string(&state_path).ok();
        let abs = |name: &str| entry.workspace.join(name).to_string_lossy().into_owned();
        let gate = crate::loops::gate::load(&entry.workspace.join(crate::loops::GATE_YAML))
            .unwrap_or_else(|_| crate::loops::gate::default_config());
        let ledger_name = pattern
            .breaker
            .then(|| crate::loops::LEDGER_JSON.to_string());
        let breaker = if pattern.breaker {
            match crate::loops::breaker::load(&entry.workspace.join(crate::loops::LEDGER_JSON)) {
                Ok(l) => {
                    let v = crate::loops::breaker::check(&l, &Default::default());
                    crate::loops::context::Breaker {
                        applicable: true,
                        status: Some(if v.tripped() {
                            "tripped".into()
                        } else {
                            "ok".into()
                        }),
                        reason: Some(v.reason),
                        near_trip: v.near_trip.map(|t| t.as_str().to_string()),
                        iterations: v.iterations,
                        consecutive_failures: l
                            .attempts
                            .iter()
                            .rev()
                            .take_while(|a| {
                                a.outcome == crate::loops::breaker::AttemptOutcome::Failure
                            })
                            .count(),
                    }
                }
                Err(_) => crate::loops::context::Breaker {
                    applicable: true,
                    status: Some("no ledger".into()),
                    ..Default::default()
                },
            }
        } else {
            crate::loops::context::Breaker::default()
        };
        // recent runs and inbox for the context
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let recent: Vec<LoopRun> = conn
            .as_ref()
            .and_then(|c| lstore::recent_runs(c, &entry.id, 5).ok())
            .unwrap_or_default();
        let inbox_waiting = conn
            .as_ref()
            .and_then(|c| lstore::inbox(c).ok())
            .map(|v| v.len())
            .unwrap_or(0);
        drop(conn);
        let summary = |r: &LoopRun| crate::loops::context::RunSummary {
            id: r.id.clone(),
            outcome: r.outcome.as_str().into(),
            items_found: r.items_found,
            actions_taken: r.actions_taken,
            escalations: r.escalations,
            tokens: r.tokens,
        };
        let doc = crate::loops::context::ContextDoc {
            schema_version: crate::loops::context::SCHEMA_VERSION,
            as_of: created.clone(),
            run: crate::loops::context::RunInfo {
                id: run_id.clone(),
                pattern: entry.pattern.clone(),
                level_configured: entry.level,
                level_effective: effective_level,
                level_reason: level_reason.clone(),
            },
            workspace: entry.workspace.to_string_lossy().into_owned(),
            worktree: worktree
                .as_ref()
                .map(|w| crate::loops::context::WorktreeInfo {
                    path: w.path.to_string_lossy().into_owned(),
                    branch: w.branch.clone(),
                    base: w.base.clone(),
                }),
            files: crate::loops::context::Files {
                state: abs(&state_file),
                run_log: abs(crate::loops::RUN_LOG_MD),
                constraints: abs(crate::loops::CONSTRAINTS_MD),
                ledger: ledger_name.as_deref().map(abs),
                gate: abs(crate::loops::GATE_YAML),
            },
            budget: crate::loops::context::Budget {
                runs_today: spend.runs,
                max_runs_per_day: entry.max_runs_per_day,
                tokens_today: spend.tokens,
                max_tokens_per_day: entry.max_tokens_per_day,
                percent: budget_percent,
                mode: if (effective_level == Level::L1 && entry.level > Level::L1)
                    || budget_percent >= 80
                {
                    "report-only".into()
                } else {
                    "normal".into()
                },
            },
            breaker,
            gate: crate::loops::context::Gate {
                denylist: gate.denylist.clone(),
                max_files: gate.max_files,
                auto_merge_allowlist: gate.auto_merge_allowlist.clone(),
            },
            readiness: audit.map(|a| crate::loops::context::Readiness {
                score: a.score,
                level: a.level_str().into(),
                findings: a
                    .findings
                    .iter()
                    .filter(|f| f.level != crate::loops::readiness::FindingLevel::Ok)
                    .take(5)
                    .map(|f| format!("{} {}", f.level.glyph(), f.message))
                    .collect(),
            }),
            previous_run: recent.first().map(summary),
            recent_runs: recent.iter().map(summary).collect(),
            inbox_waiting,
            kill_switch: false,
            human_gates: pattern.human_gates.clone(),
        };
        let runtime_dir = self.loops_runtime_dir();
        let context_path = crate::loops::context::write(&runtime_dir, &run_id, &doc)
            .map_err(|e| format!("context snapshot: {e}"))?;

        // the command line
        if let Some(cap) = entry.max_cost_usd_per_run {
            let t = profile.tracing.get_or_insert_with(Default::default);
            t.max_cost_usd = Some(cap);
        }
        // the opening prompt: the pattern's own, else `[loop] run` of the
        // library's prompts.toml, else the built-in text
        let template = match &pattern.prompt {
            Some(p) if !p.trim().is_empty() => p.clone(),
            _ => crate::prompts::Prompts::current().loop_run,
        };
        let prompt = crate::prompts::render_loop_run(
            &template,
            &crate::prompts::LoopVars {
                invocation: &invocation(pattern.triage_skill(), harness),
                pattern: &entry.pattern,
                state_file: &state_path.display().to_string(),
                workspace: &entry.workspace.display().to_string(),
                level: effective_level.as_str(),
                harness: harness.as_str(),
            },
        );
        // A print-mode run has nobody to answer an approval prompt, so the
        // harness's own prompts are bypassed; the PreToolUse guard (gate,
        // report-only, no push) and the worktree are the controls.
        let options = crate::harness::LaunchOptions {
            // the loop's own model wins over the profile's: a loop is a
            // standing job, and what it costs per run is part of it
            model: Some(entry.model.clone())
                .filter(|m| !m.trim().is_empty())
                .or_else(|| profile.model.clone()),
            bypass_approvals: true,
            resume: crate::harness::Resume::Off,
            one_shot: Some(prompt),
        };
        let mut args = crate::harness::compose(&profile.args, &options.render(harness));
        match harness {
            Harness::Claude => {
                if let Some(cap) = entry.max_cost_usd_per_run {
                    args.insert(args.len() - 2, "--max-budget-usd".into());
                    args.insert(args.len() - 2, format!("{cap}"));
                }
            }
            Harness::Codex => {}
            Harness::Antigravity => return Err("Antigravity is not supported for loops".into()),
        }
        if Harness::detect(&profile.command) != Some(harness) {
            profile.command = harness.as_str().to_string();
        }
        profile.args = args;
        profile.name = format!("{} ↻ {}", entry.pattern, entry.workspace_name());

        // MCP registration and environment
        let mut extra_args: Vec<String> = Vec::new();
        let registration = self.ensure_mcp(harness.as_str(), &entry.workspace);
        if let crate::mcp::register::Registration::PerLaunch { args } = &registration {
            extra_args.extend(args.iter().cloned());
        }
        let env: Vec<(String, String)> = vec![
            ("AGENT_MUX_LOOP_ID".into(), entry.id.clone()),
            ("AGENT_MUX_LOOP_RUN_ID".into(), run_id.clone()),
            ("AGENT_MUX_LOOP_PATTERN".into(), entry.pattern.clone()),
            (
                "AGENT_MUX_LOOP_LEVEL".into(),
                effective_level.as_str().into(),
            ),
            (
                "AGENT_MUX_LOOP_CONTEXT".into(),
                context_path.to_string_lossy().into_owned(),
            ),
            (
                "AGENT_MUX_LOOP_STATE".into(),
                state_path.to_string_lossy().into_owned(),
            ),
            (
                "AGENT_MUX_LOOP_WORKSPACE".into(),
                entry.workspace.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_MCP".into(), registration.env_value().into()),
            (
                "AGENT_MUX_WORKSPACE".into(),
                entry.workspace.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_SKILL_ID".into(), pattern.triage_skill().into()),
        ];
        let launch = LoopLaunch {
            loop_id: entry.id.clone(),
            run_id: run_id.clone(),
            pattern: entry.pattern.clone(),
            level: effective_level,
            workspace: entry.workspace.to_string_lossy().into_owned(),
            policy: LoopPolicy {
                report_only: effective_level == Level::L1,
                reason: level_reason.clone().or_else(|| {
                    (effective_level == Level::L1).then(|| "level L1 is report-only".to_string())
                }),
                state_file: state_file.clone(),
                run_log: crate::loops::RUN_LOG_MD.into(),
                denylist: gate.denylist.clone(),
                max_files: gate.max_files,
                worktree: worktree
                    .as_ref()
                    .map(|w| w.path.to_string_lossy().into_owned()),
            },
        };

        let id = self.next_id;
        let session = match self.spawn_traced_full(
            id,
            profile,
            cwd,
            &env,
            Some(pattern.triage_skill()),
            Some(launch),
            None,
            &extra_args,
        ) {
            Ok(s) => s,
            Err(e) => {
                if let Some(w) = &worktree {
                    let _ = worktree::remove(
                        &entry.workspace,
                        &self.loops.worktrees_dir,
                        w,
                        true,
                        None,
                        &run_id,
                    );
                }
                let _ = std::fs::remove_file(&context_path);
                row.outcome = Outcome::Failed;
                row.set_detail("reason", format!("spawn: {e}").into());
                self.write_run_row(&row);
                return Err(e.to_string());
            }
        };
        let launch_id = session.trace.as_ref().map(|t| t.launch_id.clone());
        self.next_id += 1;
        self.sessions.push(session);
        let _ = self.save_active_sessions();

        row.launch_id = launch_id.clone();
        row.started_ns = Some(crate::loops::to_ns(wall));
        row.effective_level = effective_level;
        row.worktree = worktree
            .as_ref()
            .map(|w| w.path.to_string_lossy().into_owned());
        row.branch = worktree.as_ref().map(|w| w.branch.clone());
        if let Some(r) = &level_reason {
            row.set_detail("level_reason", r.clone().into());
        }
        self.write_run_row(&row);

        if let Some(l) = self.loop_registry.find_mut(&entry.id) {
            l.set_next_run(crate::loops::schedule::next_after(entry, wall));
            l.last_run_id = Some(run_id.clone());
        }
        let _ = self.save_loop_registry();

        self.live_loop_runs.push(LiveLoopRun {
            session_id: id,
            loop_id: entry.id.clone(),
            run_id: run_id.clone(),
            pattern: entry.pattern.clone(),
            harness: entry.harness.clone(),
            launch_id,
            started: now,
            started_at: wall,
            configured_level: entry.level,
            effective_level,
            level_reason,
            workspace: entry.workspace.clone(),
            worktree,
            state_path,
            state_before,
            context_path,
            timeout: Duration::from_secs(self.loops.run_timeout_s),
            readiness_score: audit.map(|a| a.score),
            pattern_hash: Some(pattern.digest()),
            exited_at: None,
            timed_out: false,
        });
        self.notice = Some(Notice::info(format!(
            "{} started ({}{})",
            entry.pattern,
            effective_level.as_str(),
            if effective_level < entry.level {
                " capped"
            } else {
                ""
            }
        )));
        self.refresh_loop_cards(now);
        Ok(run_id)
    }

    /// Section 8.6: facts, outcome, gate re-check, store, run log, ledger,
    /// registry, worktree.
    fn post_run(&mut self, live: LiveLoopRun) {
        let wall = crate::loops::now();
        let Some(entry) = self.loop_registry.find(&live.loop_id).cloned() else {
            self.cleanup_orphan_run(&live);
            return;
        };
        let pattern = patterns::find(&live.pattern);
        let exit_code = self
            .sessions
            .iter()
            .find(|s| s.id == live.session_id)
            .and_then(|s| match s.status(Instant::now()) {
                Status::Exited(code) => code,
                _ => None,
            });
        let screen_text = self
            .sessions
            .iter_mut()
            .find(|s| s.id == live.session_id)
            .map(|s| s.text_dump())
            .unwrap_or_default();
        let missing_skill = run::skill_missing(&screen_text);
        let conn_ro = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let facts = match (&conn_ro, &live.launch_id) {
            (Some(c), Some(l)) => lstore::run_facts(c, l).ok(),
            _ => None,
        };
        let mut row = conn_ro
            .as_ref()
            .and_then(|c| lstore::get_run(c, &live.run_id).ok().flatten())
            .unwrap_or_else(|| {
                LoopRun::new(
                    &live.run_id,
                    &live.loop_id,
                    live.workspace.to_string_lossy().into_owned(),
                    &live.pattern,
                    &live.harness,
                    live.configured_level,
                    crate::loops::to_ns(live.started_at),
                )
            });
        drop(conn_ro);

        let state_after = std::fs::read_to_string(&live.state_path).ok();
        let state_changed = state_after != live.state_before;
        let high_grew = match (&live.state_before, &state_after) {
            (Some(b), Some(a)) => run::high_priority_items(a) > run::high_priority_items(b),
            (None, Some(a)) => run::high_priority_items(a) > 0,
            _ => false,
        };
        let worktree_changed = live.worktree.as_ref().is_some_and(worktree::has_changes);
        let final_message = facts.as_ref().and_then(|f| f.final_message.clone());
        let result = final_message.as_deref().and_then(run::parse_loop_result);
        let permission_refused = facts.as_ref().is_some_and(|f| f.files_touched.is_empty())
            && final_message.as_deref().is_some_and(|m| {
                m.to_ascii_lowercase().contains("permission")
                    && m.to_ascii_lowercase().contains("denied")
            })
            && !state_changed;
        let observed = run::Observed {
            exit_code,
            timed_out: live.timed_out,
            worktree_changed,
            state_changed,
            high_priority_grew: high_grew,
            verifier_verdicts: facts
                .as_ref()
                .map(|f| f.verifier_verdicts.clone())
                .unwrap_or_default(),
            permission_refused,
            skill_missing: missing_skill.is_some(),
        };
        let mut outcome = run::derive_outcome(result.as_ref(), &observed);

        // gate re-check over everything the run touched
        let gate = crate::loops::gate::load(&live.workspace.join(crate::loops::GATE_YAML))
            .unwrap_or_else(|_| crate::loops::gate::default_config());
        let mut touched: Vec<String> = facts
            .as_ref()
            .map(|f| f.files_touched.clone())
            .unwrap_or_default();
        if let Some(w) = &live.worktree {
            touched.extend(worktree::changed_files(w));
        }
        let root = live
            .worktree
            .as_ref()
            .map(|w| w.path.clone())
            .unwrap_or_else(|| live.workspace.clone());
        let relative: Vec<String> = touched
            .iter()
            .map(|p| crate::loops::gate::relative_to(&root, p))
            .collect();
        let violation = relative
            .iter()
            .find_map(|p| gate.denied(p).map(|g| format!("{p} matches {g}")));
        let mut pause_reason: Option<String> = None;
        if let Some(v) = &violation {
            outcome = Outcome::Escalated;
            row.set_detail("gate_violation", v.clone().into());
            pause_reason = Some(format!("gate violation: {v}"));
        }
        let verifier_missing = live.effective_level > Level::L1
            && outcome == Outcome::FixProposed
            && !facts.as_ref().is_some_and(|f| f.verifier_ran);

        // the row
        row.outcome = outcome;
        row.effective_level = live.effective_level;
        row.launch_id = live.launch_id.clone();
        row.started_ns = Some(crate::loops::to_ns(live.started_at));
        row.ended_ns = Some(crate::loops::to_ns(wall));
        row.tokens = facts.as_ref().and_then(|f| f.tokens);
        row.cost_usd = facts.as_ref().and_then(|f| f.cost_usd);
        row.readiness_score = live.readiness_score.map(i64::from);
        row.pattern_hash = live.pattern_hash.clone();
        if let Some(r) = &result {
            row.items_found = Some(r.items_found as i64);
            row.actions_taken = Some(r.actions_taken as i64);
            row.escalations = Some(r.escalations as i64);
            if !r.summary.is_empty() {
                row.set_detail("summary", r.summary.clone().into());
            }
        }
        if let Some(f) = &facts {
            row.set_detail(
                "verifier",
                serde_json::json!({
                    "ran": f.verifier_ran,
                    "verdict": f.verifier_verdict,
                    "verdicts": f.verifier_verdicts,
                    "label": run::verdict_label(&f.verifier_verdicts),
                }),
            );
        }
        row.set_detail("files", serde_json::json!(relative));
        if let Some(c) = exit_code {
            row.set_detail("exit_code", c.into());
        }
        if live.timed_out {
            row.set_detail("timed_out", true.into());
            pause_reason.get_or_insert_with(|| "run timed out".into());
        }
        if verifier_missing {
            row.set_detail("verifier_missing", true.into());
        }
        if let Some(r) = &live.level_reason {
            row.set_detail("level_reason", r.clone().into());
        }
        if let Some(m) = &final_message {
            row.set_detail(
                "final_message",
                m.chars().take(2000).collect::<String>().into(),
            );
        }
        if let Some(name) = &missing_skill {
            let why = format!(
                "the harness did not find {name}: the loop's skill is missing from the run's directory"
            );
            row.set_detail("reason", why.clone().into());
            pause_reason.get_or_insert(why);
        }
        if outcome == Outcome::Failed {
            pause_reason.get_or_insert_with(|| "last run failed".into());
        }

        // worktree bookkeeping
        if let Some(w) = &live.worktree {
            if outcome.needs_human() && worktree_changed {
                row.worktree = Some(w.path.to_string_lossy().into_owned());
                row.branch = Some(w.branch.clone());
                let _ = worktree::update_manifest(
                    &live.workspace.join(&self.loops.worktrees_dir),
                    |m| {
                        for e in m.worktrees.iter_mut().filter(|e| e.id == live.run_id) {
                            e.status = if outcome == Outcome::Escalated {
                                "escalated".into()
                            } else {
                                "active".into()
                            };
                        }
                    },
                );
                row.set_detail("diff_stat", worktree::diff_stat(w).into());
            } else {
                let _ = worktree::remove(
                    &live.workspace,
                    &self.loops.worktrees_dir,
                    w,
                    true,
                    None,
                    &live.run_id,
                );
                row.worktree = None;
                row.branch = None;
            }
        }
        // The report this run wrote. The state file is rewritten in place
        // every run, so a copy is kept per run: it is what the Report tab
        // of an older run shows, and what the next run's "since last run"
        // line is computed against.
        if let Some(p) = patterns::find(&live.pattern) {
            let path = live.workspace.join(&p.state_file);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let runtime = self.loops_runtime_dir();
                let cur = crate::loops::state::parse(&text);
                if let Some(prev) =
                    crate::loops::state::previous_snapshot(&runtime, &live.loop_id, &live.run_id)
                {
                    let prev = crate::loops::state::parse(&prev);
                    let delta = crate::loops::state::delta(&prev, &cur);
                    if delta.is_empty() {
                        row.set_detail("quiet", true.into());
                    } else if let Ok(v) = serde_json::to_value(&delta) {
                        row.set_detail("delta", v);
                    }
                }
                if crate::loops::state::write_snapshot(&runtime, &live.loop_id, &live.run_id, &text)
                    .is_ok()
                {
                    row.set_detail("state_snapshot", true.into());
                }
            }
        }
        self.write_run_row(&row);

        // run log and ledger in the workspace
        let entry_line = run::runlog_entry(&row, row.readiness_score, live.launch_id.as_deref());
        let _ = crate::loops::runlog::append(
            &live.workspace.join(crate::loops::RUN_LOG_MD),
            &entry_line,
            wall,
        );
        if pattern.is_some_and(|p| p.breaker) {
            let path = live.workspace.join(crate::loops::LEDGER_JSON);
            let mut ledger = crate::loops::breaker::load(&path).unwrap_or_else(|_| {
                crate::loops::breaker::seed(
                    pattern.map(|p| p.goal.as_str()).unwrap_or(""),
                    &live.pattern,
                    live.configured_level.as_str(),
                )
            });
            let (att, err) = match outcome {
                Outcome::FixProposed => (crate::loops::breaker::AttemptOutcome::Success, None),
                Outcome::Failed | Outcome::Escalated
                    if violation.is_some() || outcome == Outcome::Failed =>
                {
                    (
                        crate::loops::breaker::AttemptOutcome::Failure,
                        Some(
                            violation
                                .clone()
                                .or_else(|| {
                                    facts
                                        .as_ref()
                                        .and_then(|f| f.verifier_verdict.clone())
                                        .map(|v| format!("verifier: {v}"))
                                })
                                .unwrap_or_else(|| "run failed".into()),
                        ),
                    )
                }
                _ => (crate::loops::breaker::AttemptOutcome::Noop, None),
            };
            crate::loops::breaker::append(
                &mut ledger,
                &live.pattern,
                att,
                err,
                row.tokens.map(|t| t.max(0) as u64),
            );
            crate::loops::breaker::prune(&mut ledger, 5, 8);
            let _ = crate::loops::breaker::save(&path, &ledger);
        }
        let _ = std::fs::remove_file(&live.context_path);

        // registry
        if let Some(l) = self.loop_registry.find_mut(&entry.id) {
            l.last_run_id = Some(live.run_id.clone());
            if let Some(r) = pause_reason.clone() {
                l.paused_reason = Some(r);
            }
        }
        let _ = self.save_loop_registry();
        self.invalidate_audit(&live.workspace);
        // The notice says what the run found and who has to act, in the
        // words the Report tab uses. A quiet run posts nothing: on a
        // fifteen-minute loop a notice per run is noise.
        let what = row
            .detail_str("summary")
            .map(|s| s.chars().take(90).collect::<String>())
            .or_else(|| run_delta_summary(&row))
            .unwrap_or_else(|| outcome.word().to_string());
        let quiet = row.detail.get("quiet").is_some() || outcome == Outcome::NoOp;
        self.notice = match (&pause_reason, outcome) {
            (Some(r), _) => Some(Notice::warn(format!("{} paused: {r}", live.pattern))),
            (None, Outcome::FixProposed) => Some(Notice::info(format!(
                "{}: fix ready on {} · {what} · E to decide",
                live.pattern,
                row.branch.as_deref().unwrap_or("its worktree")
            ))),
            (None, Outcome::Escalated) => Some(Notice::warn(format!(
                "{}: {} need you · {what} · E to read",
                live.pattern,
                row.escalations.unwrap_or(0).max(1)
            ))),
            (None, _) if quiet => None,
            (None, _) => Some(Notice::info(format!(
                "{}: {what} · E to read",
                live.pattern
            ))),
        };
        self.refresh_loop_cards(Instant::now());
    }

    fn cleanup_orphan_run(&mut self, live: &LiveLoopRun) {
        if let Some(w) = &live.worktree {
            let _ = worktree::remove(
                &live.workspace,
                &self.loops.worktrees_dir,
                w,
                true,
                None,
                &live.run_id,
            );
        }
        let _ = std::fs::remove_file(&live.context_path);
    }

    /// The inbox decision: `applied` keeps the branch and removes the
    /// worktree; `rejected` removes both.
    pub fn decide_loop_run(&mut self, run_id: &str, applied: bool) -> Result<(), String> {
        let db = self.trace_db_path.clone().ok_or("tracing is off")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let run = lstore::get_run(&conn, run_id)
            .map_err(|e| e.to_string())?
            .ok_or("run not found")?;
        if !run.in_inbox() {
            return Err("that run is not waiting on a decision".into());
        }
        let workspace = PathBuf::from(&run.workspace);
        if let (Some(path), Some(branch)) = (&run.worktree, &run.branch) {
            let wt = Worktree {
                path: PathBuf::from(path),
                branch: branch.clone(),
                base: worktree::lookup(&workspace, &self.loops.worktrees_dir, run_id)
                    .map(|w| w.base)
                    .unwrap_or_else(|| "main".into()),
            };
            if wt.path.exists() {
                worktree::remove(
                    &workspace,
                    &self.loops.worktrees_dir,
                    &wt,
                    !applied,
                    Some(if applied { "merged" } else { "rejected" }),
                    run_id,
                )?;
            }
        }
        lstore::decide(
            &conn,
            run_id,
            if applied { "applied" } else { "rejected" },
            crate::tracing::store::now_ns(),
        )
        .map_err(|e| e.to_string())?;
        self.notice = Some(Notice::info(if applied {
            format!(
                "{run_id} applied: merge {} yourself; the worktree is gone",
                run.branch.as_deref().unwrap_or("the branch")
            )
        } else {
            format!("{run_id} rejected: worktree and branch removed")
        }));
        self.refresh_loop_cards(Instant::now());
        Ok(())
    }

    // ----- dialog -------------------------------------------------------

    /// Known directories, most relevant first: open sessions, the current
    /// directory, profile defaults, past sessions, registered loops. The
    /// first one seeds the Workspace field; the picker navigates from there.
    fn workspace_choices(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |p: &Path| {
            let s = p.to_string_lossy().into_owned();
            if p.is_dir() && !out.contains(&s) {
                out.push(s);
            }
        };
        for s in &self.sessions {
            push(&s.dir);
        }
        if let Ok(cwd) = std::env::current_dir() {
            push(&cwd);
        }
        for p in &self.profiles {
            if let Some(d) = &p.default_dir {
                push(Path::new(&shellexpand_home(d)));
            }
        }
        for h in &self.history_sessions {
            if let Some(c) = &h.cwd {
                push(c);
            }
        }
        for l in &self.loop_registry.loops {
            push(&l.workspace);
        }
        out
    }

    pub fn open_loop_dialog(&mut self, edit: Option<String>) {
        let choices = self.workspace_choices();
        let mut dialog = match edit
            .as_deref()
            .and_then(|id| self.loop_registry.find(id).cloned())
        {
            Some(entry) => LoopDialogState::from_entry(&self.profiles, choices, &entry),
            None => LoopDialogState::new(&self.profiles, choices),
        };
        self.refresh_dialog_audit(&mut dialog);
        self.mode = Mode::NewLoop(Box::new(dialog));
    }

    fn refresh_dialog_audit(&mut self, dialog: &mut LoopDialogState) {
        let ws = PathBuf::from(shellexpand_home(dialog.workspace.trim()));
        if !ws.is_dir() {
            dialog.audit_note = "workspace: not a directory".into();
            dialog.level_notes = [
                None,
                Some("no workspace".into()),
                Some("no workspace".into()),
            ];
            return;
        }
        let audit = self.audit_for(&ws, Instant::now());
        let ceiling = dialog
            .harness()
            .map(|h| {
                if self.guard_available(h) {
                    Level::L3
                } else {
                    Level::L1
                }
            })
            .unwrap_or(Level::L1);
        let is_repo = worktree::is_git_repo(&ws);
        let mut note = format!("readiness {}/100", audit.score);
        if ceiling == Level::L1 {
            note.push_str(" · this harness has no path guard, so runs only report");
        } else if !is_repo {
            note.push_str(" · not a git repository, so runs only report");
        }
        dialog.audit_note = note;
        let note = |level: Level| -> Option<String> {
            let mut missing = Vec::new();
            if level > ceiling {
                missing.push("a path guard for this harness".to_string());
            }
            if level > Level::L1 && !is_repo {
                missing.push("a git repository".into());
            }
            missing.extend(audit.missing_for(level));
            (!missing.is_empty()).then(|| format!("needs {}", missing.join("; ")))
        };
        dialog.level_notes = [None, note(Level::L2), note(Level::L3)];
    }

    pub fn handle_loop_dialog_key(&mut self, key: &KeyEvent) {
        let Mode::NewLoop(dialog) = &mut self.mode else {
            return;
        };
        let mut refresh = false;
        // The Workspace field is the shared directory picker: it gets the
        // key first and the dialog only sees what it does not take.
        if dialog.field == LoopField::Workspace {
            let LoopDialogState {
                dir_picker,
                workspace,
                ..
            } = &mut **dialog;
            match dir_picker.handle_key(key, workspace) {
                PickerEvent::Submit => {
                    let d = (**dialog).clone();
                    self.confirm_loop_dialog(d);
                    return;
                }
                PickerEvent::Consumed { path_changed } => {
                    dialog.error = None;
                    if path_changed {
                        self.refresh_loop_dialog_audit();
                    }
                    return;
                }
                PickerEvent::Ignored => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Control;
                return;
            }
            KeyCode::Enter => {
                let d = (**dialog).clone();
                self.confirm_loop_dialog(d);
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                dialog.step_field(1);
                dialog.dir_picker.leave();
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.step_field(-1);
                dialog.dir_picker.leave();
            }
            KeyCode::Left => {
                dialog.cycle(-1);
                refresh = matches!(dialog.field, LoopField::Profile | LoopField::Pattern);
            }
            KeyCode::Right | KeyCode::Char(' ')
                if !matches!(
                    dialog.field,
                    LoopField::Every
                        | LoopField::MaxRuns
                        | LoopField::MaxTokens
                        | LoopField::MaxCost
                ) || key.code == KeyCode::Right =>
            {
                dialog.cycle(1);
                refresh = matches!(dialog.field, LoopField::Profile | LoopField::Pattern);
            }
            KeyCode::Backspace => {
                if let Some(t) = dialog.text_mut() {
                    t.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(t) = dialog.text_mut() {
                    t.push(c);
                }
            }
            _ => {}
        }
        dialog.error = None;
        if refresh {
            self.refresh_loop_dialog_audit();
        }
    }

    /// Re-runs the readiness audit for the open dialog's workspace and
    /// profile and writes the notes back into it.
    fn refresh_loop_dialog_audit(&mut self) {
        let Mode::NewLoop(dialog) = &self.mode else {
            return;
        };
        let mut d = (**dialog).clone();
        self.refresh_dialog_audit(&mut d);
        if let Mode::NewLoop(dialog) = &mut self.mode {
            dialog.audit_note = d.audit_note;
            dialog.level_notes = d.level_notes;
        }
    }

    fn confirm_loop_dialog(&mut self, dialog: LoopDialogState) {
        let v = match dialog.validate() {
            Ok(v) => v,
            Err(e) => {
                if let Mode::NewLoop(d) = &mut self.mode {
                    d.error = Some(e);
                }
                return;
            }
        };
        let now = crate::loops::now();
        if v.scaffold {
            let caps = crate::loops::scaffold::Caps {
                max_runs_per_day: v.max_runs,
                max_tokens_per_day: v.max_tokens,
                verifier_model: v.verifier_model.clone(),
            };
            match crate::loops::scaffold::scaffold(
                &v.workspace,
                v.pattern,
                v.harness,
                v.level,
                &caps,
            ) {
                Ok(report) => {
                    self.invalidate_audit(&v.workspace);
                    self.notice = Some(Notice::info(format!(
                        "scaffolded {} file(s) in {}, {} kept",
                        report.written.len(),
                        v.workspace.display(),
                        report.skipped.len()
                    )));
                }
                Err(e) => {
                    if let Mode::NewLoop(d) = &mut self.mode {
                        d.error = Some(format!("scaffold: {e}"));
                    }
                    return;
                }
            }
        }
        let mut entry = match dialog
            .editing
            .as_deref()
            .and_then(|id| self.loop_registry.find(id).cloned())
        {
            Some(mut e) => {
                e.workspace = v.workspace.clone();
                e.pattern = v.pattern.id.clone();
                e.profile = v.profile.clone();
                e.harness = v.harness.as_str().to_string();
                if e.interval_s != v.interval_s {
                    e.interval_s = v.interval_s;
                    e.set_next_run(now + time::Duration::seconds(v.interval_s as i64));
                }
                e
            }
            None => registry::new_entry(
                &v.workspace,
                v.pattern,
                v.harness.as_str(),
                &v.profile,
                v.interval_s,
                v.level,
                now,
            ),
        };
        entry.level = v.level;
        entry.model = v.model.clone();
        entry.verifier_model = v.verifier_model.clone();
        entry.max_runs_per_day = v.max_runs;
        entry.max_tokens_per_day = v.max_tokens;
        entry.max_cost_usd_per_run = v.max_cost;
        let id = entry.id.clone();
        self.loop_registry.add(entry);
        if let Err(e) = self.save_loop_registry() {
            self.notice = Some(Notice::error(format!("loops.json: {e}")));
        }
        self.selected_loop = self
            .loop_registry
            .loops
            .iter()
            .position(|l| l.id == id)
            .unwrap_or(0);
        self.sidebar_section = SidebarSection::Loops;
        self.mode = Mode::Control;
        let audit = self.audit_for(&v.workspace, Instant::now());
        if self.notice.is_none() || dialog.editing.is_some() {
            self.notice = Some(Notice::info(format!(
                "{} every {} at {} · readiness {}/100 {}",
                v.pattern.id,
                crate::loops::format_interval(v.interval_s),
                v.level.as_str(),
                audit.score,
                audit.level_str()
            )));
        }
        self.refresh_loop_cards(Instant::now());
    }

    // ----- view ---------------------------------------------------------

    pub fn open_loops_view(&mut self) {
        let selected = self.selected_loop().map(|l| l.id.clone());
        let runtime = self.loops_runtime_dir();
        self.refresh_loop_cards(Instant::now());
        let mut view = super::loops_view::LoopsViewState::new(
            self.trace_db_path.as_deref(),
            Some(runtime.as_path()),
            &self.loop_registry,
            selected.as_deref(),
            &self.loops.worktrees_dir,
        );
        view.cards = self.loop_cards.clone();
        view.rebuild_detail();
        self.mode = Mode::LoopsView(Box::new(view));
    }

    pub fn handle_loops_view_key(&mut self, key: &KeyEvent) {
        use super::loops_view::{LoopsPane, LoopsTab};
        let Mode::LoopsView(view) = &mut self.mode else {
            return;
        };
        let live: Vec<(String, usize)> = self
            .live_loop_runs
            .iter()
            .filter(|r| r.exited_at.is_none())
            .map(|r| (r.loop_id.clone(), r.session_id))
            .collect();
        view.live = live;
        view.cards = self.loop_cards.clone();
        let page = view.viewport_rows.get().max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if view.focus == LoopsPane::Detail {
                    view.focus = LoopsPane::Loops;
                } else {
                    self.mode = Mode::Control;
                }
            }
            KeyCode::Tab => view.next_tab(),
            KeyCode::BackTab => view.prev_tab(),
            KeyCode::Right => view.focus = LoopsPane::Detail,
            KeyCode::Left => view.focus = LoopsPane::Loops,
            KeyCode::Down | KeyCode::Char('j') => match view.focus {
                LoopsPane::Loops => view.step(1),
                LoopsPane::Detail => view.step_detail(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match view.focus {
                LoopsPane::Loops => view.step(-1),
                LoopsPane::Detail => view.step_detail(-1),
            },
            KeyCode::PageDown => view.step_detail(page),
            KeyCode::PageUp => view.step_detail(-page),
            KeyCode::Char('1') => {
                view.tab = LoopsTab::Report;
                view.rebuild_detail();
            }
            KeyCode::Char('2') => {
                view.tab = LoopsTab::History;
                view.rebuild_detail();
            }
            KeyCode::Char('3') => {
                view.tab = LoopsTab::Setup;
                view.rebuild_detail();
            }
            KeyCode::Char('R') => {
                let reg = self.loop_registry.clone();
                if let Mode::LoopsView(view) = &mut self.mode {
                    view.reload(&reg);
                }
            }
            KeyCode::Char('r') => {
                let id = view.selected_loop().map(|l| l.id.clone());
                if let Some(id) = id
                    && let Some(i) = self.loop_registry.loops.iter().position(|l| l.id == id)
                {
                    self.selected_loop = i;
                    self.run_selected_loop_now();
                    let reg = self.loop_registry.clone();
                    if let Mode::LoopsView(view) = &mut self.mode {
                        view.reload(&reg);
                    }
                }
            }
            KeyCode::Char('p') => {
                let id = view.selected_loop().map(|l| l.id.clone());
                if let Some(id) = id
                    && let Some(i) = self.loop_registry.loops.iter().position(|l| l.id == id)
                {
                    self.selected_loop = i;
                    self.toggle_selected_loop_pause();
                    let reg = self.loop_registry.clone();
                    if let Mode::LoopsView(view) = &mut self.mode {
                        view.reload(&reg);
                    }
                }
            }
            KeyCode::Char('T') => {
                let launch = view.selected_run().and_then(|r| r.launch_id.clone());
                let Some(launch_id) = launch else {
                    self.notice = Some(Notice::info(
                        "select a run with a launch to open its traces",
                    ));
                    return;
                };
                self.open_trace_browser_for_launch(&launch_id);
            }
            KeyCode::Enter => match view.focus {
                LoopsPane::Loops => view.focus = LoopsPane::Detail,
                LoopsPane::Detail => {
                    if view.tab == LoopsTab::History {
                        let live = view
                            .selected_loop()
                            .and_then(|l| view.live_session(&l.id))
                            .zip(view.selected_run().map(|r| r.id.clone()));
                        let is_live_run = view.selected_run().is_some_and(|r| r.ended_ns.is_none())
                            && live.is_some();
                        if is_live_run
                            && let Some((sid, _)) = live
                            && let Some(idx) = self.sessions.iter().position(|s| s.id == sid)
                        {
                            self.selected = idx;
                            self.sidebar_section = SidebarSection::Active;
                            self.mode = Mode::Attached;
                            if let Some(s) = self.sessions.get_mut(idx) {
                                s.tracker.on_attach();
                            }
                            return;
                        }
                        let launch = view.selected_run().and_then(|r| r.launch_id.clone());
                        if let Some(l) = launch {
                            self.open_trace_browser_for_launch(&l);
                        }
                    }
                }
            },
            _ => {}
        }
    }

    /// Opens the Trace Browser on the session of a launch.
    pub(crate) fn open_trace_browser_for_launch(&mut self, launch_id: &str) {
        let session_key: Option<String> = self.trace_db_path.as_deref().and_then(|db| {
            let conn = crate::tracing::store::open_ro(db).ok()?;
            conn.query_row(
                "SELECT session_key FROM launches WHERE id = ?1",
                [launch_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
        });
        let Some(key) = session_key else {
            self.notice = Some(Notice::info("that launch has no session in the store yet"));
            return;
        };
        let cur_dir = std::env::current_dir().ok();
        let langfuse = self.tracing.as_ref().and_then(|rt| rt.langfuse().cloned());
        let mut browser =
            super::TraceBrowserState::new(self.trace_db_path.as_deref(), cur_dir.as_deref())
                .with_langfuse(langfuse);
        browser.focus_session(&key, None);
        self.mode = Mode::TraceBrowser(Box::new(browser));
    }

    /// The row of the sidebar for `entry`: glyph, colour, right label.
    pub fn loop_row(&self, entry: &LoopEntry) -> (LoopStatus, String) {
        match self.loop_cards.get(&entry.id) {
            Some(card) => (card.status, card.right_label()),
            None => {
                let status = if self.live_run_for(&entry.id).is_some() {
                    LoopStatus::Running
                } else if self.loop_registry.pause_all || entry.paused() {
                    LoopStatus::Paused
                } else {
                    LoopStatus::Scheduled
                };
                let right = entry
                    .next_run()
                    .map(|t| {
                        let s = (t - crate::loops::now()).whole_seconds();
                        if s <= 0 {
                            "due".to_string()
                        } else {
                            short_duration(s as u64)
                        }
                    })
                    .unwrap_or_else(|| "—".into());
                (status, right)
            }
        }
    }
}

/// Audits cached per workspace on the App.
pub type AuditCache = HashMap<PathBuf, (Instant, crate::loops::readiness::Audit)>;

/// The one-line changelog a run stored, for the notice and the card.
pub fn run_delta_summary(row: &LoopRun) -> Option<String> {
    let d: crate::loops::state::Delta =
        serde_json::from_value(row.detail.get("delta")?.clone()).ok()?;
    (!d.is_empty()).then(|| d.summary())
}
