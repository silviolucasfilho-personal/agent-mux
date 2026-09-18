use crate::config::Profile;
use crate::events::AppEvent;
use crate::history::{self, SessionSummary};
use crate::keys::encode_key_with_mode;
use crate::mouse::{WheelRoute, encode_mouse, route_wheel};
use crate::persistence;
use crate::search::SearchState;
use crate::selection::{self, Pos, Selection};
use crate::session::Session;
use crate::status::Status;
use crate::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;
use tokio::sync::mpsc::Sender;

pub mod about;
mod config_view;
pub mod dir_picker;
pub use config_view::*;
pub mod loops;
pub mod loops_view;
mod skills_view;
pub mod text_area;
pub mod workflows;
pub mod workflows_view;
pub use skills_view::*;

#[derive(Debug)]
pub enum Mode {
    Control,
    Attached,
    NewSession(DialogState),
    SessionHistory(HistoryState),
    TraceBrowser(Box<TraceBrowserState>),
    ConfirmKill,
    ConfirmQuit,
    Help,
    SkillsView(Box<SkillsViewState>),
    SkillLauncher(SkillLauncherState),
    /// The Loops view (`E`): runs, inbox, readiness, budget, files.
    LoopsView(Box<loops_view::LoopsViewState>),
    /// The add / edit loop dialog (`a` / `e` in the Loops section).
    NewLoop(Box<loops::LoopDialogState>),
    /// `x` on a loop: remove the registry entry (files stay).
    ConfirmRemoveLoop,
    /// The About overlay (`v`): version, build stamp, paths, this session.
    About(Box<about::AboutState>),
    /// The Configuration view (`C`): every prompt, skill, loop and agent.
    ConfigView(Box<ConfigViewState>),
    /// The run / compose dialog of the Workflows section.
    WorkflowDialog(Box<workflows_view::WorkflowDialogState>),
    /// The Workflows view (`W`): runs, planned documents, results.
    WorkflowsView(Box<workflows_view::WorkflowsViewState>),
}

/// An external editor the main loop must run for the App: it leaves the
/// alternate screen, runs `command` with `path` appended, comes back and
/// calls `App::editor_finished`. Recorded rather than run here so the App
/// stays testable without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRequest {
    pub path: std::path::PathBuf,
    /// The catalog id being edited, for the reload afterwards.
    pub asset_id: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone)]
struct SkillWorkbenchBookmark {
    id: String,
    harness: crate::harness::Harness,
    tab: SkillsTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarSection {
    #[default]
    Active,
    Agents,
    Loops,
    Workflows,
    History,
}

#[derive(Debug)]
pub enum Action {
    None,
    Quit,
    EnterConfirmQuit,
    MoveUp,
    MoveDown,
    /// Jump straight to session N (the `1`-`9` keys).
    SelectSession(usize),
    Attach,
    Detach,
    OpenNewSession,
    OpenSessionHistory,
    OpenTraceBrowser,
    OpenHelp,
    OpenSkillsView,
    OpenSkillLauncher,
    KillSelected,
    EnterConfirmKill,
    RemoveSelected,
    RemoveExited,
    RespawnSelected,
    ToggleTracing,
    ToggleSidebar,
    ToggleSidebarSection,
    RestartHistorySession,
    ToggleHistoryAllProjects,
    CancelToControl,
    ForwardBytes(Vec<u8>),
    SendLiteralDetachKey,
    /// NewSession mode: App routes the key to the DialogState it owns.
    DialogKey,
    /// SessionHistory mode: App routes the key to the HistoryState it owns.
    HistoryKey,
    /// TraceBrowser mode: App routes the key to the TraceBrowserState it owns.
    BrowserKey,
    /// SkillsView mode: App routes the key to the SkillsViewState it owns.
    SkillsKey,
    /// SkillLauncher mode: App routes the key to the SkillLauncherState it owns.
    SkillLauncherKey,
    /// Loops section and view.
    OpenLoopsView,
    LoopRunNow,
    LoopTogglePause,
    OpenNewLoop,
    EditLoop,
    EnterConfirmRemoveLoop,
    ToggleKillSwitch,
    OpenAbout,
    /// About mode: App routes the key to the AboutState it owns.
    AboutKey,
    /// LoopsView mode: App routes the key to the LoopsViewState it owns.
    LoopsKey,
    /// NewLoop mode: App routes the key to the LoopDialogState it owns.
    LoopDialogKey,
    /// `C`: the Configuration view.
    OpenConfigView,
    /// ConfigView mode: App routes the key to the ConfigViewState it owns.
    ConfigKey,
    /// Workflows section: Enter (run dialog or the view), c (compose), e, x.
    OpenWorkflowRun,
    OpenWorkflowPlan,
    EditWorkflow,
    CancelWorkflow,
    /// `W`: the Workflows view.
    OpenWorkflowsView,
    WorkflowsKey,
    WorkflowDialogKey,
}

/// Severity of a status-bar notice. The old single `error: Option<String>`
/// channel painted "tracing started" the same red as a PTY write failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// One transient status-bar message; cleared on the next keypress.
#[derive(Debug, Clone)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Notice {
            level: NoticeLevel::Info,
            text: text.into(),
        }
    }
    pub fn warn(text: impl Into<String>) -> Self {
        Notice {
            level: NoticeLevel::Warn,
            text: text.into(),
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Notice {
            level: NoticeLevel::Error,
            text: text.into(),
        }
    }
}

/// Codex normally owns an alternate screen, which leaves a containing
/// terminal multiplexer with no transcript history to scroll. Inside
/// agent-mux the containing terminal is our vt100 buffer, so interactive
/// Codex sessions must use inline mode. `codex exec` is non-interactive and
/// does not accept or need this TUI flag.
fn prepare_nested_tui(profile: &mut Profile) {
    let configured_provider = profile.tracing.as_ref().and_then(|t| t.provider.as_deref());
    let is_codex = configured_provider == Some("codex")
        || crate::harness::Harness::detect(&profile.command)
            == Some(crate::harness::Harness::Codex);
    let is_exec = profile.args.first().is_some_and(|arg| arg == "exec");
    if is_codex && !is_exec && !profile.args.iter().any(|arg| arg == "--no-alt-screen") {
        profile.args.push("--no-alt-screen".into());
    }
}

/// State for the skill harness picker dialog.
#[derive(Debug, Clone)]
pub struct SkillLauncherState {
    pub selected: usize,
    pub error: Option<String>,
    pub skill_id: String,
    pub skill_name: String,
    pub harnesses: Vec<crate::harness::Harness>,
}
impl Default for SkillLauncherState {
    fn default() -> Self {
        Self {
            selected: 0,
            error: None,
            skill_id: String::new(),
            skill_name: String::new(),
            harnesses: crate::harness::Harness::ALL.to_vec(),
        }
    }
}
impl SkillLauncherState {
    pub fn for_skill(skill: &crate::skill::SkillDefinition) -> Self {
        let harnesses = if skill.harnesses.is_empty() {
            crate::harness::Harness::ALL.to_vec()
        } else {
            skill.harnesses.clone()
        };
        let default_idx = crate::skill::default_harness_index(skill);
        Self {
            selected: if default_idx < harnesses.len() {
                default_idx
            } else {
                0
            },
            error: None,
            skill_id: skill.id.clone(),
            skill_name: skill.name.clone(),
            harnesses,
        }
    }

    pub fn with_harnesses(
        id: impl Into<String>,
        name: impl Into<String>,
        harnesses: Vec<crate::harness::Harness>,
    ) -> Self {
        let h = if harnesses.is_empty() {
            crate::harness::Harness::ALL.to_vec()
        } else {
            harnesses
        };
        Self {
            selected: 0,
            error: None,
            skill_id: id.into(),
            skill_name: name.into(),
            harnesses: h,
        }
    }

    pub fn selected_harness(&self) -> crate::harness::Harness {
        self.harnesses
            .get(self.selected)
            .copied()
            .unwrap_or(crate::harness::Harness::Claude)
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.harnesses.len() {
            self.selected += 1;
        }
    }
}

/// What `prepare_agent_launch` adds to a spawn.
#[derive(Debug, Default)]
struct AgentLaunchPrep {
    args: Vec<String>,
    env: Vec<(String, String)>,
    briefing_path: Option<std::path::PathBuf>,
}

pub struct DispatchCtx {
    pub selected_status: Option<Status>,
    pub any_working: bool,
    pub just_detached: bool,
    pub app_cursor: bool,
    pub sidebar_section: SidebarSection,
    pub sidebar_hidden: bool,
}

fn is_ctrl_q(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
}

/// Bytes to write for a paste: wrapped in bracketed-paste markers when the
/// child enabled bracketed paste, otherwise newlines normalized to CR so a
/// multi-line paste presses Enter instead of inserting raw LFs.
///
/// In the bracketed branch, any literal `ESC[201~` (the paste-end marker)
/// already present in the clipboard text is stripped first. Otherwise
/// clipboard content containing that sequence would terminate the bracket
/// early and the remainder would land in the child as raw keystrokes
/// instead of pasted text -- a classic paste-injection: e.g. a copied
/// snippet ending the bracket then typing `rm -rf ~` as if the user had.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let sanitized = text.replace("\x1b[201~", "");
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(sanitized.as_bytes());
        b.extend_from_slice(b"\x1b[201~");
        b
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod paste_tests {
    use super::paste_bytes;

    #[test]
    fn bracketed_paste_wraps_verbatim() {
        assert_eq!(
            paste_bytes("a\r\nb\nc", true),
            b"\x1b[200~a\r\nb\nc\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_paste_strips_embedded_terminator() {
        // an embedded terminator must not end the bracket early; it's
        // stripped, and the wrapper still appears exactly once, at the end
        let text = "before\x1b[201~after";
        assert_eq!(
            paste_bytes(text, true),
            b"\x1b[200~beforeafter\x1b[201~".to_vec()
        );
    }

    #[test]
    fn unbracketed_paste_normalizes_newlines_to_cr() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(paste_bytes("plain", false), b"plain".to_vec());
    }

    #[test]
    fn unbracketed_paste_leaves_terminator_look_alike_untouched() {
        // the unbracketed path never wraps in bracket markers at all, so an
        // embedded ESC[201~ isn't a terminator here and is passed through
        let text = "before\x1b[201~after";
        assert_eq!(paste_bytes(text, false), text.as_bytes().to_vec());
    }
}

pub fn dispatch(mode: &Mode, key: &KeyEvent, ctx: &DispatchCtx) -> Action {
    match mode {
        Mode::Control => {
            // Handle Ctrl+Q before the key.code match: matching on code alone
            // would let Ctrl+Q fall into the Char('q') arm and quit the app.
            if is_ctrl_q(key) {
                return if ctx.just_detached {
                    Action::SendLiteralDetachKey
                } else {
                    Action::None
                };
            }
            match key.code {
                _ if !ctx.sidebar_hidden
                    && ctx.sidebar_section == SidebarSection::Workflows
                    && App::workflows_section_action(key).is_some() =>
                {
                    App::workflows_section_action(key).unwrap_or(Action::None)
                }
                KeyCode::Char('b') | KeyCode::Char('B') => Action::ToggleSidebar,
                KeyCode::Tab | KeyCode::BackTab => Action::ToggleSidebarSection,
                KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                KeyCode::Char(c @ '1'..='9')
                    if ctx.sidebar_hidden || ctx.sidebar_section == SidebarSection::Active =>
                {
                    Action::SelectSession(c as usize - '1' as usize)
                }
                KeyCode::Enter => {
                    if ctx.sidebar_hidden {
                        if ctx.selected_status.is_some() {
                            Action::Attach
                        } else {
                            Action::None
                        }
                    } else {
                        match ctx.sidebar_section {
                            SidebarSection::Active if ctx.selected_status.is_some() => {
                                Action::Attach
                            }
                            SidebarSection::Agents => Action::OpenSkillLauncher,
                            SidebarSection::Loops => Action::OpenLoopsView,
                            SidebarSection::History => Action::RestartHistorySession,
                            _ => Action::None,
                        }
                    }
                }
                KeyCode::Char('r') => {
                    if ctx.sidebar_hidden {
                        match ctx.selected_status {
                            Some(Status::Exited(_)) => Action::RespawnSelected,
                            _ => Action::None,
                        }
                    } else {
                        match ctx.sidebar_section {
                            SidebarSection::Active => match ctx.selected_status {
                                Some(Status::Exited(_)) => Action::RespawnSelected,
                                _ => Action::None,
                            },
                            SidebarSection::Agents => Action::OpenSkillLauncher,
                            SidebarSection::Loops => Action::LoopRunNow,
                            SidebarSection::Workflows => Action::OpenWorkflowRun,
                            SidebarSection::History => Action::RestartHistorySession,
                        }
                    }
                }
                KeyCode::Char('p')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::Loops =>
                {
                    Action::LoopTogglePause
                }
                KeyCode::Char('a')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::Loops =>
                {
                    Action::OpenNewLoop
                }
                KeyCode::Char('e')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::Loops =>
                {
                    Action::EditLoop
                }
                KeyCode::Char('x')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::Loops =>
                {
                    Action::EnterConfirmRemoveLoop
                }
                KeyCode::Char('W') => Action::OpenWorkflowsView,
                KeyCode::Char('v') | KeyCode::Char('V') => Action::OpenAbout,
                KeyCode::Char('K') => Action::ToggleKillSwitch,
                KeyCode::Char('E') => Action::OpenLoopsView,
                KeyCode::Char('h') | KeyCode::Char('H')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::Agents =>
                {
                    Action::OpenSkillLauncher
                }
                KeyCode::Char('a') | KeyCode::Char('A')
                    if !ctx.sidebar_hidden && ctx.sidebar_section == SidebarSection::History =>
                {
                    Action::ToggleHistoryAllProjects
                }
                KeyCode::Char('n') => Action::OpenNewSession,
                KeyCode::Char('l') | KeyCode::Char('L') => Action::OpenSessionHistory,
                KeyCode::Char('t')
                    if ctx.sidebar_hidden || ctx.sidebar_section == SidebarSection::Active =>
                {
                    Action::ToggleTracing
                }
                KeyCode::Char('T') => Action::OpenTraceBrowser,
                KeyCode::Char('S') => Action::OpenSkillsView,
                KeyCode::Char('C') => Action::OpenConfigView,
                KeyCode::Char('?') | KeyCode::F(1) => Action::OpenHelp,
                KeyCode::Char('x')
                    if ctx.sidebar_hidden || ctx.sidebar_section == SidebarSection::Active =>
                {
                    match ctx.selected_status {
                        Some(Status::Exited(_)) => Action::RemoveSelected,
                        Some(_) => Action::EnterConfirmKill,
                        None => Action::None,
                    }
                }
                KeyCode::Char('X')
                    if ctx.sidebar_hidden || ctx.sidebar_section == SidebarSection::Active =>
                {
                    Action::RemoveExited
                }
                KeyCode::Char('q') => {
                    if ctx.any_working {
                        Action::EnterConfirmQuit
                    } else {
                        Action::Quit
                    }
                }
                _ => Action::None,
            }
        }
        Mode::Attached => {
            if is_ctrl_q(key) {
                Action::Detach
            } else {
                match encode_key_with_mode(key, ctx.app_cursor) {
                    Some(bytes) => Action::ForwardBytes(bytes),
                    None => Action::None,
                }
            }
        }
        Mode::NewSession(_) => Action::DialogKey,
        Mode::SessionHistory(_) => Action::HistoryKey,
        Mode::TraceBrowser(_) => Action::BrowserKey,
        Mode::SkillsView(_) => Action::SkillsKey,
        Mode::SkillLauncher(_) => Action::SkillLauncherKey,
        Mode::About(_) => match key.code {
            KeyCode::Char('?') | KeyCode::F(1) => Action::OpenHelp,
            KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('v')
            | KeyCode::Char('V')
            | KeyCode::Enter => Action::CancelToControl,
            _ => Action::AboutKey,
        },
        Mode::LoopsView(_) => Action::LoopsKey,
        Mode::NewLoop(_) => Action::LoopDialogKey,
        Mode::ConfigView(_) => Action::ConfigKey,
        Mode::WorkflowDialog(_) => Action::WorkflowDialogKey,
        Mode::WorkflowsView(_) => Action::WorkflowsKey,
        Mode::ConfirmRemoveLoop => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                Action::EnterConfirmRemoveLoop
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelToControl,
            _ => Action::None,
        },
        Mode::ConfirmKill => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::KillSelected,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelToControl,
            _ => Action::None,
        },
        Mode::ConfirmQuit => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Action::Quit,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelToControl,
            _ => Action::None,
        },
        Mode::Help => match key.code {
            KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('?')
            | KeyCode::Enter
            | KeyCode::F(1) => Action::CancelToControl,
            _ => Action::None,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogContentMode {
    Full,
    Metadata,
}

impl std::fmt::Display for DialogContentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialogContentMode::Full => write!(f, "full"),
            DialogContentMode::Metadata => write!(f, "metadata"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogField {
    Profile,
    Dir,
    Tracing,
    Backend,
    ContentMode,
    /// `--model <id>`; blank leaves the CLI its own default.
    Model,
    /// Skip the CLI's approval prompts.
    Approvals,
    /// Pick up the most recent conversation.
    Resume,
    /// A prompt to run non-interactively; blank launches interactively.
    OneShot,
    /// Name of an experiment to record this launch under; blank = none.
    Experiment,
    /// The variant label within that experiment; blank = "interactive".
    Variant,
    /// Budget guard: refuse tool calls past this spend (USD); blank = none.
    MaxCost,
    /// Budget guard: refuse tool calls past this many turns; blank = none.
    MaxTurns,
}

#[derive(Debug)]
pub enum DialogResult {
    Submit,
    Cancel,
    Consumed,
}

/// Resolves a user-provided or profile working directory path:
/// - empty / whitespace / `.` -> current working directory
/// - `~/...` or `~` -> user's home directory joined with the subpath
/// - other paths -> parsed as PathBuf directly
pub fn resolve_working_dir(dir_str: &str) -> std::path::PathBuf {
    let trimmed = dir_str.trim();
    if trimmed.is_empty() || trimmed == "." {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    } else if let Some(stripped) = trimmed.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            std::path::PathBuf::from(home).join(stripped)
        } else {
            std::path::PathBuf::from(trimmed)
        }
    } else if trimmed == "~" {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            std::path::PathBuf::from(home)
        } else {
            std::path::PathBuf::from(trimmed)
        }
    } else {
        std::path::PathBuf::from(trimmed)
    }
}

/// Lists subdirectories of a given directory, including ".." if a parent exists.
pub fn list_subdirectories(dir: &std::path::Path) -> Vec<String> {
    let mut entries = Vec::new();
    if dir.parent().is_some() {
        entries.push("..".to_string());
    }
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        let mut subdirs: Vec<String> = read_dir
            .filter_map(|e| e.ok())
            .filter(|e| {
                let file_name = e.file_name();
                let name = file_name.to_string_lossy();
                !name.starts_with('.') && e.path().is_dir()
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        subdirs.sort_by_key(|a| a.to_lowercase());
        entries.extend(subdirs);
    }
    entries
}

fn default_dir_for_profile(profile: Option<&Profile>) -> String {
    profile
        .and_then(|p| p.default_dir.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".into())
        })
}

#[derive(Debug)]
pub struct DialogState {
    pub profile_idx: usize,
    pub dir: String,
    pub dir_edited: bool,
    pub field: DialogField,
    pub error: Option<String>,
    /// The subfolder list and search under the Directory field.
    pub dir_picker: dir_picker::DirPicker,
    pub tracing_enabled: bool,
    pub content_mode: DialogContentMode,
    /// Where this launch's traces go.
    pub backend: crate::config::Backend,
    /// The CLI this profile runs, when it is one we know how to pass
    /// options to. `None` hides the four option fields.
    pub harness: Option<crate::harness::Harness>,
    /// `--model <id>`; blank means the flag is not passed at all.
    pub model: String,
    pub bypass_approvals: bool,
    pub resume_last: bool,
    /// A one-shot prompt; blank launches interactively.
    pub one_shot: String,
    /// False when no Langfuse credentials resolved: the field cannot leave
    /// local and says why.
    pub langfuse_available: bool,
    /// Experiment link for this launch; blank experiment = not linked.
    pub experiment: String,
    pub variant: String,
    /// True when a trace store is open to record the link into.
    pub experiments_available: bool,
    /// Budget guard for this launch, as typed; blank = no limit.
    pub max_cost: String,
    pub max_turns: String,
}

impl DialogState {
    pub fn new(profiles: &[Profile]) -> Self {
        let first = profiles.first();
        let dir = default_dir_for_profile(first);
        let dir_picker = dir_picker::DirPicker::for_path(&dir);
        let tracing_enabled = first
            .and_then(|p| p.tracing.as_ref())
            .and_then(|l| l.enabled)
            .unwrap_or(true);
        let content_mode = if first
            .and_then(|p| p.tracing.as_ref())
            .and_then(|l| l.content_mode.as_deref())
            == Some("metadata")
        {
            DialogContentMode::Metadata
        } else {
            DialogContentMode::Full
        };
        let backend = first
            .and_then(|p| p.tracing.as_ref())
            .and_then(|l| l.backend.as_deref())
            .and_then(crate::config::Backend::parse)
            .unwrap_or_default();
        DialogState {
            profile_idx: 0,
            dir,
            dir_edited: false,
            field: DialogField::Profile,
            error: None,
            dir_picker,
            tracing_enabled,
            content_mode,
            backend,
            harness: first.and_then(|p| crate::harness::Harness::detect(&p.command)),
            model: first.and_then(|p| p.model.clone()).unwrap_or_default(),
            bypass_approvals: first.and_then(|p| p.bypass_approvals).unwrap_or(false),
            resume_last: false,
            one_shot: String::new(),
            langfuse_available: false,
            experiment: String::new(),
            variant: String::new(),
            experiments_available: false,
            max_cost: first
                .and_then(|p| p.tracing.as_ref())
                .and_then(|t| t.max_cost_usd)
                .map(|c| format!("{c}"))
                .unwrap_or_default(),
            max_turns: first
                .and_then(|p| p.tracing.as_ref())
                .and_then(|t| t.max_turns)
                .map(|n| n.to_string())
                .unwrap_or_default(),
        }
    }

    /// The guard fields show where the CLI's PreToolUse hook can refuse a
    /// call: Claude and Codex, on a traced launch.
    fn guard_available(&self) -> bool {
        self.tracing_enabled
            && matches!(
                self.harness,
                Some(crate::harness::Harness::Claude) | Some(crate::harness::Harness::Codex)
            )
    }

    /// The typed limits, or the reason one cannot be read.
    pub fn budget(&self) -> Result<(Option<f64>, Option<u32>), String> {
        let cost = match self.max_cost.trim() {
            "" => None,
            s => Some(
                s.parse::<f64>()
                    .ok()
                    .filter(|c| c.is_finite() && *c > 0.0)
                    .ok_or_else(|| format!("max cost must be a positive number, not {s:?}"))?,
            ),
        };
        let turns = match self.max_turns.trim() {
            "" => None,
            s => Some(
                s.parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("max turns must be a positive count, not {s:?}"))?,
            ),
        };
        Ok((cost, turns))
    }

    /// The fields `Tab` walks, in order. A profile whose command is not a
    /// known CLI has nothing to render options into, so those four are
    /// left out entirely rather than shown dead.
    pub fn fields(&self) -> Vec<DialogField> {
        let mut fields = vec![
            DialogField::Profile,
            DialogField::Dir,
            DialogField::Tracing,
            DialogField::Backend,
            DialogField::ContentMode,
        ];
        if self.harness.is_some() {
            fields.extend([
                DialogField::Model,
                DialogField::Approvals,
                DialogField::Resume,
                DialogField::OneShot,
            ]);
        }
        if self.guard_available() {
            fields.extend([DialogField::MaxCost, DialogField::MaxTurns]);
        }
        if self.experiments_available && self.tracing_enabled {
            fields.extend([DialogField::Experiment, DialogField::Variant]);
        }
        fields
    }

    /// Whether the launch can be recorded as an experiment run: only with
    /// a trace store open, since the run row hangs off the launch row.
    pub fn with_experiments(mut self, available: bool) -> Self {
        self.experiments_available = available;
        self
    }

    /// The experiment this launch records itself under, if one is named.
    pub fn experiment_link(&self) -> Option<crate::tracing::experiments::ExperimentLink> {
        let experiment = self.experiment.trim();
        if experiment.is_empty() || !self.experiments_available || !self.tracing_enabled {
            return None;
        }
        let variant = self.variant.trim();
        Some(crate::tracing::experiments::ExperimentLink {
            experiment: experiment.to_string(),
            variant: if variant.is_empty() {
                "interactive".to_string()
            } else {
                variant.to_string()
            },
            prompt: self.one_shot.trim().to_string(),
        })
    }

    fn step_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let len = fields.len() as isize;
        self.field = fields[((at + delta).rem_euclid(len)) as usize];
    }

    /// The options this dialog asks for, ready to render for a harness.
    /// Blank text fields become `None`, so nothing empty is ever passed.
    pub fn launch_options(&self) -> crate::harness::LaunchOptions {
        crate::harness::LaunchOptions {
            model: Some(self.model.trim().to_string()).filter(|m| !m.is_empty()),
            bypass_approvals: self.bypass_approvals,
            resume: if self.resume_last {
                crate::harness::Resume::Last
            } else {
                crate::harness::Resume::Off
            },
            one_shot: Some(self.one_shot.trim().to_string()).filter(|p| !p.is_empty()),
        }
    }

    /// Applies the runtime's defaults: the configured backend (unless the
    /// first profile overrides it) and whether Langfuse can be chosen at
    /// all. A Langfuse choice without credentials falls back to local.
    pub fn with_backend_options(
        mut self,
        default: crate::config::Backend,
        langfuse_available: bool,
        profiles: &[Profile],
    ) -> Self {
        let overridden = profiles
            .get(self.profile_idx)
            .and_then(|p| p.tracing.as_ref())
            .and_then(|l| l.backend.as_deref())
            .and_then(crate::config::Backend::parse);
        self.backend = overridden.unwrap_or(default);
        self.langfuse_available = langfuse_available;
        if !langfuse_available && self.backend.langfuse() {
            self.backend = crate::config::Backend::Local;
        }
        self
    }

    pub fn refresh_dir_entries(&mut self) {
        self.dir_picker.refresh(&self.dir);
    }

    fn set_profile(&mut self, idx: usize, profiles: &[Profile]) {
        self.profile_idx = idx;
        if let Some(p) = profiles.get(idx) {
            self.harness = crate::harness::Harness::detect(&p.command);
            self.model = p.model.clone().unwrap_or_default();
            self.bypass_approvals = p.bypass_approvals.unwrap_or(false);
            if self.harness.is_none() {
                // the option fields are gone: do not leave focus on one
                self.resume_last = false;
                self.one_shot.clear();
                if !self.fields().contains(&self.field) {
                    self.field = DialogField::Profile;
                }
            }
        }
        if let Some(p) = profiles.get(idx)
            && let Some(over) = &p.tracing
        {
            if let Some(en) = over.enabled {
                self.tracing_enabled = en;
            }
            if let Some(cm) = over.content_mode.as_deref() {
                self.content_mode = if cm == "metadata" {
                    DialogContentMode::Metadata
                } else {
                    DialogContentMode::Full
                };
            }
            if let Some(b) = over
                .backend
                .as_deref()
                .and_then(crate::config::Backend::parse)
                && (self.langfuse_available || !b.langfuse())
            {
                self.backend = b;
            }
        }
        if !self.dir_edited {
            self.dir = default_dir_for_profile(profiles.get(idx));
            self.refresh_dir_entries();
        }
    }

    pub fn navigate_to_parent(&mut self) {
        if self.dir_picker.navigate_to_parent(&mut self.dir) {
            self.dir_edited = true;
        }
    }

    pub fn navigate_into(&mut self, sub: &str) {
        self.dir_picker.navigate_into(&mut self.dir, sub);
        self.dir_edited = true;
    }

    pub fn handle_key(&mut self, key: &KeyEvent, profiles: &[Profile]) -> DialogResult {
        match key.code {
            KeyCode::Esc => return DialogResult::Cancel,
            KeyCode::Tab => {
                self.step_field(1);
                self.dir_picker.leave();
                return DialogResult::Consumed;
            }
            KeyCode::BackTab => {
                self.step_field(-1);
                self.dir_picker.leave();
                return DialogResult::Consumed;
            }
            _ => {}
        }

        match self.field {
            DialogField::Profile => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Down | KeyCode::Char('j') => {
                    let next = (self.profile_idx + 1) % profiles.len().max(1);
                    self.set_profile(next, profiles);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let len = profiles.len().max(1);
                    self.set_profile((self.profile_idx + len - 1) % len, profiles);
                }
                _ => {}
            },
            DialogField::Dir => match self.dir_picker.handle_key(key, &mut self.dir) {
                dir_picker::PickerEvent::Submit => {
                    self.dir_edited = true;
                    return DialogResult::Submit;
                }
                dir_picker::PickerEvent::Consumed { path_changed } => {
                    if path_changed {
                        self.dir_edited = true;
                    }
                }
                dir_picker::PickerEvent::Ignored => {}
            },
            DialogField::Tracing => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(' ')
                | KeyCode::Char('t')
                | KeyCode::Char('T')
                | KeyCode::Left
                | KeyCode::Right => {
                    self.tracing_enabled = !self.tracing_enabled;
                }
                _ => {}
            },
            DialogField::Backend => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Left if self.langfuse_available => {
                    self.backend = self.backend.next().next();
                }
                KeyCode::Char(' ') | KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Right
                    if self.langfuse_available =>
                {
                    self.backend = self.backend.next();
                }
                _ => {}
            },
            DialogField::Model => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.model.push(c),
                KeyCode::Backspace => {
                    self.model.pop();
                }
                _ => {}
            },
            DialogField::Approvals => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                    self.bypass_approvals = !self.bypass_approvals;
                }
                _ => {}
            },
            DialogField::Resume => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                    self.resume_last = !self.resume_last;
                }
                _ => {}
            },
            DialogField::OneShot => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.one_shot.push(c),
                KeyCode::Backspace => {
                    self.one_shot.pop();
                }
                _ => {}
            },
            DialogField::Experiment => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.experiment.push(c),
                KeyCode::Backspace => {
                    self.experiment.pop();
                }
                _ => {}
            },
            DialogField::Variant => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.variant.push(c),
                KeyCode::Backspace => {
                    self.variant.pop();
                }
                _ => {}
            },
            DialogField::MaxCost => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.max_cost.push(c),
                KeyCode::Backspace => {
                    self.max_cost.pop();
                }
                _ => {}
            },
            DialogField::MaxTurns => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(c) => self.max_turns.push(c),
                KeyCode::Backspace => {
                    self.max_turns.pop();
                }
                _ => {}
            },
            DialogField::ContentMode => match key.code {
                KeyCode::Enter => return DialogResult::Submit,
                KeyCode::Char(' ')
                | KeyCode::Char('m')
                | KeyCode::Char('M')
                | KeyCode::Left
                | KeyCode::Right => {
                    self.content_mode = match self.content_mode {
                        DialogContentMode::Full => DialogContentMode::Metadata,
                        DialogContentMode::Metadata => DialogContentMode::Full,
                    };
                }
                _ => {}
            },
        }
        DialogResult::Consumed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryPane {
    SessionsList,
    LogDetail,
}

#[derive(Debug)]
pub struct HistoryState {
    pub sessions: Vec<SessionSummary>,
    pub selected_session_idx: usize,
    pub log_lines: Vec<ratatui::text::Line<'static>>,
    pub scroll_offset: usize,
    pub focused_pane: HistoryPane,
    pub all_projects: bool,
    pub error: Option<String>,
    pub base_dir: Option<std::path::PathBuf>,
    /// Log-pane interior height, written back by the renderer each frame so
    /// scrolling can clamp to "last full page" instead of running past the
    /// end of the content (Cell: draw only has &HistoryState).
    pub viewport_rows: std::cell::Cell<usize>,
}

impl HistoryState {
    pub fn new(current_dir: Option<&std::path::Path>) -> Self {
        let base_dir = current_dir.map(|p| p.to_path_buf());
        let sessions = history::discover_sessions(None, None, current_dir, false);
        let mut state = HistoryState {
            sessions,
            selected_session_idx: 0,
            log_lines: Vec::new(),
            scroll_offset: 0,
            focused_pane: HistoryPane::SessionsList,
            all_projects: false,
            error: None,
            base_dir,
            viewport_rows: std::cell::Cell::new(30),
        };
        state.load_selected_log();
        state
    }

    /// Greatest scroll offset that still shows a full page (0 when the log
    /// fits in the viewport).
    pub fn max_scroll(&self) -> usize {
        self.log_lines
            .len()
            .saturating_sub(self.viewport_rows.get().max(1))
    }

    pub fn load_selected_log(&mut self) {
        if let Some(summary) = self.sessions.get(self.selected_session_idx) {
            match history::load_session_log(&summary.file_path) {
                Ok(entries) => {
                    self.log_lines = history::render_log_lines(&entries);
                    self.error = None;
                }
                Err(e) => {
                    self.log_lines = Vec::new();
                    self.error = Some(format!("Failed to load session log: {e}"));
                }
            }
        } else {
            self.log_lines = Vec::new();
        }
        // open at the most recent turn, not the oldest
        self.scroll_offset = self.max_scroll();
    }

    pub fn reload_sessions(&mut self) {
        let cur_ref = self.base_dir.as_deref();
        self.sessions = history::discover_sessions(None, None, cur_ref, self.all_projects);
        self.selected_session_idx = 0;
        self.load_selected_log();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPane {
    Sessions,
    Turns,
    Detail,
}

/// How the Detail pane draws a turn's observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailView {
    /// Flat, one line per observation (the original).
    #[default]
    List,
    /// Box-drawing hierarchy with collapsible subtrees.
    Tree,
    /// Bars over the turn's own time window.
    Timeline,
    /// The loop's numbers: calls, retries, where the time went, context.
    Loop,
}

impl DetailView {
    pub fn next(self) -> DetailView {
        match self {
            DetailView::List => DetailView::Tree,
            DetailView::Tree => DetailView::Timeline,
            DetailView::Timeline => DetailView::Loop,
            DetailView::Loop => DetailView::List,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DetailView::List => "list",
            DetailView::Tree => "tree (hierarchy)",
            DetailView::Timeline => "timeline (time)",
            DetailView::Loop => "loop",
        }
    }
}

/// The `T` trace browser: sessions → turns → observations, read from the
/// local store through a read-only connection. Queries are indexed and
/// `LIMIT`ed, so they run synchronously on the main thread like the
/// history viewer's file reads.
pub struct TraceBrowserState {
    conn: Option<rusqlite::Connection>,
    pub error: Option<String>,
    pub sessions: Vec<crate::tracing::store::query::SessionStat>,
    pub selected_session: usize,
    pub turns: Vec<crate::tracing::store::query::TraceStat>,
    pub selected_turn: usize,
    pub observations: Vec<crate::tracing::store::query::ObservationView>,
    pub selected_observation: usize,
    /// Detail pane shows the selected observation's body instead of the
    /// observation list.
    pub expanded: bool,
    /// Which shape the Detail pane draws.
    pub detail_view: DetailView,
    /// Observation ids whose subtree is folded away in the tree view.
    /// Cleared whenever the turn changes.
    pub collapsed: std::collections::HashSet<String>,
    pub detail_lines: Vec<ratatui::text::Line<'static>>,
    pub scroll_offset: usize,
    pub focused: BrowserPane,
    pub all_projects: bool,
    pub project_slug: Option<String>,
    /// Search prompt: `Some` while typing after `/`.
    pub search_input: Option<String>,
    /// The query the turns pane currently shows hits for.
    pub search_query: Option<String>,
    /// Detail-pane interior height, written back by the renderer.
    pub viewport_rows: std::cell::Cell<usize>,
    last_refresh: Instant,
    /// The store, for the browser's own writes (scores).
    db_path: Option<std::path::PathBuf>,
    /// Latest verdict per turn id, for the marks in the Turns pane.
    pub scores: std::collections::HashMap<String, f64>,
    langfuse: Option<crate::config::ResolvedLangfuse>,
}

impl std::fmt::Debug for TraceBrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceBrowserState")
            .field("open", &self.conn.is_some())
            .field("error", &self.error)
            .field("sessions", &self.sessions.len())
            .field("selected_session", &self.selected_session)
            .field("turns", &self.turns.len())
            .field("selected_turn", &self.selected_turn)
            .field("observations", &self.observations.len())
            .field("detail_view", &self.detail_view)
            .field("focused", &self.focused)
            .field("all_projects", &self.all_projects)
            .finish()
    }
}

impl TraceBrowserState {
    /// `db_path` is `None` when tracing is off for this run.
    pub fn new(db_path: Option<&std::path::Path>, current_dir: Option<&std::path::Path>) -> Self {
        let (conn, error) = match db_path {
            None => (None, Some("tracing is off — nothing to browse".to_string())),
            Some(path) => match crate::tracing::store::open_ro(path) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            },
        };
        let mut state = TraceBrowserState {
            conn,
            error,
            sessions: Vec::new(),
            selected_session: 0,
            turns: Vec::new(),
            selected_turn: 0,
            observations: Vec::new(),
            selected_observation: 0,
            detail_view: DetailView::default(),
            collapsed: std::collections::HashSet::new(),
            expanded: false,
            detail_lines: Vec::new(),
            scroll_offset: 0,
            focused: BrowserPane::Sessions,
            all_projects: false,
            project_slug: current_dir.map(history::project_slug),
            search_input: None,
            search_query: None,
            viewport_rows: std::cell::Cell::new(30),
            last_refresh: Instant::now(),
            db_path: db_path.map(std::path::Path::to_path_buf),
            scores: std::collections::HashMap::new(),
            langfuse: None,
        };
        state.reload_sessions();
        state
    }
    /// Where the browser's writes go (scores), and to Langfuse when set.
    pub fn with_langfuse(mut self, lf: Option<crate::config::ResolvedLangfuse>) -> Self {
        self.langfuse = lf;
        self
    }

    fn refresh_scores(&mut self) {
        if let Some(conn) = &self.conn {
            self.scores =
                crate::tracing::scores::latest_trace_scores(conn, crate::tracing::scores::VERDICT)
                    .unwrap_or_default();
        }
    }

    /// `s`: the selected turn's verdict cycles none → good → bad → none.
    /// A verdict also goes to Langfuse, in the background, when configured.
    pub fn cycle_score(&mut self) -> Result<String, String> {
        use crate::tracing::scores;
        let Some(turn) = self.turns.get(self.selected_turn) else {
            return Err("no turn selected".into());
        };
        let Some(db) = self.db_path.clone() else {
            return Err("no trace store open".into());
        };
        let conn = crate::tracing::store::open_aux(&db)?;
        let (trace_id, ordinal) = (turn.id.clone(), turn.ordinal);
        let next = match self.scores.get(&trace_id) {
            None => Some(1.0),
            Some(v) if *v >= 0.5 => Some(0.0),
            Some(_) => None,
        };
        let message = match next {
            Some(value) => {
                let score = scores::record(&conn, "trace", &trace_id, scores::VERDICT, value, None)
                    .map_err(|e| e.to_string())?;
                if let Some(lf) = self.langfuse.clone() {
                    std::thread::spawn(move || {
                        let _ = scores::export(&lf, &score, &trace_id);
                    });
                }
                format!("turn #{ordinal}: {}", scores::label(value))
            }
            None => {
                scores::clear(&conn, "trace", &trace_id, scores::VERDICT)
                    .map_err(|e| e.to_string())?;
                format!("turn #{ordinal}: verdict cleared")
            }
        };
        self.refresh_scores();
        Ok(message)
    }

    fn filter(&self) -> crate::tracing::store::query::SessionFilter {
        crate::tracing::store::query::SessionFilter {
            project_slug: if self.all_projects {
                None
            } else {
                self.project_slug.clone()
            },
            since_ns: None,
            limit: 500,
        }
    }

    /// Positions the browser on `session_key` (widening to all projects
    /// when it is outside the current one) and, when given, on `trace_id`.
    pub fn focus_session(&mut self, session_key: &str, trace_id: Option<&str>) {
        if !self.sessions.iter().any(|s| s.key == session_key) {
            self.all_projects = true;
            self.reload_sessions();
        }
        let Some(idx) = self.sessions.iter().position(|s| s.key == session_key) else {
            return;
        };
        self.selected_session = idx;
        self.search_query = None;
        self.load_turns();
        if let Some(tid) = trace_id
            && let Some(t) = self.turns.iter().position(|t| t.id == tid)
        {
            self.selected_turn = t;
            self.load_observations();
            self.focused = BrowserPane::Turns;
        }
    }

    pub fn reload_sessions(&mut self) {
        let Some(conn) = &self.conn else {
            return;
        };
        match crate::tracing::store::query::list_sessions(conn, &self.filter()) {
            Ok(rows) => {
                self.sessions = rows;
                self.error = None;
            }
            Err(e) => {
                self.sessions.clear();
                self.error = Some(e.to_string());
            }
        }
        self.selected_session = self
            .selected_session
            .min(self.sessions.len().saturating_sub(1));
        self.search_query = None;
        self.load_turns();
    }

    pub fn load_turns(&mut self) {
        let Some(conn) = &self.conn else {
            return;
        };
        self.turns = match self.sessions.get(self.selected_session) {
            Some(s) => crate::tracing::store::query::list_traces(conn, &s.key).unwrap_or_default(),
            None => Vec::new(),
        };
        // The browser lists newest turns first, while the store query remains
        // chronological for callers that need replay order.
        self.turns.reverse();
        self.selected_turn = 0;
        self.load_observations();
    }

    pub fn load_observations(&mut self) {
        let Some(conn) = &self.conn else {
            return;
        };
        self.observations = match self.turns.get(self.selected_turn) {
            Some(t) => {
                crate::tracing::store::query::list_observations(conn, &t.id).unwrap_or_default()
            }
            None => Vec::new(),
        };
        self.selected_observation = self
            .selected_observation
            .min(self.observations.len().saturating_sub(1));
        self.expanded = false;
        self.scroll_offset = 0;
        self.collapsed.clear();
        self.refresh_scores();
        self.rebuild_detail();
    }

    /// Runs an FTS query and shows the matching turns in the turns pane.
    pub fn run_search(&mut self, query: &str) {
        let Some(conn) = &self.conn else {
            return;
        };
        match crate::tracing::store::query::search(conn, query, 200) {
            Ok(hits) => {
                let mut seen = std::collections::HashSet::new();
                let mut turns = Vec::new();
                for hit in hits {
                    if seen.insert(hit.trace_id.clone())
                        && let Ok(Some(t)) =
                            crate::tracing::store::query::find_trace(conn, &hit.trace_id)
                    {
                        turns.push(t);
                    }
                }
                self.turns = turns;
                self.selected_turn = 0;
                self.search_query = Some(query.to_string());
                self.focused = BrowserPane::Turns;
                self.error = None;
                self.load_observations();
            }
            Err(e) => self.error = Some(format!("search: {e}")),
        }
    }

    fn rebuild_detail(&mut self) {
        use ratatui::style::{Color, Style};
        use ratatui::text::Line;
        let mut lines = Vec::new();
        if let Some(o) = self.observations.get(self.selected_observation)
            && self.expanded
        {
            let dim = Style::default().fg(Color::DarkGray);
            let head = |label: &str| {
                Line::styled(format!("── {label} ──"), Style::default().fg(Color::Yellow))
            };
            lines.push(Line::styled(
                format!(
                    "{} {}  {}{}",
                    o.obs_type,
                    o.name,
                    o.model.clone().unwrap_or_default(),
                    o.status_message
                        .as_ref()
                        .map(|m| format!("  ({m})"))
                        .unwrap_or_default()
                ),
                Style::default().fg(Color::Cyan),
            ));
            let duration = o
                .end_ns
                .map(|e| crate::tracing::cli::fmt_ms((e - o.start_ns) / 1_000_000))
                .unwrap_or_else(|| "running".into());
            lines.push(Line::styled(
                format!(
                    "{}  {}  tokens {}  cost {}",
                    crate::tracing::cli::fmt_time(o.start_ns),
                    duration,
                    crate::tracing::cli::fmt_tokens(o.total_tokens),
                    crate::tracing::cli::fmt_cost(o.total_cost_usd)
                ),
                dim,
            ));
            for (label, body) in [
                ("input", &o.input),
                ("output", &o.output),
                ("thinking", &o.thinking),
            ] {
                if let Some(text) = body {
                    lines.push(head(label));
                    for l in text.lines().take(2_000) {
                        lines.push(Line::raw(l.to_string()));
                    }
                }
            }
            if o.metadata != "{}" {
                lines.push(head("metadata"));
                lines.push(Line::raw(o.metadata.clone()));
            }
            if o.input.is_none() && o.output.is_none() {
                lines.push(Line::styled(
                    "(no content available — not captured, metadata-only, or still pending)",
                    dim,
                ));
            }
        }
        self.detail_lines = lines;
    }

    pub fn max_scroll(&self) -> usize {
        self.detail_lines
            .len()
            .saturating_sub(self.viewport_rows.get().max(1))
    }

    pub fn toggle_expanded(&mut self) {
        if self.observations.is_empty() {
            return;
        }
        self.expanded = !self.expanded;
        self.scroll_offset = 0;
        self.rebuild_detail();
    }

    pub fn select_observation(&mut self, idx: usize) {
        if self.observations.is_empty() {
            return;
        }
        self.selected_observation = idx.min(self.observations.len() - 1);
        self.scroll_offset = 0;
        self.rebuild_detail();
    }

    /// The observations the current view actually draws, in draw order.
    /// Only the tree view hides rows (folded subtrees).
    pub fn visible_rows(&self) -> Vec<usize> {
        match self.detail_view {
            DetailView::Tree => {
                let tree =
                    crate::tracing::store::query::nest_observations(self.observations.clone());
                let rows = crate::tracing::view::tree_rows(&tree, &self.collapsed);
                let indexes: std::collections::HashMap<&str, usize> = self
                    .observations
                    .iter()
                    .enumerate()
                    .map(|(i, o)| (o.id.as_str(), i))
                    .collect();
                rows.iter()
                    .filter_map(|r| indexes.get(r.obs.id.as_str()).copied())
                    .collect()
            }
            _ => (0..self.observations.len()).collect(),
        }
    }

    /// Moves the selection `delta` rows through the *visible* rows, so a
    /// folded subtree is stepped over rather than through.
    pub fn step_observation(&mut self, delta: isize) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return;
        }
        let at = rows
            .iter()
            .position(|&i| i == self.selected_observation)
            .unwrap_or(0);
        let next = (at as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.select_observation(rows[next]);
    }

    /// Cycles list → tree → timeline → loop. Leaving the tree keeps the fold set
    /// so coming back looks the way it was left; the selection is pulled
    /// back onto a visible row.
    pub fn cycle_detail_view(&mut self) {
        self.detail_view = self.detail_view.next();
        self.expanded = false;
        self.scroll_offset = 0;
        let rows = self.visible_rows();
        if !rows.is_empty() && !rows.contains(&self.selected_observation) {
            // The selection fell inside a folded subtree. Follow parent ids
            // rather than numeric indexes: chronology and tree order are
            // intentionally different views of the same observations.
            let visible: std::collections::HashSet<usize> = rows.iter().copied().collect();
            let indexes: std::collections::HashMap<&str, usize> = self
                .observations
                .iter()
                .enumerate()
                .map(|(i, o)| (o.id.as_str(), i))
                .collect();
            let mut cursor = self.selected_observation;
            let mut fallback = None;
            let mut visited = std::collections::HashSet::new();
            while visited.insert(cursor) {
                let Some(parent) = self.observations[cursor].parent_id.as_deref() else {
                    break;
                };
                let Some(&parent_idx) = indexes.get(parent) else {
                    break;
                };
                if visible.contains(&parent_idx) {
                    fallback = Some(parent_idx);
                    break;
                }
                cursor = parent_idx;
            }
            self.selected_observation = fallback.unwrap_or(rows[0]);
        }
        self.rebuild_detail();
    }

    /// Folds or unfolds the selected row's subtree (tree view only, and
    /// only where there is a subtree to fold).
    pub fn toggle_collapsed(&mut self) {
        if self.detail_view != DetailView::Tree {
            return;
        }
        let Some(o) = self.observations.get(self.selected_observation) else {
            return;
        };
        let id = o.id.clone();
        let tree = crate::tracing::store::query::nest_observations(self.observations.clone());
        let has_children = crate::tracing::view::tree_rows(&tree, &Default::default())
            .iter()
            .any(|r| r.obs.id == id && r.has_children);
        if !has_children {
            return;
        }
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
    }

    /// Live sessions change under the browser: re-query at most twice a
    /// second to keep newly arriving chats and ongoing turns up to date.
    pub fn refresh_if_live(&mut self, now: Instant) {
        if now.duration_since(self.last_refresh) < std::time::Duration::from_millis(500) {
            return;
        }
        if self.search_query.is_some() {
            return;
        }
        self.last_refresh = now;
        let filter = self.filter();
        let Some(conn) = &self.conn else {
            return;
        };
        let selected_key = self
            .sessions
            .get(self.selected_session)
            .map(|s| s.key.clone());
        let selected_turn_id = self.turns.get(self.selected_turn).map(|t| t.id.clone());
        let selected_obs_id = self
            .observations
            .get(self.selected_observation)
            .map(|o| o.id.clone());
        if let Ok(rows) = crate::tracing::store::query::list_sessions(conn, &filter) {
            self.sessions = rows;
        }
        if self.sessions.is_empty() {
            self.turns.clear();
            self.observations.clear();
            self.rebuild_detail();
            return;
        }
        // If the user is focused on the Sessions list at row 0 (the newest session),
        // keep them at row 0 so newest chats/activity are reflected immediately.
        // If they navigated to an older session (> 0) or are focused on another pane,
        // stay locked to the session they were inspecting.
        let target_idx = if self.focused == BrowserPane::Sessions && self.selected_session == 0 {
            0
        } else if let Some(key) = &selected_key
            && let Some(idx) = self.sessions.iter().position(|s| &s.key == key)
        {
            idx
        } else {
            self.selected_session
                .min(self.sessions.len().saturating_sub(1))
        };
        self.selected_session = target_idx;
        if let Some(s) = self.sessions.get(self.selected_session) {
            let key = &s.key;
            self.turns = crate::tracing::store::query::list_traces(conn, key).unwrap_or_default();
            self.turns.reverse();
            // If the user was viewing the newest turn (0), keep newest turn (0) so live
            // execution streams in real time. Otherwise preserve selected turn id.
            let target_turn = if self.selected_turn == 0 {
                0
            } else {
                selected_turn_id
                    .and_then(|id| self.turns.iter().position(|t| t.id == id))
                    .unwrap_or(0)
            };
            self.selected_turn = target_turn.min(self.turns.len().saturating_sub(1));
            if let Some(t) = self.turns.get(self.selected_turn) {
                self.observations = crate::tracing::store::query::list_observations(conn, &t.id)
                    .unwrap_or_default();
                self.selected_observation = selected_obs_id
                    .and_then(|id| self.observations.iter().position(|o| o.id == id))
                    .unwrap_or(self.observations.len().saturating_sub(1));
                let keep_scroll = self.scroll_offset;
                self.refresh_scores();
                self.rebuild_detail();
                self.scroll_offset = keep_scroll.min(self.max_scroll());
            } else {
                self.observations.clear();
                self.rebuild_detail();
            }
        }
    }
}

/// A local (non-agent) text selection being dragged or held over one
/// session's screen. `session_id` (not an index) so a selection can be
/// recognized as stale once its session is removed/replaced/reordered.
#[derive(Debug)]
pub struct ActiveSelection {
    pub session_id: usize,
    pub sel: Selection,
    pub dragging: bool,
}

/// Who owns an in-progress left-button drag, latched at `Down(Left)` so a
/// modifier change mid-drag (e.g. releasing Shift) can't retarget its later
/// Drag/Up events -- see `handle_mouse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOwner {
    Local,
    Agent,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub sidebar_section: SidebarSection,
    pub history_sessions: Vec<SessionSummary>,
    pub selected_history: usize,
    pub history_all_projects: bool,
    pub mode: Mode,
    pub should_quit: bool,
    pub notice: Option<Notice>,
    pub profiles: Vec<Profile>,
    pub pane_size: (u16, u16), // (rows, cols)
    pub selection: Option<ActiveSelection>,
    pub search: Option<SearchState>,
    /// When false, `copy_to_clipboard`/`paste_into_attached` are no-ops.
    /// Tests that don't specifically exercise the clipboard round-trip set
    /// this to `false` so `cargo test` never touches the real system
    /// clipboard.
    pub clipboard_enabled: bool,
    /// Launches the dialog linked to an experiment, by session id, until
    /// the session ends and the run is recorded.
    pub experiment_links:
        std::collections::HashMap<usize, crate::tracing::experiments::ExperimentLink>,
    drag_owner: Option<DragOwner>,
    just_detached: bool,
    next_id: usize,
    tx: Sender<AppEvent>,
    tracing: Option<crate::tracing::TraceRuntime>,
    /// Where the trace browser reads from; `None` when tracing is off.
    pub trace_db_path: Option<std::path::PathBuf>,
    /// Optional override for the persistent sessions file path (defaults to ~/.agent-mux/sessions.json).
    pub sessions_file: Option<std::path::PathBuf>,
    /// Whether the sidebar is currently hidden (full-screen harness).
    pub sidebar_hidden: bool,
    /// Overall terminal dimensions (rows, cols).
    pub terminal_size: (u16, u16),
    /// Skill packages, loaded at startup and rescanned when the Skills
    /// view opens; `launch_skill` resolves ids against this list.
    pub skills: Vec<crate::skill::SkillDefinition>,
    /// Packages with `hidden = true`: workflow step skills and the
    /// planner. Installed and launched by workflows, never listed in the
    /// Agents sidebar.
    pub hidden_skills: Vec<crate::skill::SkillDefinition>,
    /// Where packages are installed for a harness; `None` is the real home.
    /// Tests point this at a temporary directory.
    pub skill_install_home: Option<std::path::PathBuf>,
    /// Where user packages are read from; `None` is `~/.agent-mux/skills`.
    pub skills_dir: Option<std::path::PathBuf>,
    /// Selected row of the Agents sidebar section (index into `skills`).
    pub selected_agent: usize,
    /// Last managed package inspected in the Skills workbench.
    skill_workbench: Option<SkillWorkbenchBookmark>,
    /// `[agents]`: whether launches get briefing snapshots and MCP.
    pub agents: crate::config::AgentsSettings,
    /// Where briefing snapshots are written; `None` is the runtime
    /// directory (`AGENT_MUX_RUNTIME_DIR` or `~/.agent-mux/snapshots`).
    pub runtime_dir: Option<std::path::PathBuf>,
    /// Cached briefing for the active Agents preview (if capable of trace.read).
    pub cached_briefing: Option<crate::tracing::analysis::Briefing>,
    /// Instant when the briefing was last successfully refreshed.
    pub cached_briefing_as_of: Option<Instant>,
    /// Most recent warning / error from briefing refresh, if any.
    pub cached_briefing_warning: Option<String>,
    /// Monotonically increasing revision counter for briefing queries.
    pub briefing_revision: u64,
    /// Whether an asynchronous briefing query is currently in flight.
    pub briefing_pending: bool,
    /// Timestamp of the last refresh trigger.
    pub last_briefing_refresh: Option<Instant>,
    /// Persistent identifier for this mux run instance.
    pub app_run_id: String,
    /// Revision counter for published live snapshots.
    pub live_snapshot_revision: u64,
    /// Instant of the last published live snapshot.
    pub last_snapshot_published: Option<Instant>,
    /// The Loop Engineering registry (`~/.agent-mux/loops.json`).
    pub loop_registry: crate::loops::registry::Registry,
    /// Where the registry is saved (tests override).
    pub loops_file: Option<std::path::PathBuf>,
    /// `[loops]` resolved.
    pub loops: crate::config::LoopRunnerSettings,
    pub selected_loop: usize,
    /// Loop runs currently live, by session id.
    pub live_loop_runs: Vec<loops::LiveLoopRun>,
    /// Preview cards per loop id, refreshed while the section is visible.
    pub loop_cards: std::collections::HashMap<String, loops::LoopCard>,
    pub last_loop_card_refresh: Option<Instant>,
    pub last_schedule_pass: Option<Instant>,
    /// The scheduler skips its passes until `load_loop_registry` ran.
    pub loops_loaded: bool,
    /// Readiness audits cached per workspace.
    pub loop_audits: loops::AuditCache,
    /// The configuration file that was accepted, for the About overlay.
    pub config_path: Option<std::path::PathBuf>,
    /// The configuration library root (`assets::root()` unless a test
    /// overrides it).
    pub library_root: Option<std::path::PathBuf>,
    /// `editor` from the settings; `$VISUAL`/`$EDITOR`/`vi` otherwise.
    pub editor: Option<String>,
    /// An editor the main loop must run before the next frame.
    pub editor_request: Option<EditorRequest>,
    /// Workflow runs with sessions in flight.
    pub live_workflow_runs: Vec<workflows::LiveWorkflowRun>,
    /// Finished runs since startup, newest first (the store has the rows).
    pub recent_workflow_runs: Vec<workflows::RecentWorkflowRun>,
    pub workflows: crate::config::WorkflowSettings,
    pub last_workflow_pass: Option<Instant>,
    /// Planner sessions in flight and the documents they produced.
    pub live_workflow_plans: Vec<workflows::LivePlan>,
    pub planned_workflows: Vec<workflows::PlannedWorkflow>,
    /// The Workflows section's list and selection.
    pub workflow_list: Vec<crate::workflows::library::Entry>,
    pub selected_workflow: usize,
}

impl App {
    pub fn new(
        profiles: Vec<Profile>,
        tracing: Option<crate::tracing::TraceRuntime>,
        tx: Sender<AppEvent>,
    ) -> App {
        let app_run_id = tracing
            .as_ref()
            .map(|rt| rt.run_id().to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let trace_db_path = tracing.as_ref().map(|rt| rt.db_path().to_path_buf());
        let cur_dir = std::env::current_dir().ok();
        let mut history_sessions =
            history::discover_sessions(None, None, cur_dir.as_deref(), false);
        if history_sessions.is_empty() {
            history_sessions = history::discover_sessions(None, None, cur_dir.as_deref(), true);
        }
        let (skills, _) = crate::skill::load_skills(None);
        let (hidden_skills, skills): (Vec<_>, Vec<_>) = skills.into_iter().partition(|s| s.hidden);
        crate::skill::launch::sweep_briefings(
            &crate::tracing::analysis::default_snapshot_dir(),
            std::time::Duration::from_secs(24 * 3600),
        );
        App {
            sessions: Vec::new(),
            selected: 0,
            sidebar_section: SidebarSection::Active,
            history_sessions,
            selected_history: 0,
            history_all_projects: false,
            mode: Mode::Control,
            should_quit: false,
            notice: None,
            profiles,
            pane_size: (24, 80),
            selection: None,
            search: None,
            clipboard_enabled: true,
            drag_owner: None,
            just_detached: false,
            next_id: 0,
            experiment_links: std::collections::HashMap::new(),
            tx,
            tracing,
            trace_db_path,
            sessions_file: None,
            sidebar_hidden: false,
            terminal_size: (27, 112),
            skills,
            hidden_skills,
            skill_install_home: None,
            skills_dir: None,
            selected_agent: 0,
            skill_workbench: None,
            agents: crate::config::AgentsSettings::default(),
            runtime_dir: None,
            cached_briefing: None,
            cached_briefing_as_of: None,
            cached_briefing_warning: None,
            briefing_revision: 0,
            briefing_pending: false,
            last_briefing_refresh: None,
            app_run_id,
            live_snapshot_revision: 0,
            last_snapshot_published: None,
            loop_registry: crate::loops::registry::Registry::default(),
            loops_file: None,
            loops: crate::config::LoopRunnerSettings::default(),
            selected_loop: 0,
            live_loop_runs: Vec::new(),
            loop_cards: std::collections::HashMap::new(),
            last_loop_card_refresh: None,
            last_schedule_pass: None,
            loops_loaded: false,
            loop_audits: std::collections::HashMap::new(),
            config_path: None,
            library_root: None,
            editor: None,
            editor_request: None,
            live_workflow_runs: Vec::new(),
            recent_workflow_runs: Vec::new(),
            workflows: crate::config::WorkflowSettings::default(),
            last_workflow_pass: None,
            live_workflow_plans: Vec::new(),
            planned_workflows: Vec::new(),
            workflow_list: Vec::new(),
            selected_workflow: 0,
        }
    }

    /// Rescans the user skill directory and the compiled-in packages,
    /// keeping the Agents selection on the same id when it still exists.
    pub fn reload_skills(&mut self) {
        let keep = self.selected_agent().map(|s| s.id.clone());
        let (skills, _) = crate::skill::load_skills(self.skills_dir.as_deref());
        let (hidden, skills): (Vec<_>, Vec<_>) = skills.into_iter().partition(|s| s.hidden);
        self.hidden_skills = hidden;
        self.skills = skills;
        self.selected_agent = keep
            .and_then(|id| self.skills.iter().position(|s| s.id == id))
            .unwrap_or_else(|| self.selected_agent.min(self.skills.len().saturating_sub(1)));
    }

    /// The package selected in the Agents sidebar section, if any.
    pub fn selected_agent(&self) -> Option<&crate::skill::SkillDefinition> {
        self.skills.get(self.selected_agent)
    }

    /// The home whose harness skill directories receive installs.
    fn skill_home(&self) -> std::path::PathBuf {
        self.skill_install_home
            .clone()
            .unwrap_or_else(crate::skill::install::home_dir)
    }

    /// A package by id, listed or hidden.
    pub fn find_skill(&self, id: &str) -> Option<&crate::skill::SkillDefinition> {
        self.skills
            .iter()
            .chain(self.hidden_skills.iter())
            .find(|s| s.id == id)
    }

    /// Index of the live session running `skill_id`. Skills are singletons:
    /// one non-exited session per skill id, whatever harness it runs on.
    pub fn running_skill_session(&self, skill_id: &str) -> Option<usize> {
        let now = Instant::now();
        self.sessions.iter().position(|s| {
            s.skill_id.as_deref() == Some(skill_id) && !matches!(s.status(now), Status::Exited(_))
        })
    }

    /// Harness the live session for `skill_id` runs on, if any.
    pub fn running_skill_harness(&self, skill_id: &str) -> Option<crate::harness::Harness> {
        self.running_skill_session(skill_id)
            .and_then(|idx| self.sessions.get(idx))
            .and_then(|s| crate::harness::Harness::detect(&s.profile.command))
    }

    /// Selects and attaches to session `idx`.
    fn attach_to_session(&mut self, idx: usize) {
        self.selected = idx;
        self.sidebar_section = SidebarSection::Active;
        self.mode = Mode::Attached;
        if let Some(s) = self.sessions.get_mut(idx) {
            s.tracker.on_attach();
        }
    }

    /// Toggles the sidebar visibility, expanding or contracting the harness.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_hidden = !self.sidebar_hidden;
        if self.sidebar_hidden {
            self.sidebar_section = SidebarSection::Active;
        }
        let (rows, cols) = self.terminal_size;
        let (pane_rows, pane_cols) = ui::main_pane_inner_dims(
            ratatui::layout::Rect::new(0, 0, cols, rows),
            self.sidebar_hidden,
        );
        self.set_pane_size(pane_rows, pane_cols);
    }

    /// Sets the terminal dimensions and updates the pane size accordingly.
    pub fn set_terminal_size(&mut self, rows: u16, cols: u16) {
        self.terminal_size = (rows, cols);
        let (pane_rows, pane_cols) = ui::main_pane_inner_dims(
            ratatui::layout::Rect::new(0, 0, cols, rows),
            self.sidebar_hidden,
        );
        self.set_pane_size(pane_rows, pane_cols);
    }

    /// Sets a custom path for persistent sessions file.
    pub fn set_sessions_file(&mut self, path: impl Into<std::path::PathBuf>) {
        self.sessions_file = Some(path.into());
    }

    /// Periodic housekeeping driven by `AppEvent::Tick`: the trace browser
    /// re-queries live sessions, live snapshots are published, and agent briefing is refreshed.
    pub fn on_tick(&mut self, now: Instant) {
        if let Mode::TraceBrowser(browser) = &mut self.mode {
            browser.refresh_if_live(now);
        }
        if let Mode::SkillsView(view) = &mut self.mode {
            view.refresh_if_live(now);
        }
        if let Mode::LoopsView(view) = &mut self.mode {
            view.refresh_if_live(now);
        }
        self.publish_live_snapshot_if_needed(now);
        self.refresh_briefing_if_needed(now);
        self.refresh_loop_cards_if_needed(now);
        self.scheduler_pass(now);
        self.workflow_pass(now);
        self.workflow_plan_pass(now);
        self.refresh_workflows_view(now);
    }

    /// Publishes live session snapshot bounded to 1MiB every second if needed.
    pub fn publish_live_snapshot_if_needed(&mut self, now: Instant) {
        let should_publish = match self.last_snapshot_published {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= std::time::Duration::from_secs(1),
        };
        if should_publish {
            self.last_snapshot_published = Some(now);
            self.live_snapshot_revision += 1;
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64;
            let sessions = self.collect_live_sessions(now);
            let snapshot = crate::tracing::analysis::LiveSnapshot {
                run_id: self.app_run_id.clone(),
                revision: self.live_snapshot_revision,
                heartbeat_ns: now_ns,
                sessions,
            };
            let root = crate::tracing::analysis::default_snapshot_dir();
            let _ = crate::tracing::analysis::publish_snapshot(&root, &snapshot);
        }
    }

    /// Cleans up the published live snapshot on shutdown.
    pub fn cleanup_live_snapshot(&self) {
        let root = crate::tracing::analysis::default_snapshot_dir();
        let _ = crate::tracing::analysis::clean_up_snapshot(&root, &self.app_run_id);
    }

    /// Refreshes the session briefing asynchronously if a trace-capable preview is visible.
    pub fn refresh_briefing_if_needed(&mut self, now: Instant) {
        let is_trace_preview_visible = !self.sidebar_hidden
            && self.sidebar_section == SidebarSection::Agents
            && matches!(self.mode, Mode::Control)
            && self
                .selected_agent()
                .is_some_and(|a| a.capabilities.iter().any(|c| c == "trace.read"));

        if is_trace_preview_visible && !self.briefing_pending {
            let should_refresh = match self.last_briefing_refresh {
                None => true,
                Some(last) => {
                    now.saturating_duration_since(last) >= std::time::Duration::from_secs(1)
                }
            };
            if should_refresh {
                self.briefing_pending = true;
                self.briefing_revision += 1;
                self.last_briefing_refresh = Some(now);

                let revision = self.briefing_revision;
                let tx = self.tx.clone();
                let db_path = self
                    .trace_db_path
                    .clone()
                    .unwrap_or_else(crate::tracing::analysis::default_trace_db_path);
                let workspace =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let live_sessions = self.collect_live_sessions(now);

                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn_blocking(move || {
                        let result = (|| -> Result<crate::tracing::analysis::Briefing, String> {
                            let conn = rusqlite::Connection::open_with_flags(
                                &db_path,
                                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                            )
                            .map_err(|e| format!("cannot open traces db: {e}"))?;

                            let now_ns = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as i64;
                            let since_ns = now_ns - (24 * 3600 * 1_000_000_000);
                            crate::tracing::analysis::briefing(
                                &conn,
                                &workspace,
                                since_ns,
                                now_ns,
                                &live_sessions,
                            )
                            .map_err(|e| e.to_string())
                        })();
                        let _ = tx.blocking_send(AppEvent::AnalysisUpdated { revision, result });
                    });
                }
            }
        }
    }

    /// Collects live session states for trace analysis correlation.
    pub fn collect_live_sessions(
        &self,
        now: Instant,
    ) -> Vec<crate::tracing::analysis::LiveSession> {
        let run_id = self
            .tracing
            .as_ref()
            .map(|rt| rt.run_id().to_string())
            .unwrap_or_else(|| self.app_run_id.clone());
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;

        self.sessions
            .iter()
            .map(|s| {
                let state = match s.status(now) {
                    Status::Working => crate::tracing::analysis::RuntimeState::Working,
                    Status::Idle => crate::tracing::analysis::RuntimeState::Idle,
                    Status::NeedsAttention => {
                        crate::tracing::analysis::RuntimeState::WaitingForUser
                    }
                    Status::Exited(_) => crate::tracing::analysis::RuntimeState::Exited,
                };
                let provider = crate::harness::Harness::detect(&s.profile.command)
                    .map(|h| h.as_str().to_string());
                let launch_id = s
                    .trace
                    .as_ref()
                    .map(|t| t.launch_id.clone())
                    .unwrap_or_else(|| format!("session-{}", s.id));

                crate::tracing::analysis::LiveSession {
                    run_id: run_id.clone(),
                    launch_id,
                    session_id: s.id,
                    session_key: None,
                    provider,
                    cwd: s.dir.clone(),
                    state,
                    updated_at_ns: now_ns,
                    active_tools: Vec::new(),
                }
            })
            .collect()
    }

    /// Handles completion of a background briefing query.
    pub fn handle_analysis_updated(
        &mut self,
        revision: u64,
        result: Result<crate::tracing::analysis::Briefing, String>,
    ) {
        self.briefing_pending = false;
        if revision >= self.briefing_revision {
            match result {
                Ok(briefing) => {
                    self.cached_briefing = Some(briefing);
                    self.cached_briefing_as_of = Some(Instant::now());
                    self.cached_briefing_warning = None;
                }
                Err(err) => {
                    self.cached_briefing_warning = Some(err);
                }
            }
        }
    }

    /// Discovers and reloads history sessions from Claude Code and Antigravity logs.
    pub fn reload_history_sessions(&mut self) {
        let cur_dir = self
            .sessions
            .get(self.selected)
            .map(|s| s.dir.clone())
            .or_else(|| std::env::current_dir().ok());
        let mut sessions =
            history::discover_sessions(None, None, cur_dir.as_deref(), self.history_all_projects);
        if sessions.is_empty() && !self.history_all_projects {
            sessions = history::discover_sessions(None, None, cur_dir.as_deref(), true);
        }
        self.history_sessions = sessions;
        if self.selected_history >= self.history_sessions.len() {
            self.selected_history = self.history_sessions.len().saturating_sub(1);
        }
    }

    /// Persists active (non-exited) sessions to disk so they can be restored on restart.
    pub fn save_active_sessions(&self) -> anyhow::Result<()> {
        let Some(path) = self
            .sessions_file
            .clone()
            .or_else(persistence::sessions_file_path)
        else {
            return Ok(());
        };
        let now = Instant::now();
        let saved: Vec<persistence::SavedSession> = self
            .sessions
            .iter()
            .filter(|s| !matches!(s.status(now), Status::Exited(_)))
            .map(|s| persistence::SavedSession {
                profile: s.profile.clone(),
                dir: s.dir.clone(),
                skill_id: s.skill_id.clone(),
            })
            .collect();
        persistence::save_sessions(&path, &saved)?;
        Ok(())
    }

    /// Restores saved sessions from disk upon application startup.
    pub fn restore_saved_sessions(&mut self) {
        let Some(path) = self
            .sessions_file
            .clone()
            .or_else(persistence::sessions_file_path)
        else {
            return;
        };
        let saved = persistence::load_saved_sessions(&path);
        if saved.is_empty() {
            return;
        }
        for s in saved {
            let dir = if s.dir.is_dir() {
                s.dir
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd
            } else {
                continue;
            };
            let id = self.next_id;
            match self.spawn_traced_with_env(id, s.profile, dir, &[], s.skill_id.as_deref()) {
                Ok(mut session) => {
                    session.skill_id = s.skill_id;
                    self.next_id += 1;
                    self.sessions.push(session);
                }
                Err(e) => {
                    self.notice = Some(Notice::warn(format!("Failed to restore session: {e}")));
                }
            }
        }
        if !self.sessions.is_empty() {
            self.selected = 0;
            self.sidebar_section = SidebarSection::Active;
        } else if !self.history_sessions.is_empty() {
            self.sidebar_section = SidebarSection::History;
        }
        let _ = self.save_active_sessions();
    }

    /// Spawns a session with tracing launch extras applied and the tracing
    /// pipeline attached on success. The single spawn path for all three
    /// call sites (dialog, resume, respawn); with tracing off it degrades to
    /// a plain `Session::spawn`.
    fn spawn_traced(
        &mut self,
        id: usize,
        profile: Profile,
        dir: std::path::PathBuf,
    ) -> anyhow::Result<Session> {
        self.spawn_traced_with_env(id, profile, dir, &[], None)
    }

    /// Like `spawn_traced`, with extra environment for the child (a skill
    /// launch's declared variables) merged after the tracing plan's own,
    /// and the skill id recorded on the launch row when there is one.
    fn spawn_traced_with_env(
        &mut self,
        id: usize,
        profile: Profile,
        dir: std::path::PathBuf,
        env: &[(String, String)],
        skill_id: Option<&str>,
    ) -> anyhow::Result<Session> {
        self.spawn_traced_full(id, profile, dir, env, skill_id, None, None, &[])
    }

    /// The one spawn path: `spawn_traced_with_env` plus, for a loop run,
    /// the launch row's loop keys and policy and extra harness arguments
    /// (the MCP registration the loop composed itself).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_traced_full(
        &mut self,
        id: usize,
        mut profile: Profile,
        dir: std::path::PathBuf,
        env: &[(String, String)],
        skill_id: Option<&str>,
        loop_launch: Option<crate::loops::LoopLaunch>,
        workflow_launch: Option<crate::workflows::WorkflowLaunch>,
        loop_args: &[String],
    ) -> anyhow::Result<Session> {
        prepare_nested_tui(&mut profile);
        let (rows, cols) = self.pane_size;
        let mut plan = self
            .tracing
            .as_ref()
            .and_then(|rt| rt.plan_launch(&profile, &dir));
        if let (Some(p), Some(skill)) = (plan.as_mut(), skill_id) {
            let harness = crate::harness::Harness::detect(&profile.command)
                .map(|h| h.as_str())
                .unwrap_or("unknown");
            p.skill = Some((skill.to_string(), harness.to_string()));
        }
        let is_loop = loop_launch.is_some() || workflow_launch.is_some();
        if let (Some(p), Some(w)) = (plan.as_mut(), workflow_launch) {
            p.workflow = Some(w);
        }
        if let (Some(p), Some(launch)) = (plan.as_mut(), loop_launch) {
            let home = self.skill_home();
            p.attach_loop(launch, &home);
        }
        let mut extra_args: Vec<String> = match &plan {
            Some(p) => p.extra_args.clone(),
            None => Vec::new(),
        };
        extra_args.extend(loop_args.iter().cloned());
        let mut extra_env: Vec<(String, String)> = match &plan {
            Some(p) => p.extra_env.clone(),
            None => Vec::new(),
        };
        extra_env.extend(env.iter().cloned());
        // An agent launch (fresh, restored or respawned) gets its facts
        // from Rust: the briefing snapshot and the MCP registration.
        let mut briefing_path = None;
        if let Some(def) = skill_id
            .filter(|_| !is_loop)
            .and_then(|sid| self.skills.iter().find(|s| s.id == sid))
            .cloned()
        {
            let prep = self.prepare_agent_launch(&def, &profile, &dir);
            extra_args.extend(prep.args);
            extra_env.extend(prep.env);
            briefing_path = prep.briefing_path;
        }
        let mut session = Session::spawn(
            id,
            profile,
            dir,
            rows,
            cols,
            self.tx.clone(),
            &extra_args,
            &extra_env,
        )?;
        session.briefing_path = briefing_path;
        if let (Some(rt), Some(plan)) = (self.tracing.as_mut(), plan) {
            session.trace = Some(rt.start_session(id, plan));
        }
        Ok(session)
    }

    /// The Rust side of an agent launch: writes the briefing snapshot the
    /// package asked for and decides how the harness reaches the MCP
    /// server. Nothing here can fail the launch; problems become notices.
    fn prepare_agent_launch(
        &mut self,
        def: &crate::skill::SkillDefinition,
        profile: &Profile,
        dir: &std::path::Path,
    ) -> AgentLaunchPrep {
        let mut prep = AgentLaunchPrep::default();
        if self.agents.hydrate && !def.hydrate.is_empty() {
            let key = uuid::Uuid::new_v4().to_string();
            let runtime_dir = self
                .runtime_dir
                .clone()
                .unwrap_or_else(crate::tracing::analysis::default_snapshot_dir);
            let home = self.skill_home();
            if let Some(h) = crate::skill::launch::hydrate(
                def,
                self.trace_db_path.as_deref(),
                dir,
                &home,
                &runtime_dir,
                &key,
            ) {
                prep.env.push((
                    "AGENT_MUX_BRIEFING".into(),
                    h.path.to_string_lossy().into_owned(),
                ));
                prep.env
                    .push(("AGENT_MUX_BRIEFING_AS_OF".into(), h.as_of.clone()));
                prep.env.push((
                    "AGENT_MUX_BRIEFING_SCHEMA".into(),
                    h.schema_version.to_string(),
                ));
                if let Some(e) = &h.error {
                    self.notice =
                        Some(Notice::warn(format!("{} briefing snapshot: {e}", def.name)));
                }
                prep.briefing_path = Some(h.path);
            }
        }
        let wanted = def.mcp == crate::skill::McpMode::Auto
            && self.agents.mcp == crate::skill::McpMode::Auto;
        let registration = if wanted {
            let exe = crate::tracing::hooks::register::current_exe();
            crate::mcp::register::plan(
                crate::harness::Harness::detect(&profile.command),
                exe.as_deref(),
                self.trace_db_path.as_deref(),
                dir,
                &self.skill_home(),
            )
        } else {
            crate::mcp::register::Registration::Unavailable("mcp is off".into())
        };
        match &registration {
            crate::mcp::register::Registration::PerLaunch { args } => {
                prep.args.extend(args.iter().cloned());
            }
            crate::mcp::register::Registration::Installed => {}
            crate::mcp::register::Registration::Unavailable(why) if wanted => {
                self.notice = Some(Notice::info(format!(
                    "{} runs without MCP tools: {why}",
                    def.name
                )));
            }
            crate::mcp::register::Registration::Unavailable(_) => {}
        }
        prep.env
            .push(("AGENT_MUX_MCP".into(), registration.env_value().into()));
        prep.env.push((
            "AGENT_MUX_WORKSPACE".into(),
            dir.to_string_lossy().into_owned(),
        ));
        prep
    }

    /// Hands the runtime back to `main` for the post-`kill_all` bounded
    /// shutdown flush.
    /// Spawns a session from a ready profile, the way the dialog does, and
    /// returns its index. The headless runner's entry point.
    pub fn launch(&mut self, profile: Profile, dir: std::path::PathBuf) -> anyhow::Result<usize> {
        let id = self.next_id;
        let session = self.spawn_traced(id, profile, dir)?;
        self.next_id += 1;
        self.sessions.push(session);
        self.selected = self.sessions.len() - 1;
        Ok(self.selected)
    }

    pub fn take_tracing(&mut self) -> Option<crate::tracing::TraceRuntime> {
        self.tracing.take()
    }

    /// Routes a live rollup from the store writer to the session it
    /// belongs to (by launch id); unknown launches are ignored.
    pub fn handle_trace_stats(
        &mut self,
        launch_id: &str,
        stats: crate::tracing::store::query::LaunchStats,
    ) {
        if let Some(s) = self
            .sessions
            .iter_mut()
            .find(|s| s.trace.as_ref().is_some_and(|t| t.launch_id == launch_id))
        {
            s.trace_stats = Some(stats);
        }
    }

    pub fn attached(&self) -> Option<usize> {
        matches!(self.mode, Mode::Attached).then_some(self.selected)
    }

    fn session_index(&self, id: usize) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// The selection, but only if it belongs to the session currently shown
    /// in the main pane -- selections on removed/replaced/other sessions
    /// are treated as gone.
    pub fn displayed_selection(&self) -> Option<&Selection> {
        let shown = self.sessions.get(self.selected)?.id;
        self.selection
            .as_ref()
            .filter(|a| a.session_id == shown)
            .map(|a| &a.sel)
    }

    /// The cursor style that should currently be applied to the terminal.
    /// When attached, follows the active session's DECSCUSR requests (or
    /// DefaultUserShape). When detached/in dialog/in browser, uses DefaultUserShape.
    pub fn active_cursor_style(&self) -> crossterm::cursor::SetCursorStyle {
        if matches!(self.mode, Mode::Attached) {
            self.sessions
                .get(self.selected)
                .and_then(|s| s.cursor_style())
                .unwrap_or(crossterm::cursor::SetCursorStyle::DefaultUserShape)
        } else {
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        }
    }

    fn copy_to_clipboard(&mut self, text: String) {
        if text.is_empty() || !self.clipboard_enabled {
            return;
        }
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            self.notice = Some(Notice::error(format!("clipboard: {e}")));
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.displayed_selection().copied() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        let text = selection::extract_text(&mut s.parser, len, &sel);
        self.copy_to_clipboard(text);
    }

    fn paste_into_attached(&mut self) {
        if !matches!(self.mode, Mode::Attached) {
            return;
        }
        if !self.clipboard_enabled {
            return;
        }
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(Notice::error(format!("clipboard: {e}")));
                return;
            }
        };
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        let bytes = paste_bytes(&text, s.parser.screen().bracketed_paste());
        self.snap_selected_to_live();
        self.forward_bytes(&bytes);
    }

    pub fn handle_key(&mut self, key: &KeyEvent, now: Instant) {
        self.notice = None;
        if self.search.is_some() {
            self.handle_search_key(key);
            return;
        }
        if self.handle_ux_key(key) {
            self.just_detached = false;
            return;
        }
        let app_cursor = self
            .sessions
            .get(self.selected)
            .map(|s| s.parser.screen().application_cursor())
            .unwrap_or(false);
        let ctx = DispatchCtx {
            selected_status: self.sessions.get(self.selected).map(|s| s.status(now)),
            any_working: self
                .sessions
                .iter()
                .any(|s| matches!(s.status(now), Status::Working)),
            just_detached: self.just_detached,
            app_cursor,
            sidebar_section: self.sidebar_section,
            sidebar_hidden: self.sidebar_hidden,
        };
        let action = dispatch(&self.mode, key, &ctx);
        // any Control-mode key other than the literal-send consumes the flag
        if !matches!(action, Action::Detach | Action::SendLiteralDetachKey) {
            self.just_detached = false;
        }
        self.apply(action, key, now);
    }

    /// Terminal-emulator chords intercepted before v1 dispatch (Ghostty /
    /// Windows Terminal convention: the app reserves Ctrl+Shift and
    /// Shift+navigation for itself; everything else still reaches the
    /// agent). Returns true if the key was consumed. Tasks: selection
    /// (Ctrl+Shift+C/V) and search (Ctrl+Shift+F) add arms here.
    fn handle_ux_key(&mut self, key: &KeyEvent) -> bool {
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        enum Chord {
            LineUp,
            LineDown,
            PageUp,
            PageDown,
            Top,
            Bottom,
            Copy,
            Paste,
            OpenSearch,
            ToggleSidebar,
        }
        let chord = match (key.code, shift, ctrl) {
            (KeyCode::Up, true, _) => Chord::LineUp,
            (KeyCode::Down, true, _) => Chord::LineDown,
            // macOS keyboards commonly report Fn+Up/Fn+Down as an
            // unmodified PageUp/PageDown. These are documented app-level
            // scroll shortcuts, so accept them with or without Shift.
            (KeyCode::PageUp, _, _) => Chord::PageUp,
            (KeyCode::PageDown, _, _) => Chord::PageDown,
            (KeyCode::Home, true, _) => Chord::Top,
            (KeyCode::End, true, _) => Chord::Bottom,
            (KeyCode::Char('c') | KeyCode::Char('C'), true, true) => Chord::Copy,
            (KeyCode::Char('v') | KeyCode::Char('V'), true, true) => Chord::Paste,
            (KeyCode::Char('f') | KeyCode::Char('F'), true, true) => Chord::OpenSearch,
            // plain Ctrl+F only when nothing is forwarded (Control mode)
            (KeyCode::Char('f') | KeyCode::Char('F'), false, true)
                if matches!(self.mode, Mode::Control) =>
            {
                Chord::OpenSearch
            }
            (KeyCode::Char('b') | KeyCode::Char('B'), true, true) => Chord::ToggleSidebar,
            _ => return false,
        };
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        match chord {
            Chord::LineUp => self.scroll_selected(3),
            Chord::LineDown => self.scroll_selected(-3),
            Chord::PageUp => self.scroll_selected(page),
            Chord::PageDown => self.scroll_selected(-page),
            Chord::Top => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_top();
                }
            }
            Chord::Bottom => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            Chord::Copy => self.copy_selection(),
            Chord::Paste => self.paste_into_attached(),
            Chord::OpenSearch => {
                self.search = Some(SearchState::new());
            }
            Chord::ToggleSidebar => self.toggle_sidebar(),
        }
        true
    }

    fn handle_search_key(&mut self, key: &KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => {
                self.search = None;
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.scroll_to_bottom();
                }
            }
            KeyCode::Enter => {
                if let Some(st) = self.search.as_mut() {
                    if shift {
                        st.prev();
                    } else {
                        st.next();
                    }
                }
                self.scroll_to_current_match();
            }
            KeyCode::Backspace => {
                if let Some(st) = self.search.as_mut() {
                    st.query.pop();
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            KeyCode::Char(c) => {
                // Ctrl-modified chars (e.g. Ctrl+Shift+F reopening the
                // chord, or any other Ctrl+letter) must not pollute the
                // query -- only plain/Shift'd character input edits it.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                if let Some(st) = self.search.as_mut() {
                    st.query.push(c);
                }
                self.rerun_search();
                self.scroll_to_current_match();
            }
            _ => {}
        }
    }

    fn rerun_search(&mut self) {
        let Some(st) = self.search.as_mut() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        st.run(&mut s.parser, len);
    }

    fn scroll_to_current_match(&mut self) {
        let Some(row) = self
            .search
            .as_ref()
            .and_then(|st| st.current_match().map(|m| m.row))
        else {
            return;
        };
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        let (len, _) = s.scroll_view();
        // live rows need no scrolling; scrollback rows go to the top of view
        let offset = len.saturating_sub(row);
        s.set_scroll(offset);
    }

    fn scroll_selected(&mut self, delta: i32) {
        if let Some(s) = self.sessions.get_mut(self.selected) {
            s.scroll_by(delta);
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, _now: Instant) {
        if let Mode::TraceBrowser(ref mut browser) = self.mode {
            let delta = match ev.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            if browser.expanded {
                if delta < 0 {
                    browser.scroll_offset = browser.scroll_offset.saturating_sub(3);
                } else {
                    browser.scroll_offset = browser
                        .scroll_offset
                        .saturating_add(3)
                        .min(browser.max_scroll());
                }
                return;
            }
            // List views are selection-driven (sidebar_window follows the
            // selected row), so moving detail_lines' scroll_offset had no
            // visible effect. Scroll the focused list instead.
            match browser.focused {
                BrowserPane::Sessions => {
                    if !browser.sessions.is_empty() {
                        let max = browser.sessions.len() as isize - 1;
                        let next =
                            (browser.selected_session as isize + delta).clamp(0, max) as usize;
                        if next != browser.selected_session {
                            browser.selected_session = next;
                            browser.search_query = None;
                            browser.load_turns();
                        }
                    }
                }
                BrowserPane::Turns => {
                    if !browser.turns.is_empty() {
                        let max = browser.turns.len() as isize - 1;
                        let next = (browser.selected_turn as isize + delta).clamp(0, max) as usize;
                        if next != browser.selected_turn {
                            browser.selected_turn = next;
                            browser.load_observations();
                        }
                    }
                }
                BrowserPane::Detail => browser.step_observation(delta),
            }
            return;
        }
        if let Mode::SkillsView(ref mut view) = self.mode {
            let delta = match ev.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            match (view.focus, view.tab) {
                (SkillsPane::Skills, _) => view.step(delta),
                (SkillsPane::Detail, SkillsTab::Executions) => view.step_execution(delta),
                (SkillsPane::Detail, _) => {
                    view.scroll_offset = if delta < 0 {
                        view.scroll_offset.saturating_sub(3)
                    } else {
                        view.scroll_offset.saturating_add(3).min(view.max_scroll())
                    };
                }
            }
            return;
        }
        if matches!(self.mode, Mode::WorkflowsView(_)) {
            let delta = match ev.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            self.scroll_workflows_view(delta);
            return;
        }
        if let Mode::ConfigView(ref mut view) = self.mode {
            let delta = match ev.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            match view.focus {
                ConfigPane::List => view.step(delta, &self.loop_registry),
                ConfigPane::Detail => view.scroll(delta),
            }
            return;
        }
        if let Mode::SessionHistory(ref mut history) = self.mode {
            if matches!(ev.kind, MouseEventKind::ScrollUp) {
                history.scroll_offset = history.scroll_offset.saturating_sub(3);
            } else if matches!(ev.kind, MouseEventKind::ScrollDown) {
                history.scroll_offset = history
                    .scroll_offset
                    .saturating_add(3)
                    .min(history.max_scroll());
            }
            return;
        }
        // Mouse handling is scoped to Control (sidebar preview scroll) and
        // Attached (the pane showing a live child): during the NewSession
        // dialog or a Confirm prompt the pane underneath must not react to
        // clicks/drags/wheel meant for the dialog.
        if !matches!(self.mode, Mode::Control | Mode::Attached) {
            return;
        }
        // Click on the status bar sidebar toggle button "[b] sidebar"
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && ev.row >= self.terminal_size.0.saturating_sub(1)
            && ev.column <= 12
        {
            self.toggle_sidebar();
            return;
        }
        // Left click on a sidebar row selects that session (and follows it
        // when attached). Only a fresh Down, never a drag that slid over.
        if !self.sidebar_hidden
            && matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self.selection.as_ref().is_none_or(|a| !a.dragging)
            && ev.column > 0
            && ev.column < ui::SIDEBAR_WIDTH.saturating_sub(1)
        {
            let (active_rect, agents_rect, loops_rect, workflows_rect, history_rect) =
                ui::sidebar_areas(
                    self.pane_size.0 + 3,
                    self.skills.len(),
                    self.loop_registry.loops.len(),
                    self.workflow_list.len(),
                );
            if ev.row >= active_rect.y && ev.row < active_rect.y + active_rect.height {
                if ev.row > active_rect.y
                    && ev.row < active_rect.y + active_rect.height.saturating_sub(1)
                {
                    let visible = usize::from(active_rect.height.saturating_sub(2));
                    let row = usize::from(ev.row - active_rect.y - 1);
                    let idx = ui::sidebar_window(self.selected, self.sessions.len(), visible) + row;
                    if idx < self.sessions.len() {
                        self.sidebar_section = SidebarSection::Active;
                        if idx != self.selected {
                            self.selection = None;
                            self.selected = idx;
                            if matches!(self.mode, Mode::Attached)
                                && let Some(s) = self.sessions.get_mut(idx)
                            {
                                s.tracker.on_attach();
                            }
                        }
                    }
                }
                return;
            } else if ev.row >= agents_rect.y && ev.row < agents_rect.y + agents_rect.height {
                self.sidebar_section = SidebarSection::Agents;
                if ev.row > agents_rect.y
                    && ev.row < agents_rect.y + agents_rect.height.saturating_sub(1)
                {
                    let visible = usize::from(agents_rect.height.saturating_sub(2));
                    let row = usize::from(ev.row - agents_rect.y - 1);
                    let idx =
                        ui::sidebar_window(self.selected_agent, self.skills.len(), visible) + row;
                    if idx < self.skills.len() {
                        self.selected_agent = idx;
                    }
                }
                return;
            } else if ev.row >= workflows_rect.y
                && ev.row < workflows_rect.y + workflows_rect.height
            {
                if ev.row > workflows_rect.y
                    && ev.row < workflows_rect.y + workflows_rect.height.saturating_sub(1)
                {
                    let visible = usize::from(workflows_rect.height.saturating_sub(2));
                    let row = usize::from(ev.row - workflows_rect.y - 1);
                    let idx = ui::sidebar_window(
                        self.selected_workflow,
                        self.workflow_list.len(),
                        visible,
                    ) + row;
                    if idx < self.workflow_list.len() {
                        self.sidebar_section = SidebarSection::Workflows;
                        self.selected_workflow = idx;
                    }
                }
                return;
            }
            if ev.row >= loops_rect.y && ev.row < loops_rect.y + loops_rect.height {
                self.sidebar_section = SidebarSection::Loops;
                if ev.row > loops_rect.y
                    && ev.row < loops_rect.y + loops_rect.height.saturating_sub(1)
                {
                    let visible = usize::from(loops_rect.height.saturating_sub(2));
                    let row = usize::from(ev.row - loops_rect.y - 1);
                    let n = self.loop_registry.loops.len();
                    let idx = ui::sidebar_window(self.selected_loop, n, visible) + row;
                    if idx < n {
                        self.selected_loop = idx;
                    }
                }
                return;
            } else if ev.row >= history_rect.y && ev.row < history_rect.y + history_rect.height {
                if ev.row > history_rect.y
                    && ev.row < history_rect.y + history_rect.height.saturating_sub(1)
                {
                    let visible = usize::from(history_rect.height.saturating_sub(2));
                    let row = usize::from(ev.row - history_rect.y - 1);
                    let idx = ui::sidebar_window(
                        self.selected_history,
                        self.history_sessions.len(),
                        visible,
                    ) + row;
                    if idx < self.history_sessions.len() {
                        self.sidebar_section = SidebarSection::History;
                        self.selected_history = idx;
                    }
                }
                return;
            }
        }
        // A live left-button drag must be able to finish even if the
        // terminating Drag/Up event lands outside the pane (e.g. the mouse
        // slid into the adjacent sidebar): clamp into the pane instead of
        // dropping the event, so the drag can't get stranded with
        // `dragging: true` and a frozen highlight. Any other out-of-pane
        // event is still dropped -- the sidebar stays keyboard-driven.
        // Keyed to an active LOCAL drag: `self.selection`'s `dragging` flag
        // is only ever set true from the local (non-agent-owned) branch
        // below, so this is already local-drag-specific.
        let dragging = self.selection.as_ref().is_some_and(|a| a.dragging);
        let finalizing_drag = dragging
            && matches!(
                ev.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            );
        let pane_pos =
            ui::pane_local_with_sidebar(ev.column, ev.row, self.pane_size, self.sidebar_hidden);
        let (lcol, lrow) = match pane_pos {
            Some(p) => p,
            None if finalizing_drag => ui::pane_clamped_with_sidebar(
                ev.column,
                ev.row,
                self.pane_size,
                self.sidebar_hidden,
            ),
            // Wheel gestures should scroll the selected session even when
            // the pointer is over the sidebar. This is especially important
            // for macOS trackpads: the pointer often stays parked over the
            // session list while the user performs a two-finger scroll.
            // Child mouse protocols remain pane-scoped, so an out-of-pane
            // wheel is always handled locally.
            None if matches!(
                ev.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) =>
            {
                if !self.sidebar_hidden && ev.column < ui::SIDEBAR_WIDTH {
                    let (_, agents_rect, loops_rect, _workflows_rect, history_rect) =
                        ui::sidebar_areas(
                            self.pane_size.0 + 3,
                            self.skills.len(),
                            self.loop_registry.loops.len(),
                            self.workflow_list.len(),
                        );
                    if ev.row >= loops_rect.y
                        && ev.row < loops_rect.y + loops_rect.height
                        && !self.loop_registry.loops.is_empty()
                    {
                        if matches!(ev.kind, MouseEventKind::ScrollUp) {
                            self.selected_loop = self.selected_loop.saturating_sub(1);
                        } else {
                            self.selected_loop =
                                (self.selected_loop + 1).min(self.loop_registry.loops.len() - 1);
                        }
                        return;
                    }
                    if ev.row >= agents_rect.y
                        && ev.row < agents_rect.y + agents_rect.height
                        && !self.skills.is_empty()
                    {
                        if matches!(ev.kind, MouseEventKind::ScrollUp) {
                            self.selected_agent = self.selected_agent.saturating_sub(1);
                        } else {
                            self.selected_agent =
                                (self.selected_agent + 1).min(self.skills.len() - 1);
                        }
                        return;
                    }
                    if ev.row >= history_rect.y && !self.history_sessions.is_empty() {
                        let delta = if matches!(ev.kind, MouseEventKind::ScrollUp) {
                            -1
                        } else {
                            1
                        };
                        if delta > 0 {
                            self.selected_history =
                                (self.selected_history + 1).min(self.history_sessions.len() - 1);
                        } else {
                            self.selected_history = self.selected_history.saturating_sub(1);
                        }
                        return;
                    }
                }
                let delta = if matches!(ev.kind, MouseEventKind::ScrollUp) {
                    3
                } else {
                    -3
                };
                self.scroll_selected(delta);
                return;
            }
            None => return,
        };
        let attached = matches!(self.mode, Mode::Attached);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        // read child terminal state up front so no borrow is held across
        // the mutating calls below
        let Some((mouse_mode, enc, alt, app_cursor, codex_local_scroll)) =
            self.sessions.get(self.selected).map(|s| {
                let sc = s.parser.screen();
                let configured_provider = s
                    .profile
                    .tracing
                    .as_ref()
                    .and_then(|t| t.provider.as_deref());
                let codex = configured_provider == Some("codex")
                    || crate::harness::Harness::detect(&s.profile.command)
                        == Some(crate::harness::Harness::Codex);
                (
                    sc.mouse_protocol_mode(),
                    sc.mouse_protocol_encoding(),
                    sc.alternate_screen(),
                    sc.application_cursor(),
                    codex,
                )
            })
        else {
            return;
        };
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                match route_wheel(shift, attached, mouse_mode, alt, codex_local_scroll) {
                    WheelRoute::Local => {
                        if let Some(s) = self.sessions.get_mut(self.selected) {
                            s.scroll_by(if up { 3 } else { -3 });
                        }
                    }
                    WheelRoute::Forward => {
                        if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                            self.forward_bytes(&bytes);
                        }
                    }
                    WheelRoute::Arrows => {
                        let seq: &[u8] = match (up, app_cursor) {
                            (true, false) => b"\x1b[A",
                            (true, true) => b"\x1bOA",
                            (false, false) => b"\x1b[B",
                            (false, true) => b"\x1bOB",
                        };
                        self.forward_bytes(&seq.repeat(3));
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left) => {
                let is_up = matches!(ev.kind, MouseEventKind::Up(_));
                // Ownership is decided fresh at Down (agent owns the mouse
                // when attached + it asked for events, unless Shift forces
                // local selection -- the iTerm2 rule) and then latched in
                // `drag_owner` for the rest of the drag. Without this, a
                // modifier change mid-drag (e.g. releasing Shift) would let
                // this same computation -- re-run per event, from that
                // event's own shift flag -- retarget the Drag/Up to the
                // child, stranding a local drag with `dragging: true` still
                // set (and, worse, a later unrelated out-of-pane Up could
                // then clamp and finalize that stale selection). Latching
                // at Down means Drag/Up always follow where the drag
                // started, regardless of the shift flag on those events.
                let agent_owns = match ev.kind {
                    MouseEventKind::Down(_) => {
                        let owns =
                            attached && !shift && mouse_mode != vt100::MouseProtocolMode::None;
                        self.drag_owner = Some(if owns {
                            DragOwner::Agent
                        } else {
                            DragOwner::Local
                        });
                        owns
                    }
                    _ => matches!(self.drag_owner, Some(DragOwner::Agent)),
                };
                if agent_owns {
                    if let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc) {
                        self.forward_bytes(&bytes);
                    }
                } else if let Some(s) = self.sessions.get(self.selected) {
                    let (len, offset) = s.scroll_view();
                    let session_id = s.id;
                    let pos = Pos {
                        row: selection::abs_row(len, offset, lrow),
                        col: lcol,
                    };
                    match ev.kind {
                        MouseEventKind::Down(_) => {
                            let alt = ev.modifiers.contains(KeyModifiers::ALT);
                            if attached && alt && offset == 0 {
                                let (cur_row, cur_col) = s.parser.screen().cursor_position();
                                let mut seq = Vec::new();
                                if lrow < cur_row {
                                    seq.extend(b"\x1b[A".repeat((cur_row - lrow) as usize));
                                } else if lrow > cur_row {
                                    seq.extend(b"\x1b[B".repeat((lrow - cur_row) as usize));
                                }
                                if lcol < cur_col {
                                    seq.extend(b"\x1b[D".repeat((cur_col - lcol) as usize));
                                } else if lcol > cur_col {
                                    seq.extend(b"\x1b[C".repeat((lcol - cur_col) as usize));
                                }
                                if !seq.is_empty() {
                                    self.forward_bytes(&seq);
                                }
                                return;
                            }
                            self.selection = Some(ActiveSelection {
                                session_id,
                                sel: Selection::new(pos),
                                dragging: true,
                            });
                        }
                        MouseEventKind::Drag(_) => {
                            if let Some(a) = self.selection.as_mut().filter(|a| a.dragging) {
                                a.sel.head = pos;
                            }
                        }
                        _ => {
                            // release: apply the final position (a
                            // throttled terminal may not have sent a Drag
                            // for it), finish the drag, then copy-on-select
                            // or clear
                            let finished = self.selection.take_if(|a| a.dragging);
                            if let Some(mut a) = finished {
                                a.sel.head = pos;
                                a.dragging = false;
                                if a.sel.is_empty() {
                                    // plain click: selection stays cleared
                                } else {
                                    self.selection = Some(a);
                                    self.copy_selection();
                                }
                            }
                        }
                    }
                }
                if is_up {
                    self.drag_owner = None;
                }
            }
            _ => {
                // other buttons: forward when the agent owns the mouse
                if attached
                    && !shift
                    && let Some(bytes) = encode_mouse(ev.kind, lcol, lrow, mouse_mode, enc)
                {
                    self.forward_bytes(&bytes);
                }
            }
        }
    }

    fn apply(&mut self, action: Action, key: &KeyEvent, _now: Instant) {
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::EnterConfirmQuit => self.mode = Mode::ConfirmQuit,
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::MoveDown => {
                self.selection = None;
                if self.sidebar_hidden {
                    if !self.sessions.is_empty() {
                        self.selected = (self.selected + 1).min(self.sessions.len() - 1);
                    }
                } else {
                    match self.sidebar_section {
                        SidebarSection::Active => {
                            if !self.sessions.is_empty() && self.selected + 1 < self.sessions.len()
                            {
                                self.selected += 1;
                            } else {
                                self.sidebar_section = SidebarSection::Agents;
                                self.selected_agent = 0;
                            }
                        }
                        SidebarSection::Agents => {
                            if !self.skills.is_empty()
                                && self.selected_agent + 1 < self.skills.len()
                            {
                                self.selected_agent += 1;
                            } else {
                                self.sidebar_section = SidebarSection::Loops;
                                self.selected_loop = 0;
                            }
                        }
                        SidebarSection::Loops => {
                            if !self.loop_registry.loops.is_empty()
                                && self.selected_loop + 1 < self.loop_registry.loops.len()
                            {
                                self.selected_loop += 1;
                            } else {
                                self.sidebar_section = SidebarSection::Workflows;
                                self.reload_workflow_list();
                                self.selected_workflow = 0;
                            }
                        }
                        SidebarSection::Workflows => {
                            if !self.workflow_list.is_empty()
                                && self.selected_workflow + 1 < self.workflow_list.len()
                            {
                                self.selected_workflow += 1;
                            } else if !self.history_sessions.is_empty() {
                                self.sidebar_section = SidebarSection::History;
                                self.selected_history = 0;
                            }
                        }
                        SidebarSection::History => {
                            if !self.history_sessions.is_empty() {
                                self.selected_history = (self.selected_history + 1)
                                    .min(self.history_sessions.len() - 1);
                            }
                        }
                    }
                }
            }
            Action::MoveUp => {
                self.selection = None;
                if self.sidebar_hidden {
                    self.selected = self.selected.saturating_sub(1);
                } else {
                    match self.sidebar_section {
                        SidebarSection::Active => {
                            self.selected = self.selected.saturating_sub(1);
                        }
                        SidebarSection::Agents => {
                            if self.selected_agent > 0 {
                                self.selected_agent -= 1;
                            } else if !self.sessions.is_empty() {
                                self.sidebar_section = SidebarSection::Active;
                                self.selected = self.sessions.len() - 1;
                            }
                        }
                        SidebarSection::Loops => {
                            if self.selected_loop > 0 {
                                self.selected_loop -= 1;
                            } else {
                                self.sidebar_section = SidebarSection::Agents;
                                self.selected_agent = self.skills.len().saturating_sub(1);
                            }
                        }
                        SidebarSection::Workflows => {
                            if self.selected_workflow > 0 {
                                self.selected_workflow -= 1;
                            } else {
                                self.sidebar_section = SidebarSection::Loops;
                                self.selected_loop =
                                    self.loop_registry.loops.len().saturating_sub(1);
                            }
                        }
                        SidebarSection::History => {
                            if self.selected_history > 0 {
                                self.selected_history -= 1;
                            } else {
                                self.sidebar_section = SidebarSection::Workflows;
                                self.reload_workflow_list();
                                self.selected_workflow = self.workflow_list.len().saturating_sub(1);
                            }
                        }
                    }
                }
            }
            Action::ToggleSidebarSection => {
                if self.sidebar_hidden {
                    if !self.sessions.is_empty() {
                        self.selected = (self.selected + 1) % self.sessions.len();
                    }
                } else {
                    self.sidebar_section = match self.sidebar_section {
                        SidebarSection::Active => {
                            self.reload_skills();
                            SidebarSection::Agents
                        }
                        SidebarSection::Agents => SidebarSection::Loops,
                        SidebarSection::Loops => {
                            self.reload_workflow_list();
                            SidebarSection::Workflows
                        }
                        SidebarSection::Workflows => SidebarSection::History,
                        SidebarSection::History => SidebarSection::Active,
                    };
                }
            }
            Action::OpenAbout => self.open_about(),
            Action::AboutKey => {
                if let Mode::About(state) = &mut self.mode {
                    let page = state.viewport_rows.get().max(1) as isize;
                    match key.code {
                        KeyCode::Down | KeyCode::Char('j') => state.scroll(1),
                        KeyCode::Up | KeyCode::Char('k') => state.scroll(-1),
                        KeyCode::PageDown | KeyCode::Char(' ') => state.scroll(page),
                        KeyCode::PageUp => state.scroll(-page),
                        KeyCode::Home => state.scroll_offset = 0,
                        KeyCode::End => state.scroll_offset = state.max_scroll(),
                        _ => {}
                    }
                }
            }
            Action::OpenLoopsView => self.open_loops_view(),
            Action::LoopsKey => self.handle_loops_view_key(key),
            Action::LoopDialogKey => self.handle_loop_dialog_key(key),
            Action::LoopRunNow => self.run_selected_loop_now(),
            Action::LoopTogglePause => self.toggle_selected_loop_pause(),
            Action::OpenNewLoop => self.open_loop_dialog(None),
            Action::EditLoop => {
                let id = self.selected_loop().map(|l| l.id.clone());
                if id.is_some() {
                    self.open_loop_dialog(id);
                }
            }
            Action::EnterConfirmRemoveLoop => {
                if matches!(self.mode, Mode::ConfirmRemoveLoop) {
                    self.remove_selected_loop();
                    self.mode = Mode::Control;
                } else if self.selected_loop().is_some() {
                    self.mode = Mode::ConfirmRemoveLoop;
                }
            }
            Action::ToggleKillSwitch => self.toggle_kill_switch(),
            Action::OpenSkillsView => self.open_skills_view(),
            Action::SkillsKey => self.handle_skills_key(key),
            Action::OpenConfigView => self.open_config_view(),
            Action::ConfigKey => self.handle_config_key(key),
            Action::OpenWorkflowRun => self.open_workflow_run(),
            Action::OpenWorkflowPlan => self.open_workflow_plan(),
            Action::EditWorkflow => self.edit_selected_workflow(),
            Action::CancelWorkflow => self.cancel_selected_workflow(),
            Action::OpenWorkflowsView => self.open_workflows_view(),
            Action::WorkflowsKey => self.handle_workflows_view_key(key),
            Action::WorkflowDialogKey => self.handle_workflow_dialog_key(key),
            Action::OpenSkillLauncher => {
                let Some(skill) = self.selected_agent() else {
                    return;
                };
                // Singleton: a running agent session is attached to, never
                // relaunched; the harness can only change once it is closed.
                if let Some(idx) = self.running_skill_session(&skill.id) {
                    self.attach_to_session(idx);
                } else {
                    let state = SkillLauncherState::for_skill(skill);
                    self.mode = Mode::SkillLauncher(state);
                }
            }
            Action::SkillLauncherKey => self.handle_skill_launcher_key(key),
            Action::RestartHistorySession => {
                if let Some(summary) = self.history_sessions.get(self.selected_history).cloned() {
                    self.resume_history_session(&summary);
                }
            }
            Action::ToggleHistoryAllProjects => {
                self.history_all_projects = !self.history_all_projects;
                self.reload_history_sessions();
            }
            Action::SelectSession(idx) => {
                if idx < self.sessions.len() {
                    self.selection = None;
                    self.selected = idx;
                    self.sidebar_section = SidebarSection::Active;
                }
            }
            Action::Attach => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.tracker.on_attach();
                    self.mode = Mode::Attached;
                }
            }
            Action::Detach => {
                self.mode = Mode::Control;
                self.just_detached = true;
            }
            Action::SendLiteralDetachKey => {
                self.just_detached = false;
                self.snap_selected_to_live();
                // Only re-attach if the literal Ctrl+Q actually made it to
                // the pty -- a failed write already dropped us to Control
                // via forward_bytes (spec: write failure -> error + Exited
                // + Control), and re-attaching here would override that.
                if self.forward_bytes(&[0x11])
                    && let Some(s) = self.sessions.get_mut(self.selected)
                {
                    s.tracker.on_attach();
                    self.mode = Mode::Attached;
                }
            }
            Action::ForwardBytes(bytes) => {
                self.snap_selected_to_live();
                self.forward_bytes(&bytes);
            }
            Action::OpenNewSession => {
                let (default, available) = match &self.tracing {
                    Some(rt) => (rt.default_backend(), rt.langfuse_configured()),
                    None => (crate::config::Backend::Local, false),
                };
                self.mode = Mode::NewSession(
                    DialogState::new(&self.profiles)
                        .with_backend_options(default, available, &self.profiles)
                        .with_experiments(self.tracing.is_some()),
                );
            }
            Action::OpenHelp => self.mode = Mode::Help,
            Action::EnterConfirmKill => self.mode = Mode::ConfirmKill,
            Action::KillSelected => {
                if let Some(s) = self.sessions.get_mut(self.selected) {
                    s.kill();
                }
                self.mode = Mode::Control;
                let _ = self.save_active_sessions();
            }
            Action::RemoveSelected => {
                self.selection = None;
                if self.selected < self.sessions.len() {
                    self.sessions.remove(self.selected);
                    if self.selected >= self.sessions.len() {
                        self.selected = self.sessions.len().saturating_sub(1);
                    }
                    if self.sessions.is_empty() && !self.history_sessions.is_empty() {
                        self.sidebar_section = SidebarSection::History;
                    }
                    let _ = self.save_active_sessions();
                }
            }
            Action::RemoveExited => {
                let before = self.sessions.len();
                let now = Instant::now();
                self.sessions
                    .retain(|session| !matches!(session.status(now), Status::Exited(_)));
                if self.sessions.len() != before {
                    self.selection = None;
                    self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
                    if self.sessions.is_empty() && !self.history_sessions.is_empty() {
                        self.sidebar_section = SidebarSection::History;
                    }
                    let _ = self.save_active_sessions();
                }
            }
            Action::OpenSessionHistory => {
                let cur_dir = self
                    .sessions
                    .get(self.selected)
                    .map(|s| s.dir.clone())
                    .or_else(|| std::env::current_dir().ok());
                self.mode = Mode::SessionHistory(HistoryState::new(cur_dir.as_deref()));
            }
            Action::HistoryKey => self.handle_history_key(key),
            Action::OpenTraceBrowser => {
                let cur_dir = self
                    .sessions
                    .get(self.selected)
                    .map(|s| s.dir.clone())
                    .or_else(|| std::env::current_dir().ok());
                let langfuse = self.tracing.as_ref().and_then(|rt| rt.langfuse().cloned());
                self.mode = Mode::TraceBrowser(Box::new(
                    TraceBrowserState::new(self.trace_db_path.as_deref(), cur_dir.as_deref())
                        .with_langfuse(langfuse),
                ));
            }
            Action::BrowserKey => self.handle_browser_key(key),
            Action::RespawnSelected => {
                self.selection = None;
                self.respawn_selected();
            }
            Action::ToggleTracing => self.toggle_selected_tracing(),
            Action::CancelToControl => self.mode = Mode::Control,
            Action::DialogKey => self.handle_dialog_key(key),
        }
    }

    /// Writes `bytes` to the selected session. Returns `true` if the write
    /// succeeded (or there was no selected session to write to -- a no-op
    /// counts as success for callers deciding whether to proceed), `false`
    /// if the write failed. On failure this already applies the full spec
    /// consequence (status-bar error, session marked Exited, drop to
    /// Control) -- callers must not re-attach or otherwise treat the
    /// session as still live when this returns `false`.
    fn forward_bytes(&mut self, bytes: &[u8]) -> bool {
        if let Some(s) = self.sessions.get_mut(self.selected)
            && let Err(e) = s.write_bytes(bytes)
        {
            // spec: write failure -> status-bar error, session Exited
            self.notice = Some(Notice::error(format!(
                "write to '{}' failed: {e}",
                s.profile.name
            )));
            s.tracker.on_exit(None);
            // this path marks Exited with no PtyExit event ever arriving —
            // the tracing pipeline must still learn about it
            if let Some(trace) = &s.trace {
                trace.mark_exited(None);
            }
            self.mode = Mode::Control;
            return false;
        }
        true
    }

    /// Spec: any keystroke forwarded while scrolled first snaps the view
    /// back to the live bottom, like every terminal emulator.
    fn snap_selected_to_live(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.selected)
            && s.scrolled() > 0
        {
            s.scroll_to_bottom();
        }
    }

    fn handle_dialog_key(&mut self, key: &KeyEvent) {
        let Mode::NewSession(dialog) = &mut self.mode else {
            return;
        };
        match dialog.handle_key(key, &self.profiles) {
            DialogResult::Consumed => {}
            DialogResult::Cancel => self.mode = Mode::Control,
            DialogResult::Submit => {
                let mut profile = match self.profiles.get(dialog.profile_idx) {
                    Some(p) => p.clone(),
                    None => return,
                };
                let mut p_tracing = profile.tracing.unwrap_or_default();
                if dialog.guard_available() {
                    match dialog.budget() {
                        Ok((cost, turns)) => {
                            p_tracing.max_cost_usd = cost;
                            p_tracing.max_turns = turns;
                        }
                        Err(e) => {
                            let Mode::NewSession(dialog) = &mut self.mode else {
                                return;
                            };
                            dialog.error = Some(e);
                            return;
                        }
                    }
                }
                p_tracing.enabled = Some(dialog.tracing_enabled);
                p_tracing.content_mode = Some(dialog.content_mode.to_string());
                p_tracing.backend = Some(dialog.backend.as_str().to_string());
                profile.tracing = Some(p_tracing);
                // options become real arguments before planning, so the
                // trace planner sees --continue / -p / --resume and can
                // decide about session-id injection on the same command
                // line the CLI will get
                if let Some(harness) = dialog.harness {
                    let options = dialog.launch_options();
                    if !options.is_empty() {
                        profile.args =
                            crate::harness::compose(&profile.args, &options.render(harness));
                    }
                }
                let dir = resolve_working_dir(&dialog.dir);
                let link = dialog.experiment_link();
                let id = self.next_id;
                match self.spawn_traced(id, profile, dir) {
                    Ok(session) => {
                        self.next_id += 1;
                        self.sessions.push(session);
                        self.selected = self.sessions.len() - 1;
                        self.sidebar_section = SidebarSection::Active;
                        self.mode = Mode::Control;
                        let _ = self.save_active_sessions();
                        if let Some(link) = link {
                            self.experiment_links.insert(id, link);
                        }
                    }
                    Err(e) => {
                        let Mode::NewSession(dialog) = &mut self.mode else {
                            return;
                        };
                        dialog.error = Some(e.to_string());
                    }
                }
            }
        }
    }

    fn handle_browser_key(&mut self, key: &KeyEvent) {
        // `s` scores the selected turn; the outcome is a status-bar notice
        if key.code == KeyCode::Char('s')
            && let Mode::TraceBrowser(browser) = &mut self.mode
            && browser.search_input.is_none()
        {
            let outcome = browser.cycle_score();
            self.notice = Some(match outcome {
                Ok(message) => Notice::info(message),
                Err(e) => Notice::error(format!("score: {e}")),
            });
            return;
        }
        let Mode::TraceBrowser(browser) = &mut self.mode else {
            return;
        };
        // the search prompt captures every key until Enter/Esc
        if let Some(input) = browser.search_input.as_mut() {
            match key.code {
                KeyCode::Esc => browser.search_input = None,
                KeyCode::Enter => {
                    let query = input.trim().to_string();
                    browser.search_input = None;
                    if query.is_empty() {
                        browser.reload_sessions();
                    } else {
                        browser.run_search(&query);
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
                _ => {}
            }
            return;
        }
        let page = browser.viewport_rows.get().max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if browser.expanded {
                    browser.toggle_expanded();
                } else if browser.search_query.is_some() {
                    browser.reload_sessions();
                    browser.focused = BrowserPane::Sessions;
                } else {
                    self.mode = Mode::Control;
                }
            }
            KeyCode::Tab | KeyCode::Right => {
                browser.focused = match browser.focused {
                    BrowserPane::Sessions => BrowserPane::Turns,
                    BrowserPane::Turns => BrowserPane::Detail,
                    BrowserPane::Detail => BrowserPane::Sessions,
                };
            }
            KeyCode::BackTab | KeyCode::Left => {
                browser.focused = match browser.focused {
                    BrowserPane::Sessions => BrowserPane::Detail,
                    BrowserPane::Turns => BrowserPane::Sessions,
                    BrowserPane::Detail => BrowserPane::Turns,
                };
            }
            KeyCode::Char('/') => browser.search_input = Some(String::new()),
            KeyCode::Char('v') | KeyCode::Char('V') => browser.cycle_detail_view(),
            KeyCode::Char(' ') => browser.toggle_collapsed(),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                browser.all_projects = !browser.all_projects;
                browser.reload_sessions();
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if let Some(session) = browser.sessions.get(browser.selected_session).cloned() {
                    self.resume_traced_session(&session);
                }
            }
            KeyCode::Enter => match browser.focused {
                BrowserPane::Sessions => browser.focused = BrowserPane::Turns,
                BrowserPane::Turns => browser.focused = BrowserPane::Detail,
                BrowserPane::Detail => browser.toggle_expanded(),
            },
            KeyCode::Down | KeyCode::Char('j') => match browser.focused {
                BrowserPane::Sessions => {
                    if !browser.sessions.is_empty() {
                        let next = (browser.selected_session + 1).min(browser.sessions.len() - 1);
                        if next != browser.selected_session {
                            browser.selected_session = next;
                            browser.search_query = None;
                            browser.load_turns();
                        }
                    }
                }
                BrowserPane::Turns => {
                    if !browser.turns.is_empty() {
                        let next = (browser.selected_turn + 1).min(browser.turns.len() - 1);
                        if next != browser.selected_turn {
                            browser.selected_turn = next;
                            browser.load_observations();
                        }
                    }
                }
                BrowserPane::Detail => {
                    if browser.expanded {
                        browser.scroll_offset = browser
                            .scroll_offset
                            .saturating_add(1)
                            .min(browser.max_scroll());
                    } else {
                        browser.step_observation(1);
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match browser.focused {
                BrowserPane::Sessions => {
                    if browser.selected_session > 0 {
                        browser.selected_session -= 1;
                        browser.search_query = None;
                        browser.load_turns();
                    }
                }
                BrowserPane::Turns => {
                    if browser.selected_turn > 0 {
                        browser.selected_turn -= 1;
                        browser.load_observations();
                    }
                }
                BrowserPane::Detail => {
                    if browser.expanded {
                        browser.scroll_offset = browser.scroll_offset.saturating_sub(1);
                    } else {
                        browser.step_observation(-1);
                    }
                }
            },
            KeyCode::PageDown => {
                browser.scroll_offset = browser
                    .scroll_offset
                    .saturating_add(page)
                    .min(browser.max_scroll());
            }
            KeyCode::PageUp => browser.scroll_offset = browser.scroll_offset.saturating_sub(page),
            KeyCode::Home => browser.scroll_offset = 0,
            KeyCode::End => browser.scroll_offset = browser.max_scroll(),
            _ => {}
        }
    }

    /// Resumes a session picked in the trace browser through the same path
    /// the history viewer uses.
    fn resume_traced_session(&mut self, session: &crate::tracing::store::query::SessionStat) {
        let harness = match session.provider.as_str() {
            "claude" => crate::harness::Harness::Claude,
            "codex" => crate::harness::Harness::Codex,
            "antigravity" => crate::harness::Harness::Antigravity,
            other => {
                self.notice = Some(Notice::warn(format!(
                    "resume is not supported for {other} sessions"
                )));
                return;
            }
        };
        self.resume_conversation(
            harness,
            &session.session_id,
            session.cwd.as_ref().map(std::path::PathBuf::from),
        );
    }

    fn handle_history_key(&mut self, key: &KeyEvent) {
        let Mode::SessionHistory(history) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Control;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                history.focused_pane = match history.focused_pane {
                    HistoryPane::SessionsList => HistoryPane::LogDetail,
                    HistoryPane::LogDetail => HistoryPane::SessionsList,
                };
            }
            KeyCode::Left => {
                history.focused_pane = HistoryPane::SessionsList;
            }
            KeyCode::Right => {
                history.focused_pane = HistoryPane::LogDetail;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                history.all_projects = !history.all_projects;
                history.reload_sessions();
            }
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                if let Some(summary) = history.sessions.get(history.selected_session_idx).cloned() {
                    self.resume_history_session(&summary);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => match history.focused_pane {
                HistoryPane::SessionsList => {
                    if !history.sessions.is_empty() {
                        let next =
                            (history.selected_session_idx + 1).min(history.sessions.len() - 1);
                        if next != history.selected_session_idx {
                            history.selected_session_idx = next;
                            history.load_selected_log();
                        }
                    }
                }
                HistoryPane::LogDetail => {
                    history.scroll_offset = history
                        .scroll_offset
                        .saturating_add(1)
                        .min(history.max_scroll());
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match history.focused_pane {
                HistoryPane::SessionsList => {
                    if history.selected_session_idx > 0 {
                        history.selected_session_idx -= 1;
                        history.load_selected_log();
                    }
                }
                HistoryPane::LogDetail => {
                    history.scroll_offset = history.scroll_offset.saturating_sub(1);
                }
            },
            KeyCode::PageDown => {
                history.scroll_offset = history
                    .scroll_offset
                    .saturating_add(15)
                    .min(history.max_scroll());
            }
            KeyCode::PageUp => {
                history.scroll_offset = history.scroll_offset.saturating_sub(15);
            }
            KeyCode::Home => {
                history.scroll_offset = 0;
            }
            KeyCode::End => {
                history.scroll_offset = history.max_scroll();
            }
            _ => {}
        }
    }

    fn handle_skill_launcher_key(&mut self, key: &KeyEvent) {
        let Mode::SkillLauncher(ref mut state) = self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Control;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.move_up();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.move_down();
            }
            KeyCode::Char('1') | KeyCode::Char('c') | KeyCode::Char('C') => {
                if !state.harnesses.is_empty() {
                    state.selected = 0;
                }
            }
            KeyCode::Char('2') | KeyCode::Char('x') | KeyCode::Char('X') => {
                if state.harnesses.len() > 1 {
                    state.selected = 1;
                }
            }
            KeyCode::Char('3') | KeyCode::Char('a') | KeyCode::Char('A') => {
                if state.harnesses.len() > 2 {
                    state.selected = 2;
                }
            }
            KeyCode::Enter => {
                let skill_id = state.skill_id.clone();
                let harness = state.selected_harness();
                let launch_res = self.launch_skill(&skill_id, harness);
                match launch_res {
                    Ok(_) => {
                        self.sidebar_section = SidebarSection::Active;
                        self.mode = Mode::Attached;
                    }
                    Err(e) => {
                        self.mode = Mode::Control;
                        self.notice = Some(Notice::error(format!("Skill launch failed: {e}")));
                    }
                }
            }
            _ => {}
        }
    }

    /// `v`: what this binary is, where its files are, what this session
    /// runs. The facts are gathered once, here, never on the draw path.
    pub fn open_about(&mut self) {
        let facts = about::AboutFacts {
            config_path: self.config_path.as_deref(),
            trace_db: self.trace_db_path.as_deref(),
            runtime_dir: self
                .runtime_dir
                .clone()
                .unwrap_or_else(crate::tracing::analysis::default_snapshot_dir),
            run_id: &self.app_run_id,
            sessions: self.sessions.len(),
            traced_sessions: self.sessions.iter().filter(|s| s.trace.is_some()).count(),
            skills: self.skills.len(),
            loops: self.loop_registry.loops.len(),
            loops_paused: self
                .loop_registry
                .loops
                .iter()
                .filter(|l| l.paused())
                .count(),
            loops_kill_switch: self.loop_registry.pause_all,
        };
        self.mode = Mode::About(Box::new(about::AboutState {
            rows: about::rows(&facts),
            scroll_offset: 0,
            viewport_rows: std::cell::Cell::new(24),
        }));
    }

    /// `S`: opens the Skills view over the current screen.
    /// The configuration library root.
    pub fn library_root(&self) -> std::path::PathBuf {
        self.library_root
            .clone()
            .unwrap_or_else(crate::assets::root)
    }

    fn open_config_view(&mut self) {
        let root = self.library_root();
        self.mode = Mode::ConfigView(Box::new(ConfigViewState::new(
            &root,
            self.config_path.as_deref(),
            &self.loop_registry,
        )));
    }

    /// Records an editor request for `path`; the main loop runs it and
    /// calls `editor_finished`.
    fn request_editor(&mut self, path: std::path::PathBuf, asset_id: String) {
        let command = crate::assets::editor_command(self.editor.as_deref());
        self.editor_request = Some(EditorRequest {
            path,
            asset_id,
            command,
        });
    }

    pub fn take_editor_request(&mut self) -> Option<EditorRequest> {
        self.editor_request.take()
    }

    /// After the editor returns: rescans the library, re-validates the
    /// item, reloads whatever reads it, and reports.
    pub fn editor_finished(&mut self, request: EditorRequest, result: Result<(), String>) {
        if let Err(e) = result {
            self.notice = Some(Notice::error(format!("editor: {e}")));
        }
        if request.asset_id == "dialog:text" {
            self.dialog_editor_finished(&request.path);
            return;
        }
        if let Some(plan_id) = request.asset_id.strip_prefix("plan:") {
            self.reload_planned_after_edit(plan_id);
            if self.notice.is_none() {
                self.notice = Some(Notice::info("planned document updated"));
            }
            return;
        }
        if request.asset_id.starts_with("workflows/") {
            self.reload_workflow_list();
        }
        let skill_validation = request
            .asset_id
            .strip_prefix("skills/")
            .and_then(|rest| rest.strip_suffix("/SKILL.md"))
            .map(|id| {
                let result = request
                    .path
                    .parent()
                    .ok_or_else(|| format!("{} has no package directory", request.path.display()))
                    .and_then(|dir| {
                        crate::skill::load_skill_dir(dir)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
                (id.to_string(), result)
            });
        let root = self.library_root();
        let catalog = crate::assets::Catalog::load(&root, self.config_path.as_deref());
        let asset = catalog.get(&request.asset_id).cloned();
        let kind = asset.as_ref().map(|a| a.kind);
        self.reload_after_config_change(kind);
        if let Mode::ConfigView(view) = &mut self.mode {
            view.reload(&self.loop_registry);
            view.select_id(&request.asset_id, &self.loop_registry);
        }
        if let Mode::SkillsView(view) = &mut self.mode {
            let selected = view.selected_identity();
            if skill_validation
                .as_ref()
                .is_none_or(|(_, validation)| validation.is_ok())
            {
                view.reload();
                if let Some((id, harness)) = selected {
                    view.select_package(&id, harness);
                }
            }
        }
        if self.notice.is_some() {
            return;
        }
        if let Some((id, validation)) = skill_validation {
            self.notice = Some(match validation {
                Ok(()) => Notice::info(format!("saved skills/{id}/SKILL.md")),
                Err(error) => Notice::warn(error),
            });
            return;
        }
        self.notice = Some(match asset {
            Some(a) if a.valid() => {
                let extra = match a.kind {
                    crate::assets::Kind::LoopSkill | crate::assets::Kind::LoopAgent => {
                        let stale = catalog
                            .workspace_copies(&a, &self.loop_registry)
                            .iter()
                            .filter(|c| !c.same)
                            .count();
                        if stale > 0 {
                            format!("; {stale} workspace copy(ies) differ, u pushes them")
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                };
                Notice::info(format!("saved {}{extra}", a.id))
            }
            Some(a) => Notice::warn(format!(
                "{}: {}",
                a.id,
                a.problems.first().cloned().unwrap_or_default()
            )),
            None => Notice::info(format!("{} was removed", request.asset_id)),
        });
    }

    /// Reloads the state that reads a kind of configuration item.
    fn reload_after_config_change(&mut self, kind: Option<crate::assets::Kind>) {
        use crate::assets::Kind;
        match kind {
            Some(Kind::Skill) => self.reload_skills(),
            Some(Kind::LoopPattern)
            | Some(Kind::LoopSkill)
            | Some(Kind::LoopAgent)
            | Some(Kind::LoopTemplate) => {
                crate::loops::patterns::reload();
                self.loop_audits.clear();
            }
            Some(Kind::Settings) => self.reload_settings(),
            Some(Kind::Workflow) | Some(Kind::Prompts) | None => {}
        }
    }

    /// Re-reads `profiles.toml`: profiles, agents, loops settings and the
    /// editor apply immediately; the tracing runtime keeps its startup
    /// configuration.
    fn reload_settings(&mut self) {
        let path = match &self.config_path {
            Some(p) => p.clone(),
            None => self.library_root().join("profiles.toml"),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(Notice::warn(format!("{}: {e}", path.display())));
                return;
            }
        };
        match crate::config::parse(&text) {
            Ok(cfg) => {
                if !cfg.profiles.is_empty() {
                    self.profiles = cfg.profiles;
                }
                self.agents = crate::config::resolve_agents(cfg.agents.as_ref());
                self.loops = crate::config::resolve_loops(cfg.loops.as_ref());
                self.editor = cfg.editor;
                self.config_path = Some(path);
            }
            Err(e) => {
                self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            }
        }
    }

    fn handle_config_key(&mut self, key: &KeyEvent) {
        use crate::assets::Kind;
        let Mode::ConfigView(view) = &mut self.mode else {
            return;
        };
        let page = view.viewport_rows.get().max(1) as isize;
        // a footer question first
        match &mut view.pending {
            Pending::Reset => {
                let yes = matches!(
                    key.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                );
                let no = matches!(
                    key.code,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                );
                if !yes && !no {
                    return;
                }
                view.pending = Pending::None;
                if !yes {
                    return;
                }
                let Some(asset) = view.selected_asset().cloned() else {
                    return;
                };
                let notice = match view.catalog.reset(&asset) {
                    Ok(()) => Notice::info(format!("{} reset", asset.id)),
                    Err(e) => Notice::warn(e),
                };
                self.notice = Some(notice);
                self.reload_after_config_change(Some(asset.kind));
                if let Mode::ConfigView(view) = &mut self.mode {
                    view.reload(&self.loop_registry);
                }
                return;
            }
            Pending::Push => {
                let yes = matches!(
                    key.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                );
                let no = matches!(
                    key.code,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                );
                if !yes && !no {
                    return;
                }
                view.pending = Pending::None;
                if !yes {
                    return;
                }
                let report = view.catalog.push(&self.loop_registry, false);
                view.reload(&self.loop_registry);
                self.notice = Some(if report.errors.is_empty() {
                    Notice::info(format!(
                        "pushed {} file(s), {} unchanged{}",
                        report.written.len(),
                        report.unchanged.len(),
                        if report.skipped_loops.is_empty() {
                            String::new()
                        } else {
                            format!(", skipped {}", report.skipped_loops.join(", "))
                        }
                    ))
                } else {
                    Notice::error(report.errors.join("; "))
                });
                return;
            }
            Pending::NewName { kind, input } => {
                match key.code {
                    KeyCode::Esc => view.pending = Pending::None,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.push(c);
                    }
                    KeyCode::Enter => {
                        let (kind, name) = (*kind, input.clone());
                        view.pending = Pending::None;
                        match view.catalog.new_item(kind, &name) {
                            Ok(path) => {
                                let id = path
                                    .strip_prefix(&view.root)
                                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                                    .unwrap_or_else(|_| path.display().to_string());
                                view.reload(&self.loop_registry);
                                view.select_id(&id, &self.loop_registry);
                                self.request_editor(path, id);
                            }
                            Err(e) => self.notice = Some(Notice::warn(e)),
                        }
                    }
                    _ => {}
                }
                return;
            }
            Pending::None => {}
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if view.focus == ConfigPane::Detail {
                    view.focus = ConfigPane::List;
                } else {
                    self.mode = Mode::Control;
                }
            }
            KeyCode::Right | KeyCode::Tab => view.focus = ConfigPane::Detail,
            KeyCode::Left | KeyCode::BackTab => view.focus = ConfigPane::List,
            KeyCode::Down | KeyCode::Char('j') => match view.focus {
                ConfigPane::List => view.step(1, &self.loop_registry),
                ConfigPane::Detail => view.scroll(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match view.focus {
                ConfigPane::List => view.step(-1, &self.loop_registry),
                ConfigPane::Detail => view.scroll(-1),
            },
            KeyCode::PageDown => match view.focus {
                ConfigPane::List => view.step(page, &self.loop_registry),
                ConfigPane::Detail => view.scroll(page),
            },
            KeyCode::PageUp => match view.focus {
                ConfigPane::List => view.step(-page, &self.loop_registry),
                ConfigPane::Detail => view.scroll(-page),
            },
            KeyCode::Home => view.scroll_offset = 0,
            KeyCode::End => view.scroll_offset = view.max_scroll(),
            KeyCode::Enter | KeyCode::Char('e') => {
                let Some(asset) = view.selected_asset().cloned() else {
                    return;
                };
                match view.catalog.create_override(&asset) {
                    Ok(path) => self.request_editor(path, asset.id),
                    Err(e) => self.notice = Some(Notice::warn(e)),
                }
            }
            KeyCode::Char('n') => {
                let kind = view.selected_kind();
                match kind {
                    Some(k) if k.creatable() => {
                        view.pending = Pending::NewName {
                            kind: k,
                            input: String::new(),
                        };
                    }
                    Some(Kind::LoopPattern) => {
                        self.notice = Some(Notice::info(
                            "add a [[patterns]] table to loops/registry.toml (Enter edits it)",
                        ));
                    }
                    Some(k) => {
                        self.notice = Some(Notice::info(format!(
                            "{} items are edited in place; n creates skills, loop skills and loop agents",
                            k.label()
                        )));
                    }
                    None => {}
                }
            }
            KeyCode::Char('R') => match view.selected_asset() {
                Some(a) if a.kind == Kind::Settings => {
                    self.notice = Some(Notice::info(
                        "profiles.toml is never deleted; edit it instead",
                    ));
                }
                Some(a) if a.source == crate::assets::Source::Builtin => {
                    self.notice = Some(Notice::info(format!(
                        "{} already uses the built-in text",
                        a.id
                    )));
                }
                Some(_) => view.pending = Pending::Reset,
                None => {}
            },
            KeyCode::Char('u') => {
                if self.loop_registry.loops.is_empty() {
                    self.notice = Some(Notice::info("no registered loops to push into"));
                } else {
                    view.pending = Pending::Push;
                }
            }
            KeyCode::Char('r') => {
                view.reload(&self.loop_registry);
                self.notice = Some(Notice::info("configuration rescanned"));
            }
            _ => {}
        }
    }

    fn open_skills_view(&mut self) {
        self.reload_skills();
        // A test override of the install home is the whole home: harness
        // definitions are read from the same tree the installs land in, and
        // the project scan runs there too rather than in the real cwd.
        let cwd = match &self.skill_install_home {
            Some(h) => h.clone(),
            None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        };
        let install_home = self.skill_home();
        let home = match &self.skill_install_home {
            Some(h) => h.clone(),
            None => self
                .tracing
                .as_ref()
                .map(|rt| rt.home().to_path_buf())
                .unwrap_or_else(crate::skill::install::home_dir),
        };
        let mut view = SkillsViewState::new(
            self.trace_db_path.as_deref(),
            &cwd,
            &home,
            &install_home,
            self.skills_dir.as_deref(),
        );
        if let Some(bookmark) = &self.skill_workbench
            && view.select_package(&bookmark.id, bookmark.harness)
        {
            view.tab = bookmark.tab;
        }
        self.mode = Mode::SkillsView(Box::new(view));
    }

    fn handle_skills_key(&mut self, key: &KeyEvent) {
        self.remember_skill_workbench();
        match key.code {
            KeyCode::Char('e') => {
                self.edit_selected_skill();
                return;
            }
            KeyCode::Char('v') => {
                self.validate_selected_skill();
                return;
            }
            KeyCode::Char('l') => {
                self.launch_selected_workbench_skill();
                return;
            }
            _ => {}
        }
        let Mode::SkillsView(view) = &mut self.mode else {
            return;
        };
        let page = view.viewport_rows.get().max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if view.focus == SkillsPane::Detail {
                    view.focus = SkillsPane::Skills;
                } else if !view.clear_filter() {
                    self.mode = Mode::Control;
                }
            }
            KeyCode::Tab => view.next_tab(),
            KeyCode::BackTab => view.prev_tab(),
            KeyCode::Right => view.focus = SkillsPane::Detail,
            KeyCode::Left => view.focus = SkillsPane::Skills,
            KeyCode::Char('1') | KeyCode::Char('c') | KeyCode::Char('C') => {
                view.toggle_filter(crate::harness::Harness::Claude)
            }
            KeyCode::Char('2') | KeyCode::Char('x') | KeyCode::Char('X') => {
                view.toggle_filter(crate::harness::Harness::Codex)
            }
            KeyCode::Char('3') | KeyCode::Char('a') | KeyCode::Char('A') => {
                view.toggle_filter(crate::harness::Harness::Antigravity)
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                view.reload();
                self.reload_skills();
            }
            KeyCode::Char('T') => {
                let target = view.selected_execution().and_then(|e| {
                    e.session_key()
                        .map(|k| (k.to_string(), e.trace_id().map(str::to_string)))
                });
                let Some((session_key, trace_id)) = target else {
                    self.notice = Some(Notice::info(
                        "select an execution to open in the Trace Browser",
                    ));
                    return;
                };
                let cur_dir = std::env::current_dir().ok();
                let langfuse = self.tracing.as_ref().and_then(|rt| rt.langfuse().cloned());
                let mut browser =
                    TraceBrowserState::new(self.trace_db_path.as_deref(), cur_dir.as_deref())
                        .with_langfuse(langfuse);
                browser.focus_session(&session_key, trace_id.as_deref());
                self.mode = Mode::TraceBrowser(Box::new(browser));
            }
            KeyCode::Enter => match view.focus {
                SkillsPane::Skills => view.focus = SkillsPane::Detail,
                SkillsPane::Detail => {
                    // a live launch of the skill is attached to; everything
                    // else in this view is read-only
                    if view.tab == SkillsTab::Executions
                        && let Some(Execution::Launch(launch)) = view.selected_execution()
                    {
                        let launch_id = launch.id.clone();
                        let live = self.sessions.iter().position(|s| {
                            s.trace.as_ref().is_some_and(|t| t.launch_id == launch_id)
                                && !matches!(s.status(Instant::now()), Status::Exited(_))
                        });
                        match live {
                            Some(idx) => self.attach_to_session(idx),
                            None => {
                                self.notice = Some(Notice::info(
                                    "that launch is not running in this agent-mux; T opens its traces",
                                ));
                            }
                        }
                    }
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match (view.focus, view.tab) {
                (SkillsPane::Skills, _) => view.step(1),
                (SkillsPane::Detail, SkillsTab::Executions) => view.step_execution(1),
                (SkillsPane::Detail, _) => {
                    view.scroll_offset =
                        view.scroll_offset.saturating_add(1).min(view.max_scroll());
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match (view.focus, view.tab) {
                (SkillsPane::Skills, _) => view.step(-1),
                (SkillsPane::Detail, SkillsTab::Executions) => view.step_execution(-1),
                (SkillsPane::Detail, _) => {
                    view.scroll_offset = view.scroll_offset.saturating_sub(1);
                }
            },
            KeyCode::PageDown => {
                view.scroll_offset = view
                    .scroll_offset
                    .saturating_add(page)
                    .min(view.max_scroll());
            }
            KeyCode::PageUp => view.scroll_offset = view.scroll_offset.saturating_sub(page),
            KeyCode::Home => view.scroll_offset = 0,
            KeyCode::End => view.scroll_offset = view.max_scroll(),
            _ => {}
        }
        self.remember_skill_workbench();
    }

    fn remember_skill_workbench(&mut self) {
        let bookmark = match &self.mode {
            Mode::SkillsView(view) => {
                view.selected_identity()
                    .map(|(id, harness)| SkillWorkbenchBookmark {
                        id,
                        harness,
                        tab: view.tab,
                    })
            }
            _ => None,
        };
        if bookmark.is_some() {
            self.skill_workbench = bookmark;
        }
    }

    fn launch_selected_workbench_skill(&mut self) {
        let selected = match &self.mode {
            Mode::SkillsView(view) => view
                .selected_package()
                .map(|(skill, harness)| (skill.clone(), harness)),
            _ => None,
        };
        let Some((skill, harness)) = selected else {
            self.notice = Some(Notice::info(
                "native harness skills cannot be launched as agent-mux packages",
            ));
            return;
        };
        let package_dir = skill.dir.clone().or_else(|| {
            let override_dir = self.library_root().join("skills").join(&skill.id);
            override_dir
                .join("SKILL.md")
                .is_file()
                .then_some(override_dir)
        });
        if let Some(dir) = package_dir
            && let Err(error) = crate::skill::load_skill_dir(&dir)
        {
            self.notice = Some(Notice::warn(format!(
                "{} is invalid and was not launched: {error}",
                skill.id
            )));
            return;
        }
        self.skill_workbench = Some(SkillWorkbenchBookmark {
            id: skill.id.clone(),
            harness,
            tab: SkillsTab::Executions,
        });
        self.reload_skills();
        if let Err(error) = self.launch_skill(&skill.id, harness) {
            self.notice = Some(Notice::error(format!("Skill launch failed: {error}")));
        }
    }

    fn edit_selected_skill(&mut self) {
        let selected = match &self.mode {
            Mode::SkillsView(view) => view.selected_package().map(|(skill, _)| skill.clone()),
            _ => None,
        };
        let Some(skill) = selected else {
            self.notice = Some(Notice::info(
                "native harness skills are read-only here; edit their source path directly",
            ));
            return;
        };
        let asset_id = format!("skills/{}/SKILL.md", skill.id);
        let path = if let Some(dir) = skill.dir {
            dir.join("SKILL.md")
        } else {
            let catalog =
                crate::assets::Catalog::load(&self.library_root(), self.config_path.as_deref());
            let Some(asset) = catalog.get(&asset_id) else {
                self.notice = Some(Notice::error(format!(
                    "configuration entry for {} was not found",
                    skill.id
                )));
                return;
            };
            match catalog.create_override(asset) {
                Ok(path) => path,
                Err(error) => {
                    self.notice = Some(Notice::warn(error));
                    return;
                }
            }
        };
        self.request_editor(path, asset_id);
    }

    fn validate_selected_skill(&mut self) {
        let selected = match &self.mode {
            Mode::SkillsView(view) => view.selected_package().map(|(skill, _)| skill.clone()),
            _ => None,
        };
        let Some(skill) = selected else {
            self.notice = Some(Notice::info(
                "native harness skills are not validated as agent-mux packages",
            ));
            return;
        };
        let package_dir = skill.dir.or_else(|| {
            let override_dir = self.library_root().join("skills").join(&skill.id);
            override_dir
                .join("SKILL.md")
                .is_file()
                .then_some(override_dir)
        });
        self.notice = Some(match package_dir {
            Some(dir) => match crate::skill::load_skill_dir(&dir) {
                Ok(_) => Notice::info(format!("{} is valid", skill.id)),
                Err(error) => Notice::warn(error.to_string()),
            },
            None => Notice::info(format!("{} is valid (built-in)", skill.id)),
        });
    }

    /// Launches a harness session around `skill_id`, or attaches to the one
    /// already running it. The skill is (re)installed into the harness's
    /// skill directory right before the spawn, so the session always sees
    /// the current package.
    pub fn launch_skill(
        &mut self,
        skill_id: &str,
        harness: crate::harness::Harness,
    ) -> anyhow::Result<usize> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.id == skill_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Skill '{skill_id}' not found"))?;

        // 1. Singleton: attach to a live session for this skill.
        if let Some(idx) = self.running_skill_session(skill_id) {
            let running = self.running_skill_harness(skill_id);
            if running.is_some() && running != Some(harness) {
                self.notice = Some(Notice::warn(format!(
                    "{} is already running on {}; close that session to switch harness",
                    skill.name,
                    running.map(|h| h.as_str()).unwrap_or("another harness")
                )));
            }
            self.attach_to_session(idx);
            return Ok(idx);
        }

        // 2. Install the skill where this harness reads it.
        let home = self.skill_home();
        crate::skill::install::install(&skill, harness, &home, false)
            .map_err(|e| anyhow::anyhow!("cannot install skill for {}: {e}", harness.as_str()))?;

        // 3. Base profile for the harness, or a bare one.
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let base = self
            .profiles
            .iter()
            .find(|p| crate::harness::Harness::detect(&p.command) == Some(harness))
            .cloned()
            .unwrap_or_else(|| Profile {
                name: String::new(),
                command: harness.as_str().to_string(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            });
        let launch = crate::skill::launch::build_skill_launch_full(
            &skill,
            harness,
            &base,
            &cwd,
            self.trace_db_path.as_deref(),
            self.agents.hydrate && !skill.hydrate.is_empty(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

        let id = self.next_id;
        let mut session = self.spawn_traced_with_env(
            id,
            launch.profile,
            launch.cwd,
            &launch.env,
            Some(&skill.id),
        )?;
        session.skill_id = Some(skill.id.clone());
        self.next_id += 1;
        self.sessions.push(session);
        let idx = self.sessions.len() - 1;
        self.attach_to_session(idx);
        let _ = self.save_active_sessions();
        Ok(idx)
    }

    pub fn resume_history_session(&mut self, summary: &SessionSummary) {
        let harness = match summary.provider {
            crate::history::AgentProvider::Claude => crate::harness::Harness::Claude,
            crate::history::AgentProvider::Antigravity => crate::harness::Harness::Antigravity,
        };
        self.resume_conversation(harness, &summary.session_id, summary.cwd.clone());
    }

    /// Spawns a resume of one recorded conversation. The flags come from
    /// the harness mapping, so the history viewer and the trace browser
    /// cannot drift apart — and Codex, which spells resume as a
    /// subcommand rather than a flag, works through the same path.
    fn resume_conversation(
        &mut self,
        harness: crate::harness::Harness,
        session_id: &str,
        cwd: Option<std::path::PathBuf>,
    ) {
        let command = harness.as_str();
        let label = match harness {
            crate::harness::Harness::Claude => "claude",
            crate::harness::Harness::Codex => "codex",
            crate::harness::Harness::Antigravity => "antigravity",
        };
        let mut profile = self
            .profiles
            .iter()
            .find(|p| {
                crate::harness::Harness::detect(&p.command) == Some(harness)
                    || p.name.to_lowercase().contains(label)
            })
            .cloned()
            .unwrap_or_else(|| Profile {
                name: command.to_string(),
                command: command.to_string(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            });
        // a resume replaces the profile's arguments: resuming an id and
        // whatever the profile said about starting fresh cannot both hold
        profile.args = crate::harness::resume_args(harness, session_id);

        // The resumed session must run where it originally ran — the
        // transcript records it. Only if that is unknown (or gone) fall
        // back to the selected session's dir / the current dir.
        let dir = cwd
            .filter(|p| p.is_dir())
            .or_else(|| self.sessions.get(self.selected).map(|s| s.dir.clone()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let id = self.next_id;
        match self.spawn_traced(id, profile, dir) {
            Ok(session) => {
                self.next_id += 1;
                self.sessions.push(session);
                self.selected = self.sessions.len() - 1;
                self.sidebar_section = SidebarSection::Active;
                self.mode = Mode::Control;
                let _ = self.save_active_sessions();
            }
            Err(e) => {
                self.notice = Some(Notice::error(format!("Failed to resume session: {e}")));
                self.mode = Mode::Control;
            }
        }
    }

    fn respawn_selected(&mut self) {
        let Some(old) = self.sessions.get(self.selected) else {
            return;
        };
        let profile = old.profile.clone();
        let dir = old.dir.clone();
        let skill_id = old.skill_id.clone();
        // Fresh id: a stale reader thread from the dead session may still
        // send PtyExit for the old id; it must not find the new session.
        // Launch extras are re-planned fresh too (never stored in the
        // profile), so a respawned Claude tab gets a NEW session uuid.
        let id = self.next_id;
        match self.spawn_traced_with_env(id, profile, dir, &[], skill_id.as_deref()) {
            Ok(mut session) => {
                session.skill_id = skill_id;
                self.next_id += 1;
                self.sessions[self.selected] = session;
                let _ = self.save_active_sessions();
            }
            Err(e) => self.notice = Some(Notice::error(format!("respawn failed: {e}"))),
        }
    }

    pub fn toggle_selected_tracing(&mut self) {
        let Some(s) = self.sessions.get_mut(self.selected) else {
            return;
        };
        if let Some(trace) = s.trace.take() {
            trace.mark_stopped();
            s.trace_stats = None;
            self.notice = Some(Notice::info(format!(
                "tracing stopped for '{}'",
                s.profile.name
            )));
        } else {
            if matches!(s.status(Instant::now()), Status::Exited(_)) {
                self.notice = Some(Notice::warn("cannot trace an exited session"));
                return;
            }
            let Some(rt) = self.tracing.as_mut() else {
                self.notice = Some(Notice::warn(
                    "tracing is off (store unavailable or [tracing] enabled = false)",
                ));
                return;
            };
            if let Some(plan) = rt.plan_attach(&s.profile, &s.dir) {
                let id = s.id;
                s.trace = Some(rt.start_session(id, plan));
                self.notice = Some(Notice::info(format!(
                    "tracing started for '{}'",
                    s.profile.name
                )));
            } else {
                self.notice = Some(Notice::warn(format!(
                    "tracing is not supported for '{}'",
                    s.profile.name
                )));
            }
        }
    }

    pub fn handle_pty_output(&mut self, id: usize, bytes: &[u8], now: Instant) {
        let focused = self
            .attached()
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.id)
            == Some(id);
        if let Some(i) = self.session_index(id) {
            self.sessions[i].process_output(bytes, now, focused);
        }
        if !self.live_workflow_runs.is_empty() {
            self.capture_workflow_output(id, bytes);
        }
        if !self.live_workflow_plans.is_empty() {
            self.capture_plan_output(id, bytes);
        }
        if self.search.is_some() && self.sessions.get(self.selected).map(|s| s.id) == Some(id) {
            self.rerun_search();
        }
    }

    pub fn handle_pty_exit(&mut self, id: usize) {
        if let Some(i) = self.session_index(id) {
            self.sessions[i].mark_exited();
            if let Some(path) = self.sessions[i].briefing_path.take() {
                let _ = std::fs::remove_file(path);
            }
            // notify the tracing pipeline (idempotent against the
            // documented duplicate PtyExit) with the exit code now known
            if let Some(trace) = &self.sessions[i].trace {
                let code = match self.sessions[i].status(Instant::now()) {
                    Status::Exited(code) => code,
                    _ => None,
                };
                trace.mark_exited(code);
            }
            self.record_experiment_link(i);
            self.finish_loop_run_for_session(id);
            self.finish_workflow_session_for_session(id);
            self.finish_plan_for_session(id);
            // if we were attached to it, drop back to Control
            if self.attached() == Some(i) {
                self.mode = Mode::Control;
            }
            self.reload_history_sessions();
            let _ = self.save_active_sessions();
        }
    }

    pub fn set_pane_size(&mut self, rows: u16, cols: u16) {
        if self.pane_size == (rows, cols) {
            return;
        }
        self.pane_size = (rows, cols);
        let side_w = if self.sidebar_hidden {
            0
        } else {
            ui::SIDEBAR_WIDTH
        };
        self.terminal_size = (rows + 3, cols + side_w + 2);
        for s in &mut self.sessions {
            s.resize(rows, cols);
        }
    }

    pub fn kill_all(&mut self) {
        // Kill unconditionally, even for sessions whose status is already
        // Exited: forward_bytes marks a session Exited(None) on a write
        // failure without killing the child, so skipping "Exited" sessions
        // here could leave a still-live child running past app quit.
        // Session::kill() already swallows errors on a dead/already-exited
        // child, so calling it again here is harmless.
        for s in &mut self.sessions {
            s.kill();
            if let Some(path) = s.briefing_path.take() {
                let _ = std::fs::remove_file(path);
            }
        }
        for i in 0..self.sessions.len() {
            self.record_experiment_link(i);
        }
    }

    /// Writes the experiment run for a session that named one, once it is
    /// over (or is being killed with the app): the launch row is in the
    /// store by then, and the exit code is as known as it will get.
    fn record_experiment_link(&mut self, i: usize) {
        let Some(link) = self.experiment_links.remove(&self.sessions[i].id) else {
            return;
        };
        let (Some(rt), Some(trace)) = (&self.tracing, &self.sessions[i].trace) else {
            return;
        };
        let code = match self.sessions[i].status(Instant::now()) {
            Status::Exited(code) => code,
            _ => None,
        };
        if let Err(e) = crate::tracing::experiments::link_launch(
            rt.db_path(),
            &trace.launch_id,
            &link,
            &self.sessions[i].dir,
            code,
        ) {
            self.notice = Some(Notice::error(format!(
                "experiment {}: {e}",
                link.experiment
            )));
        }
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::status::Status;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_q() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
    }

    fn ctx(selected: Option<Status>) -> DispatchCtx {
        DispatchCtx {
            selected_status: selected,
            any_working: false,
            just_detached: false,
            app_cursor: false,
            sidebar_section: SidebarSection::Active,
            sidebar_hidden: false,
        }
    }

    #[test]
    fn control_navigation() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('j')), &c),
            Action::MoveDown
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Down), &c),
            Action::MoveDown
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('k')), &c),
            Action::MoveUp
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Up), &c),
            Action::MoveUp
        ));
    }

    #[test]
    fn control_attach_and_new() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Enter), &c),
            Action::Attach
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('n')), &c),
            Action::OpenNewSession
        ));
    }

    #[test]
    fn interactive_codex_uses_inline_scrollback_inside_the_mux() {
        let mut codex = crate::config::Config::default_profiles()
            .into_iter()
            .find(|p| p.command == "codex")
            .unwrap();
        prepare_nested_tui(&mut codex);
        assert_eq!(
            codex
                .args
                .iter()
                .filter(|a| *a == "--no-alt-screen")
                .count(),
            1
        );
        prepare_nested_tui(&mut codex);
        assert_eq!(
            codex
                .args
                .iter()
                .filter(|a| *a == "--no-alt-screen")
                .count(),
            1,
            "respawn preparation is idempotent"
        );

        codex.args = vec!["exec".into(), "say hello".into()];
        prepare_nested_tui(&mut codex);
        assert!(!codex.args.iter().any(|a| a == "--no-alt-screen"));
    }

    #[test]
    fn digits_select_sessions_and_question_mark_opens_help() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('1')), &c),
            Action::SelectSession(0)
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('9')), &c),
            Action::SelectSession(8)
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('?')), &c),
            Action::OpenHelp
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::F(1)), &c),
            Action::OpenHelp
        ));
        // any close key leaves Help
        for code in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Char('?'),
            KeyCode::Enter,
        ] {
            assert!(matches!(
                dispatch(&Mode::Help, &key(code), &c),
                Action::CancelToControl
            ));
        }
        assert!(matches!(
            dispatch(&Mode::Help, &key(KeyCode::Char('x')), &c),
            Action::None
        ));
    }

    #[test]
    fn control_toggle_tracing() {
        let c = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('t')), &c),
            Action::ToggleTracing
        ));
        // Shift+T is the trace browser, not a second toggle
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('T')), &c),
            Action::OpenTraceBrowser
        ));
    }

    #[test]
    fn enter_with_no_sessions_is_noop() {
        let c = ctx(None);
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Enter), &c),
            Action::None
        ));
    }

    #[test]
    fn x_confirms_kill_when_running_removes_when_exited() {
        let running = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('x')), &running),
            Action::EnterConfirmKill
        ));
        let exited = ctx(Some(Status::Exited(Some(0))));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('x')), &exited),
            Action::RemoveSelected
        ));
    }

    #[test]
    fn uppercase_x_clears_all_exited_sessions_from_active() {
        let running = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('X')), &running),
            Action::RemoveExited
        ));

        let mut hidden = ctx(Some(Status::Working));
        hidden.sidebar_hidden = true;
        hidden.sidebar_section = SidebarSection::Agents;
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('X')), &hidden),
            Action::RemoveExited
        ));

        for section in [
            SidebarSection::Agents,
            SidebarSection::Loops,
            SidebarSection::Workflows,
            SidebarSection::History,
        ] {
            let mut visible = ctx(Some(Status::Working));
            visible.sidebar_section = section;
            assert!(matches!(
                dispatch(&Mode::Control, &key(KeyCode::Char('X')), &visible),
                Action::None
            ));
        }
    }

    #[test]
    fn r_respawns_only_exited() {
        let exited = ctx(Some(Status::Exited(Some(1))));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('r')), &exited),
            Action::RespawnSelected
        ));
        let running = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('r')), &running),
            Action::None
        ));
    }

    #[test]
    fn q_quits_directly_unless_something_is_working() {
        let quiet = ctx(Some(Status::Idle));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('q')), &quiet),
            Action::Quit
        ));
        let mut busy = ctx(Some(Status::Idle));
        busy.any_working = true;
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('q')), &busy),
            Action::EnterConfirmQuit
        ));
    }

    #[test]
    fn attached_ctrl_q_detaches_everything_else_forwards() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::Attached, &ctrl_q(), &c),
            Action::Detach
        ));
        match dispatch(&Mode::Attached, &key(KeyCode::Char('a')), &c) {
            Action::ForwardBytes(b) => assert_eq!(b, b"a".to_vec()),
            other => panic!("expected ForwardBytes, got {other:?}"),
        }
        // unencodable keys are swallowed, not errors
        assert!(matches!(
            dispatch(&Mode::Attached, &key(KeyCode::CapsLock), &c),
            Action::None
        ));
    }

    #[test]
    fn double_ctrl_q_sends_literal() {
        let mut c = ctx(Some(Status::Working));
        c.just_detached = true;
        assert!(matches!(
            dispatch(&Mode::Control, &ctrl_q(), &c),
            Action::SendLiteralDetachKey
        ));
        // without the flag, Ctrl+Q in Control does nothing
        c.just_detached = false;
        assert!(matches!(
            dispatch(&Mode::Control, &ctrl_q(), &c),
            Action::None
        ));
    }
}

#[cfg(test)]
mod confirm_modes {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctx(selected: Option<Status>) -> DispatchCtx {
        DispatchCtx {
            selected_status: selected,
            any_working: false,
            just_detached: false,
            app_cursor: false,
            sidebar_section: SidebarSection::Active,
            sidebar_hidden: false,
        }
    }

    #[test]
    fn confirm_modes() {
        let c = ctx(Some(Status::Working));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('y')), &c),
            Action::KillSelected
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Enter), &c),
            Action::KillSelected
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Esc), &c),
            Action::CancelToControl
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmKill, &key(KeyCode::Char('n')), &c),
            Action::CancelToControl
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmQuit, &key(KeyCode::Char('y')), &c),
            Action::Quit
        ));
        assert!(matches!(
            dispatch(&Mode::ConfirmQuit, &key(KeyCode::Esc), &c),
            Action::CancelToControl
        ));
    }

    #[test]
    fn new_session_mode_routes_to_dialog() {
        let mode = Mode::NewSession(DialogState::new(&crate::config::Config::default_profiles()));
        let c = ctx(None);
        assert!(matches!(
            dispatch(&mode, &key(KeyCode::Char('a')), &c),
            Action::DialogKey
        ));
    }
}

#[cfg(test)]
mod dialog_tests {
    use super::*;
    use crate::config::Profile;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "A".into(),
                command: "a".into(),
                args: vec![],
                default_dir: Some("C:\\one".into()),
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            Profile {
                name: "B".into(),
                command: "b".into(),
                args: vec![],
                default_dir: Some("C:\\two".into()),
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
        ]
    }

    #[test]
    fn new_dialog_prefills_first_profiles_default_dir() {
        let d = DialogState::new(&profiles());
        assert_eq!(d.profile_idx, 0);
        assert_eq!(d.dir, "C:\\one");
        assert!(matches!(d.field, DialogField::Profile));
    }

    #[test]
    fn cycling_profile_updates_untouched_dir() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.profile_idx, 1);
        assert_eq!(d.dir, "C:\\two");
    }

    #[test]
    fn edited_dir_survives_profile_cycling() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Tab), &ps); // to Dir field
        d.handle_key(&key(KeyCode::Char('X')), &ps);
        d.handle_key(&key(KeyCode::Tab), &ps); // to Tracing
        d.handle_key(&key(KeyCode::Tab), &ps); // to ContentMode
        d.handle_key(&key(KeyCode::Tab), &ps); // back to Profile
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.dir, "C:\\oneX");
    }

    #[test]
    fn dialog_tab_navigation_cycles_all_fields() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        assert_eq!(d.field, DialogField::Profile);

        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Dir);

        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Tracing);

        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Backend);

        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::ContentMode);

        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Profile);

        // BackTab reverse
        d.handle_key(&key(KeyCode::BackTab), &ps);
        assert_eq!(d.field, DialogField::ContentMode);
    }

    #[test]
    fn dialog_toggles_tracing_and_content_mode() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        assert!(d.tracing_enabled);
        assert_eq!(d.content_mode, DialogContentMode::Full);

        // Focus Tracing field and toggle
        d.field = DialogField::Tracing;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert!(!d.tracing_enabled);

        d.handle_key(&key(KeyCode::Char('t')), &ps);
        assert!(d.tracing_enabled);

        // Focus ContentMode field and toggle
        d.field = DialogField::ContentMode;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert_eq!(d.content_mode, DialogContentMode::Metadata);

        d.handle_key(&key(KeyCode::Char('m')), &ps);
        assert_eq!(d.content_mode, DialogContentMode::Full);
    }

    /// Profiles that run a real CLI, so the option fields appear.
    fn harness_profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "Claude Code".into(),
                command: "claude".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: Some("claude-opus-5".into()),
                bypass_approvals: Some(true),
            },
            Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            Profile {
                name: "Shell".into(),
                command: "bash".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
        ]
    }

    #[test]
    fn launch_option_fields_appear_only_for_a_known_cli() {
        let ps = harness_profiles();
        let mut d = DialogState::new(&ps);
        assert_eq!(d.harness, Some(crate::harness::Harness::Claude));
        assert_eq!(
            d.fields(),
            vec![
                DialogField::Profile,
                DialogField::Dir,
                DialogField::Tracing,
                DialogField::Backend,
                DialogField::ContentMode,
                DialogField::Model,
                DialogField::Approvals,
                DialogField::Resume,
                DialogField::OneShot,
                // the budget guard: Claude's PreToolUse hook can refuse a call
                DialogField::MaxCost,
                DialogField::MaxTurns,
            ]
        );
        // the profile's own defaults pre-fill the fields
        assert_eq!(d.model, "claude-opus-5");
        assert!(d.bypass_approvals);

        // Tab reaches them and wraps back round
        d.field = DialogField::ContentMode;
        for expected in [
            DialogField::Model,
            DialogField::Approvals,
            DialogField::Resume,
            DialogField::OneShot,
            DialogField::MaxCost,
            DialogField::MaxTurns,
            DialogField::Profile,
        ] {
            d.handle_key(&key(KeyCode::Tab), &ps);
            assert_eq!(d.field, expected);
        }
        d.handle_key(&key(KeyCode::BackTab), &ps);
        assert_eq!(d.field, DialogField::MaxTurns);
        // the guard fields go with tracing: off, and they are gone
        d.tracing_enabled = false;
        assert!(!d.fields().contains(&DialogField::MaxCost));
        d.tracing_enabled = true;
        // typed limits parse, or say why not
        d.max_cost = "2.5".into();
        d.max_turns = "40".into();
        assert_eq!(d.budget(), Ok((Some(2.5), Some(40))));
        d.max_turns = "lots".into();
        assert!(d.budget().unwrap_err().contains("max turns"));
        d.max_turns.clear();
        d.max_cost = "-1".into();
        assert!(d.budget().unwrap_err().contains("max cost"));
        d.max_cost = "  ".into();
        assert_eq!(d.budget(), Ok((None, None)));

        // switching profile re-reads its defaults
        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Profile);
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.harness, Some(crate::harness::Harness::Codex));
        assert_eq!(d.model, "", "codex profile sets no default model");
        assert!(!d.bypass_approvals);

        // a profile that is not a known CLI hides the four fields
        d.handle_key(&key(KeyCode::Down), &ps);
        assert_eq!(d.harness, None);
        assert_eq!(d.fields().len(), 5);
        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Dir, "tab skips what is not shown");
    }

    #[test]
    fn experiment_fields_appear_only_with_a_store_and_link_the_launch() {
        let ps = harness_profiles();
        // no store: the fields are not offered and Tab never lands on them
        let mut d = DialogState::new(&ps);
        assert!(!d.fields().contains(&DialogField::Experiment));
        d.experiment = "x".into();
        assert_eq!(d.experiment_link(), None, "no store, no link");

        let mut d = DialogState::new(&ps).with_experiments(true);
        let fields = d.fields();
        assert_eq!(
            &fields[fields.len() - 2..],
            &[DialogField::Experiment, DialogField::Variant]
        );
        assert_eq!(d.experiment_link(), None, "blank experiment = not linked");
        d.field = DialogField::Experiment;
        for c in "touch file".chars() {
            d.handle_key(&key(KeyCode::Char(c)), &ps);
        }
        d.handle_key(&key(KeyCode::Backspace), &ps);
        assert_eq!(d.experiment, "touch fil");
        let link = d.experiment_link().unwrap();
        assert_eq!(link.experiment, "touch fil");
        assert_eq!(link.variant, "interactive", "blank variant gets a label");
        assert_eq!(link.prompt, "", "no one-shot: a conversation");
        d.field = DialogField::Variant;
        for c in "b".chars() {
            d.handle_key(&key(KeyCode::Char(c)), &ps);
        }
        d.one_shot = " fix it ".into();
        let link = d.experiment_link().unwrap();
        assert_eq!(
            (link.variant.as_str(), link.prompt.as_str()),
            ("b", "fix it")
        );
        // tracing off for this launch: nothing to record into
        d.tracing_enabled = false;
        assert!(!d.fields().contains(&DialogField::Variant));
        assert_eq!(d.experiment_link(), None);
    }

    #[test]
    fn launch_options_leave_blank_fields_out_entirely() {
        use crate::harness::{Harness, Resume};
        let ps = harness_profiles();
        let mut d = DialogState::new(&ps);
        d.model.clear();
        d.bypass_approvals = false;
        // nothing chosen: nothing is passed
        let o = d.launch_options();
        assert!(o.is_empty());
        assert!(o.render(Harness::Claude).trailing.is_empty());

        // typing fills the two text fields
        d.field = DialogField::Model;
        for c in "gpt-5.6".chars() {
            d.handle_key(&key(KeyCode::Char(c)), &ps);
        }
        d.handle_key(&key(KeyCode::Backspace), &ps);
        assert_eq!(d.model, "gpt-5.");
        d.field = DialogField::OneShot;
        for c in "fix it".chars() {
            d.handle_key(&key(KeyCode::Char(c)), &ps);
        }
        assert_eq!(d.one_shot, "fix it", "space is text here, not a toggle");

        // and the toggles toggle
        d.field = DialogField::Approvals;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert!(d.bypass_approvals);
        d.field = DialogField::Resume;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert!(d.resume_last);

        let o = d.launch_options();
        assert_eq!(o.model.as_deref(), Some("gpt-5."));
        assert_eq!(o.one_shot.as_deref(), Some("fix it"));
        assert_eq!(o.resume, Resume::Last);
        assert!(o.bypass_approvals);
        // whitespace alone still counts as unset
        d.model = "   ".into();
        assert_eq!(d.launch_options().model, None);
    }

    #[test]
    fn dialog_backend_cycles_only_when_langfuse_is_available() {
        use crate::config::Backend;
        let ps = profiles();
        // no credentials: the field is inert and a profile asking for
        // Langfuse still launches local
        let mut d = DialogState::new(&ps).with_backend_options(Backend::Both, false, &ps);
        assert_eq!(d.backend, Backend::Local);
        assert!(!d.langfuse_available);
        d.field = DialogField::Backend;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert_eq!(d.backend, Backend::Local);
        // with credentials: Space/b cycle forward, Left cycles back
        let mut d = DialogState::new(&ps).with_backend_options(Backend::Local, true, &ps);
        d.field = DialogField::Backend;
        d.handle_key(&key(KeyCode::Char(' ')), &ps);
        assert_eq!(d.backend, Backend::Langfuse);
        d.handle_key(&key(KeyCode::Char('b')), &ps);
        assert_eq!(d.backend, Backend::Both);
        d.handle_key(&key(KeyCode::Right), &ps);
        assert_eq!(d.backend, Backend::Local);
        d.handle_key(&key(KeyCode::Left), &ps);
        assert_eq!(d.backend, Backend::Both);
        // the profile's own default applies when it has one
        let mut with_override = ps.clone();
        with_override[0].tracing = Some(crate::config::ProfileTracing {
            backend: Some("langfuse".into()),
            ..Default::default()
        });
        let d = DialogState::new(&with_override).with_backend_options(
            Backend::Local,
            true,
            &with_override,
        );
        assert_eq!(d.backend, Backend::Langfuse);
        // Tab reaches the field between Tracing and Content Mode
        let mut d = DialogState::new(&ps);
        d.field = DialogField::Tracing;
        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::Backend);
        d.handle_key(&key(KeyCode::Tab), &ps);
        assert_eq!(d.field, DialogField::ContentMode);
        d.handle_key(&key(KeyCode::BackTab), &ps);
        assert_eq!(d.field, DialogField::Backend);
    }

    #[test]
    fn typing_and_backspace_edit_dir() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        d.handle_key(&key(KeyCode::Tab), &ps);
        d.handle_key(&key(KeyCode::Char('Z')), &ps);
        d.handle_key(&key(KeyCode::Backspace), &ps);
        assert_eq!(d.dir, "C:\\one");
    }

    #[test]
    fn enter_submits_esc_cancels() {
        let ps = profiles();
        let mut d = DialogState::new(&ps);
        assert!(matches!(
            d.handle_key(&key(KeyCode::Enter), &ps),
            DialogResult::Submit
        ));
        assert!(matches!(
            d.handle_key(&key(KeyCode::Esc), &ps),
            DialogResult::Cancel
        ));
        assert!(matches!(
            d.handle_key(&key(KeyCode::Char('q')), &ps),
            DialogResult::Consumed
        ));
    }

    #[test]
    fn new_dialog_prefills_current_dir_when_no_default_dir() {
        let profiles = vec![Profile {
            name: "Default".into(),
            command: "claude".into(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }];
        let d = DialogState::new(&profiles);
        assert_eq!(d.profile_idx, 0);
        let expected = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into());
        assert_eq!(d.dir, expected);
    }

    #[test]
    fn resolve_working_dir_empty_and_tilde() {
        let cur = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        assert_eq!(resolve_working_dir(""), cur);
        assert_eq!(resolve_working_dir("   "), cur);
        assert_eq!(resolve_working_dir("."), cur);
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(resolve_working_dir("~"), std::path::PathBuf::from(&home));
            assert_eq!(
                resolve_working_dir("~/test"),
                std::path::PathBuf::from(&home).join("test")
            );
        }
    }

    #[test]
    fn directory_navigation_and_selection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        std::fs::create_dir(root.join("sub_a")).unwrap();
        std::fs::create_dir(root.join("sub_b")).unwrap();

        let profiles = vec![Profile {
            name: "Test".into(),
            command: "sh".into(),
            args: vec![],
            default_dir: Some(root.to_string_lossy().into_owned()),
            tracing: None,
            model: None,
            bypass_approvals: None,
        }];

        let mut d = DialogState::new(&profiles);
        d.handle_key(&key(KeyCode::Tab), &profiles); // switch to Dir field
        assert!(matches!(d.field, DialogField::Dir));
        assert!(d.dir_picker.rows().contains(&"sub_a".to_string()));
        assert!(d.dir_picker.rows().contains(&"sub_b".to_string()));

        // Down arrow selects first entry
        d.handle_key(&key(KeyCode::Down), &profiles);
        assert_eq!(d.dir_picker.selected, Some(0));

        // Up arrow goes back to text field
        d.handle_key(&key(KeyCode::Up), &profiles);
        assert_eq!(d.dir_picker.selected, None);

        // Find index of sub_a
        let sub_a_idx = d
            .dir_picker
            .rows()
            .iter()
            .position(|e| e == "sub_a")
            .unwrap();
        d.dir_picker.selected = Some(sub_a_idx);

        // Right arrow descends into sub_a
        d.handle_key(&key(KeyCode::Right), &profiles);
        assert_eq!(std::path::PathBuf::from(&d.dir), root.join("sub_a"));

        // Left arrow goes back up to root
        d.handle_key(&key(KeyCode::Left), &profiles);
        assert_eq!(std::path::PathBuf::from(&d.dir), root);
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctx() -> DispatchCtx {
        DispatchCtx {
            selected_status: None,
            any_working: false,
            just_detached: false,
            app_cursor: false,
            sidebar_section: SidebarSection::Active,
            sidebar_hidden: false,
        }
    }

    #[test]
    fn l_in_control_opens_history() {
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('l')), &ctx()),
            Action::OpenSessionHistory
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('L')), &ctx()),
            Action::OpenSessionHistory
        ));
    }

    /// A browser state with a known tree of observations and no store.
    fn browser_with_tree() -> TraceBrowserState {
        use crate::tracing::store::query::ObservationView;
        let obs = |id: &str, depth: usize| ObservationView {
            id: id.into(),
            trace_id: "t".into(),
            parent_id: None,
            depth,
            obs_type: "tool".into(),
            name: id.into(),
            kind: None,
            start_ns: 0,
            end_ns: Some(1_000_000),
            level: "DEFAULT".into(),
            status_message: None,
            model: None,
            model_id: None,
            input: None,
            output: None,
            thinking: None,
            usage: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            metadata: "{}".into(),
        };
        let mut browser = TraceBrowserState::new(None, None);
        // gen, agent[grep, read], bash
        browser.observations = vec![
            obs("gen", 0),
            obs("agent", 0),
            obs("grep", 1),
            obs("read", 1),
            obs("bash", 0),
        ];
        browser.observations[2].parent_id = Some("agent".into());
        browser.observations[3].parent_id = Some("agent".into());
        browser.error = None;
        browser
    }

    #[test]
    fn v_cycles_the_detail_view_and_space_folds_only_in_the_tree() {
        let mut b = browser_with_tree();
        assert_eq!(b.detail_view, DetailView::List);
        // space does nothing outside the tree
        b.selected_observation = 1;
        b.toggle_collapsed();
        assert!(b.collapsed.is_empty(), "list view does not fold");

        b.cycle_detail_view();
        assert_eq!(b.detail_view, DetailView::Tree);
        b.toggle_collapsed();
        assert_eq!(b.collapsed.len(), 1, "the agent row folds");
        assert_eq!(b.visible_rows(), vec![0, 1, 4], "its children are hidden");
        b.toggle_collapsed();
        assert!(b.collapsed.is_empty(), "space unfolds again");

        // a leaf cannot be folded
        b.selected_observation = 2;
        b.toggle_collapsed();
        assert!(b.collapsed.is_empty(), "a leaf has no subtree");

        b.cycle_detail_view();
        assert_eq!(b.detail_view, DetailView::Timeline);
        assert_eq!(b.visible_rows().len(), 5, "the timeline hides nothing");
        b.cycle_detail_view();
        assert_eq!(b.detail_view, DetailView::Loop);
        assert_eq!(b.visible_rows().len(), 5, "nor does the loop view");
        b.cycle_detail_view();
        assert_eq!(b.detail_view, DetailView::List);
    }

    #[test]
    fn selection_steps_over_folded_subtrees_and_never_hides() {
        let mut b = browser_with_tree();
        b.cycle_detail_view();
        b.selected_observation = 1;
        b.toggle_collapsed();
        // down from the folded agent lands on bash, not on its children
        b.step_observation(1);
        assert_eq!(b.observations[b.selected_observation].id, "bash");
        b.step_observation(-1);
        assert_eq!(b.observations[b.selected_observation].id, "agent");
        b.step_observation(-1);
        assert_eq!(b.observations[b.selected_observation].id, "gen");
        b.step_observation(-1);
        assert_eq!(b.observations[b.selected_observation].id, "gen", "clamped");

        // a selection inside a subtree folded from elsewhere is pulled
        // back to a visible row when the view is entered again
        b.collapsed.clear();
        b.selected_observation = 3; // "read", inside agent
        b.collapsed.insert("agent".to_string());
        b.detail_view = DetailView::List;
        b.cycle_detail_view(); // List → Tree, where the fold applies
        assert_eq!(b.detail_view, DetailView::Tree);
        assert!(
            b.visible_rows().contains(&b.selected_observation),
            "selection is never left on a hidden row"
        );
        assert_eq!(b.observations[b.selected_observation].id, "agent");
    }

    #[test]
    fn trace_browser_wheel_moves_the_focused_list() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        let mut browser = browser_with_tree();
        browser.focused = BrowserPane::Detail;
        app.mode = Mode::TraceBrowser(Box::new(browser));
        let wheel = |kind| MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };

        app.handle_mouse(wheel(MouseEventKind::ScrollDown), Instant::now());
        let Mode::TraceBrowser(browser) = &app.mode else {
            panic!("expected trace browser");
        };
        assert_eq!(browser.selected_observation, 3);

        app.handle_mouse(wheel(MouseEventKind::ScrollUp), Instant::now());
        let Mode::TraceBrowser(browser) = &app.mode else {
            panic!("expected trace browser");
        };
        assert_eq!(browser.selected_observation, 0);
    }

    #[test]
    fn tree_arrows_follow_render_order_when_chronology_differs() {
        let mut b = browser_with_tree();
        let mut by_id: std::collections::HashMap<String, _> = std::mem::take(&mut b.observations)
            .into_iter()
            .map(|o| (o.id.clone(), o))
            .collect();
        // This is chronological order: a child can start before the agent
        // container is observed. The tree renderer moves both children below
        // that parent, so keyboard navigation must make the same move.
        b.observations = ["gen", "grep", "bash", "agent", "read"]
            .into_iter()
            .map(|id| by_id.remove(id).unwrap())
            .collect();
        b.detail_view = DetailView::Tree;

        let rendered: Vec<&str> = b
            .visible_rows()
            .iter()
            .map(|&i| b.observations[i].id.as_str())
            .collect();
        assert_eq!(rendered, ["gen", "bash", "agent", "grep", "read"]);

        b.selected_observation = 0;
        for expected in ["bash", "agent", "grep", "read"] {
            b.step_observation(1);
            assert_eq!(b.observations[b.selected_observation].id, expected);
        }
        for expected in ["grep", "agent", "bash", "gen"] {
            b.step_observation(-1);
            assert_eq!(b.observations[b.selected_observation].id, expected);
        }
    }

    #[test]
    fn shift_t_opens_the_trace_browser_and_t_toggles_tracing() {
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('T')), &ctx()),
            Action::OpenTraceBrowser
        ));
        assert!(matches!(
            dispatch(&Mode::Control, &key(KeyCode::Char('t')), &ctx()),
            Action::ToggleTracing
        ));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        // tracing off: opening still works and explains itself
        app.handle_key(&key(KeyCode::Char('T')), Instant::now());
        match &app.mode {
            Mode::TraceBrowser(b) => assert!(b.error.is_some()),
            other => panic!("expected the trace browser, got {other:?}"),
        }
        assert!(matches!(
            dispatch(&app.mode, &key(KeyCode::Tab), &ctx()),
            Action::BrowserKey
        ));
        app.handle_key(&key(KeyCode::Esc), Instant::now());
        assert!(matches!(app.mode, Mode::Control));
    }

    #[test]
    fn history_mode_dispatches_history_key() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        app.mode = Mode::SessionHistory(HistoryState::new(None));
        assert!(matches!(
            dispatch(&app.mode, &key(KeyCode::Tab), &ctx()),
            Action::HistoryKey
        ));
    }

    #[test]
    fn history_navigation_and_tab() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(vec![], None, tx);
        let mut hist = HistoryState::new(None);
        hist.log_lines = vec![
            ratatui::text::Line::raw("line1"),
            ratatui::text::Line::raw("line2"),
        ];
        // 1-row viewport: two lines of content leave exactly one scroll step
        hist.viewport_rows.set(1);
        hist.scroll_offset = 0;
        app.mode = Mode::SessionHistory(hist);

        // Tab toggles to LogDetail
        app.handle_key(&key(KeyCode::Tab), Instant::now());
        if let Mode::SessionHistory(ref h) = app.mode {
            assert_eq!(h.focused_pane, HistoryPane::LogDetail);
        } else {
            panic!("Expected SessionHistory mode");
        }

        // Down in LogDetail scrolls
        app.handle_key(&key(KeyCode::Down), Instant::now());
        if let Mode::SessionHistory(ref h) = app.mode {
            assert_eq!(h.scroll_offset, 1);
        }

        // Esc returns to Control
        app.handle_key(&key(KeyCode::Esc), Instant::now());
        assert!(matches!(app.mode, Mode::Control));
    }

    #[test]
    fn trace_browser_refresh_if_live_updates_sessions_and_tracks_selection() {
        use crate::tracing::pricing::PriceTable;
        use crate::tracing::store::model::{LaunchRow, StoreOp, TraceRow, TraceStatus};
        use crate::tracing::store::{OpenOptions, open_rw};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("traces.db");
        let mut store = open_rw(
            &db_path,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-1".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();

        let make_launch = |id: &str, session_key: &str, started_ns: i64| LaunchRow {
            id: id.into(),
            run_id: "run-1".into(),
            agent_mux_session: 0,
            profile: "Claude Code".into(),
            provider: "claude".into(),
            cwd: "/proj".into(),
            project_slug: "-proj".into(),
            content_mode: "full".into(),
            correlation_plan: "deterministic".into(),
            correlation: Some("deterministic".into()),
            session_key: Some(session_key.into()),
            injected_session_id: true,
            attached: false,
            started_ns,
            ended_ns: None,
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: vec![],
            metadata: None,
        };

        let make_trace = |id: &str, session_key: &str, start_ns: i64| TraceRow {
            id: id.into(),
            session_key: session_key.into(),
            provider: "claude".into(),
            session_id: session_key
                .strip_prefix("claude:")
                .unwrap_or(session_key)
                .into(),
            launch_id: None,
            ordinal: 1,
            name: "turn 1".into(),
            status: TraceStatus::Closed,
            start_ns,
            end_ns: Some(start_ns + 1000),
            input: Some("hello".into()),
            output: None,
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            metadata: None,
        };

        // Insert first session
        store
            .apply(&[
                StoreOp::Launch(make_launch("l1", "claude:s1", 1_000_000)),
                StoreOp::Trace(make_trace("t1", "claude:s1", 1_000_000)),
            ])
            .unwrap();

        let mut browser = TraceBrowserState::new(Some(&db_path), None);
        assert_eq!(browser.sessions.len(), 1);
        assert_eq!(browser.sessions[0].key, "claude:s1");
        assert_eq!(browser.selected_session, 0);

        // Insert second, newer session
        store
            .apply(&[
                StoreOp::Launch(make_launch("l2", "claude:s2", 2_000_000)),
                StoreOp::Trace(make_trace("t2", "claude:s2", 2_000_000)),
            ])
            .unwrap();

        // Focused on Sessions at row 0: refresh should keep row 0, showing newer session s2
        browser.focused = BrowserPane::Sessions;
        browser.selected_session = 0;
        let t1 = Instant::now() + Duration::from_millis(600);
        browser.refresh_if_live(t1);

        assert_eq!(browser.sessions.len(), 2);
        assert_eq!(browser.selected_session, 0);
        assert_eq!(browser.sessions[0].key, "claude:s2");
        assert_eq!(browser.sessions[1].key, "claude:s1");

        // Now user navigates to row 1 (older session s1)
        browser.selected_session = 1;

        // Insert third session s3, even newer
        store
            .apply(&[
                StoreOp::Launch(make_launch("l3", "claude:s3", 3_000_000)),
                StoreOp::Trace(make_trace("t3", "claude:s3", 3_000_000)),
            ])
            .unwrap();

        let t2 = t1 + Duration::from_millis(600);
        browser.refresh_if_live(t2);

        assert_eq!(browser.sessions.len(), 3);
        // Since user was on s1 (row 1 previously), selection tracks key "claude:s1", now at index 2
        assert_eq!(browser.selected_session, 2);
        assert_eq!(browser.sessions[browser.selected_session].key, "claude:s1");
    }
}
