use crate::config::Profile;
use crate::events::AppEvent;
use crate::history::{self, SessionSummary};
use crate::keys::encode_key;
use crate::mouse::{WheelRoute, encode_mouse, route_wheel};
use crate::search::SearchState;
use crate::selection::{self, Pos, Selection};
use crate::session::Session;
use crate::status::Status;
use crate::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;
use tokio::sync::mpsc::Sender;

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
    KillSelected,
    EnterConfirmKill,
    RemoveSelected,
    RespawnSelected,
    ToggleTracing,
    CancelToControl,
    ForwardBytes(Vec<u8>),
    SendLiteralDetachKey,
    /// NewSession mode: App routes the key to the DialogState it owns.
    DialogKey,
    /// SessionHistory mode: App routes the key to the HistoryState it owns.
    HistoryKey,
    /// TraceBrowser mode: App routes the key to the TraceBrowserState it owns.
    BrowserKey,
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

pub struct DispatchCtx {
    pub selected_status: Option<Status>,
    pub any_working: bool,
    pub just_detached: bool,
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
                KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                KeyCode::Char(c @ '1'..='9') => Action::SelectSession(c as usize - '1' as usize),
                KeyCode::Enter if ctx.selected_status.is_some() => Action::Attach,
                KeyCode::Char('n') => Action::OpenNewSession,
                KeyCode::Char('l') | KeyCode::Char('L') => Action::OpenSessionHistory,
                KeyCode::Char('t') => Action::ToggleTracing,
                KeyCode::Char('T') => Action::OpenTraceBrowser,
                KeyCode::Char('?') | KeyCode::F(1) => Action::OpenHelp,
                KeyCode::Char('x') => match ctx.selected_status {
                    Some(Status::Exited(_)) => Action::RemoveSelected,
                    Some(_) => Action::EnterConfirmKill,
                    None => Action::None,
                },
                KeyCode::Char('r') => match ctx.selected_status {
                    Some(Status::Exited(_)) => Action::RespawnSelected,
                    _ => Action::None,
                },
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
                match encode_key(key) {
                    Some(bytes) => Action::ForwardBytes(bytes),
                    None => Action::None,
                }
            }
        }
        Mode::NewSession(_) => Action::DialogKey,
        Mode::SessionHistory(_) => Action::HistoryKey,
        Mode::TraceBrowser(_) => Action::BrowserKey,
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
    pub dir_entries: Vec<String>,
    pub dir_selected_idx: Option<usize>,
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
}

impl DialogState {
    pub fn new(profiles: &[Profile]) -> Self {
        let first = profiles.first();
        let dir = default_dir_for_profile(first);
        let resolved = resolve_working_dir(&dir);
        let dir_entries = list_subdirectories(&resolved);
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
            dir_entries,
            dir_selected_idx: None,
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
        }
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
        let resolved = resolve_working_dir(&self.dir);
        self.dir_entries = list_subdirectories(&resolved);
        if let Some(idx) = self.dir_selected_idx
            && idx >= self.dir_entries.len()
        {
            self.dir_selected_idx = if self.dir_entries.is_empty() {
                None
            } else {
                Some(self.dir_entries.len() - 1)
            };
        }
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
        let resolved = resolve_working_dir(&self.dir);
        if let Some(parent) = resolved.parent() {
            self.dir = parent.to_string_lossy().into_owned();
            self.dir_edited = true;
            self.dir_selected_idx = None;
            self.refresh_dir_entries();
        }
    }

    pub fn navigate_into(&mut self, sub: &str) {
        let resolved = resolve_working_dir(&self.dir);
        let target = resolved.join(sub);
        self.dir = target.to_string_lossy().into_owned();
        self.dir_edited = true;
        self.dir_selected_idx = None;
        self.refresh_dir_entries();
    }

    pub fn handle_key(&mut self, key: &KeyEvent, profiles: &[Profile]) -> DialogResult {
        match key.code {
            KeyCode::Esc => return DialogResult::Cancel,
            KeyCode::Tab => {
                self.step_field(1);
                self.dir_selected_idx = None;
                return DialogResult::Consumed;
            }
            KeyCode::BackTab => {
                self.step_field(-1);
                self.dir_selected_idx = None;
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
            DialogField::Dir => match key.code {
                KeyCode::Enter => {
                    if let Some(idx) = self.dir_selected_idx
                        && let Some(entry) = self.dir_entries.get(idx).cloned()
                    {
                        if entry == ".." {
                            self.navigate_to_parent();
                            return DialogResult::Consumed;
                        } else {
                            self.navigate_into(&entry);
                            return DialogResult::Submit;
                        }
                    }
                    return DialogResult::Submit;
                }
                KeyCode::Right => {
                    if let Some(idx) = self.dir_selected_idx
                        && let Some(entry) = self.dir_entries.get(idx).cloned()
                    {
                        if entry == ".." {
                            self.navigate_to_parent();
                        } else {
                            self.navigate_into(&entry);
                        }
                    } else if !self.dir_entries.is_empty() {
                        self.dir_selected_idx = Some(0);
                    }
                }
                KeyCode::Left => {
                    self.navigate_to_parent();
                }
                KeyCode::Down => {
                    if self.dir_entries.is_empty() {
                        self.dir_selected_idx = None;
                    } else {
                        self.dir_selected_idx = match self.dir_selected_idx {
                            None => Some(0),
                            Some(i) => Some((i + 1).min(self.dir_entries.len().saturating_sub(1))),
                        };
                    }
                }
                KeyCode::Up => {
                    if let Some(i) = self.dir_selected_idx {
                        if i == 0 {
                            self.dir_selected_idx = None;
                        } else {
                            self.dir_selected_idx = Some(i - 1);
                        }
                    }
                }
                KeyCode::Char(c) => {
                    self.dir.push(c);
                    self.dir_edited = true;
                    self.dir_selected_idx = None;
                    self.refresh_dir_entries();
                }
                KeyCode::Backspace => {
                    self.dir.pop();
                    self.dir_edited = true;
                    self.dir_selected_idx = None;
                    self.refresh_dir_entries();
                }
                _ => {}
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
            DetailView::Tree => "tree",
            DetailView::Timeline => "timeline",
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
    /// `K`: the left column lists skills instead of sessions.
    pub skills_pane: bool,
    pub skills: Vec<crate::tracing::inventory::SkillReport>,
    pub selected_skill: usize,
    cwd: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
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
            skills_pane: false,
            skills: Vec::new(),
            selected_skill: 0,
            cwd: current_dir.map(std::path::Path::to_path_buf),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from),
        };
        state.reload_sessions();
        state
    }
    /// Where the Skills pane reads the user's home definitions from.
    pub fn with_home(mut self, home: Option<std::path::PathBuf>) -> Self {
        if home.is_some() {
            self.home = home;
        }
        self
    }

    /// `K`: the left column shows the skill inventory joined to the store
    /// instead of sessions, and back.
    pub fn toggle_skills_pane(&mut self) {
        self.skills_pane = !self.skills_pane;
        if self.skills_pane {
            self.load_skills();
            self.focused = BrowserPane::Sessions;
        }
    }

    pub fn load_skills(&mut self) {
        use crate::tracing::inventory::{inventory_all, skill_reports};
        let cwd = self
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let home = self
            .home
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let defs = inventory_all(&cwd, &home);
        let (stats, prompts) = match &self.conn {
            Some(conn) => (
                crate::tracing::store::query::skill_stats(conn).unwrap_or_default(),
                crate::tracing::store::query::prompt_rows(conn, 5000).unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        self.skills = skill_reports(&defs, &stats, &prompts);
        self.selected_skill = self.selected_skill.min(self.skills.len().saturating_sub(1));
    }

    pub fn step_skill(&mut self, delta: isize) {
        if self.skills.is_empty() {
            return;
        }
        let max = self.skills.len() as isize - 1;
        self.selected_skill = (self.selected_skill as isize + delta).clamp(0, max) as usize;
    }

    /// `Enter` on a skill: the Turns pane shows the turns that loaded it.
    pub fn filter_by_skill(&mut self) {
        let Some(report) = self.skills.get(self.selected_skill) else {
            return;
        };
        let Some(conn) = &self.conn else {
            return;
        };
        let names = report
            .def
            .as_ref()
            .map(|d| d.store_names())
            .unwrap_or_else(|| vec![report.name.clone()]);
        let label = report.name.clone();
        let mut turns = Vec::new();
        for name in names {
            if let Ok(found) = crate::tracing::store::query::traces_with_skill(conn, &name, 200) {
                for t in found {
                    if !turns
                        .iter()
                        .any(|u: &crate::tracing::store::query::TraceStat| u.id == t.id)
                    {
                        turns.push(t);
                    }
                }
            }
        }
        turns.sort_by_key(|t| std::cmp::Reverse(t.start_ns));
        self.turns = turns;
        self.selected_turn = 0;
        self.search_query = Some(format!("skill: {label}"));
        self.focused = BrowserPane::Turns;
        self.error = None;
        self.load_observations();
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
        // open at the newest turn
        self.selected_turn = self.turns.len().saturating_sub(1);
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
                lines.push(Line::styled("(no content stored — metadata mode)", dim));
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
                let rows = crate::tracing::view::tree_rows(&self.observations, &self.collapsed);
                let visible: std::collections::HashSet<&str> =
                    rows.iter().map(|r| r.obs.id.as_str()).collect();
                self.observations
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| visible.contains(o.id.as_str()))
                    .map(|(i, _)| i)
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

    /// Cycles list → tree → timeline. Leaving the tree keeps the fold set
    /// so coming back looks the way it was left; the selection is pulled
    /// back onto a visible row.
    pub fn cycle_detail_view(&mut self) {
        self.detail_view = self.detail_view.next();
        self.expanded = false;
        self.scroll_offset = 0;
        let rows = self.visible_rows();
        if !rows.is_empty() && !rows.contains(&self.selected_observation) {
            // the selection fell inside a folded subtree: land on its
            // nearest visible ancestor
            let fallback = rows
                .iter()
                .rev()
                .find(|&&i| i < self.selected_observation)
                .copied()
                .unwrap_or(rows[0]);
            self.selected_observation = fallback;
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
        let has_children = crate::tracing::view::tree_rows(&self.observations, &Default::default())
            .iter()
            .any(|r| r.obs.id == id && r.has_children);
        if !has_children {
            return;
        }
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
    }

    /// Live sessions change under the browser: re-query at most once a
    /// second while the selected session still has an open turn.
    pub fn refresh_if_live(&mut self, now: Instant) {
        if now.duration_since(self.last_refresh) < std::time::Duration::from_secs(1) {
            return;
        }
        let live = self
            .sessions
            .get(self.selected_session)
            .is_some_and(|s| s.open_turns > 0)
            || self.turns.iter().any(|t| t.status == "open");
        if !live || self.search_query.is_some() {
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
        if let Some(key) = selected_key
            && let Some(idx) = self.sessions.iter().position(|s| s.key == key)
        {
            self.selected_session = idx;
            self.turns = crate::tracing::store::query::list_traces(conn, &key).unwrap_or_default();
            self.selected_turn = selected_turn_id
                .and_then(|id| self.turns.iter().position(|t| t.id == id))
                .unwrap_or(self.turns.len().saturating_sub(1));
            if let Some(t) = self.turns.get(self.selected_turn) {
                self.observations = crate::tracing::store::query::list_observations(conn, &t.id)
                    .unwrap_or_default();
                self.selected_observation = selected_obs_id
                    .and_then(|id| self.observations.iter().position(|o| o.id == id))
                    .unwrap_or(self.observations.len().saturating_sub(1));
                let keep_scroll = self.scroll_offset;
                self.rebuild_detail();
                self.scroll_offset = keep_scroll.min(self.max_scroll());
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
    trace_db_path: Option<std::path::PathBuf>,
}

impl App {
    pub fn new(
        profiles: Vec<Profile>,
        tracing: Option<crate::tracing::TraceRuntime>,
        tx: Sender<AppEvent>,
    ) -> App {
        let trace_db_path = tracing.as_ref().map(|rt| rt.db_path().to_path_buf());
        App {
            sessions: Vec::new(),
            selected: 0,
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
        }
    }

    /// Periodic housekeeping driven by `AppEvent::Tick`: the trace browser
    /// re-queries live sessions.
    pub fn on_tick(&mut self, now: Instant) {
        if let Mode::TraceBrowser(browser) = &mut self.mode {
            browser.refresh_if_live(now);
        }
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
        let (rows, cols) = self.pane_size;
        let plan = self
            .tracing
            .as_ref()
            .and_then(|rt| rt.plan_launch(&profile, &dir));
        let (extra_args, extra_env): (&[String], &[(String, String)]) = match &plan {
            Some(p) => (&p.extra_args, &p.extra_env),
            None => (&[], &[]),
        };
        let mut session = Session::spawn(
            id,
            profile,
            dir,
            rows,
            cols,
            self.tx.clone(),
            extra_args,
            extra_env,
        )?;
        if let (Some(rt), Some(plan)) = (self.tracing.as_mut(), plan) {
            session.trace = Some(rt.start_session(id, plan));
        }
        Ok(session)
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
        let ctx = DispatchCtx {
            selected_status: self.sessions.get(self.selected).map(|s| s.status(now)),
            any_working: self
                .sessions
                .iter()
                .any(|s| matches!(s.status(now), Status::Working)),
            just_detached: self.just_detached,
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
            PageUp,
            PageDown,
            Top,
            Bottom,
            Copy,
            Paste,
            OpenSearch,
        }
        let chord = match (key.code, shift, ctrl) {
            (KeyCode::PageUp, true, _) => Chord::PageUp,
            (KeyCode::PageDown, true, _) => Chord::PageDown,
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
            _ => return false,
        };
        let page = i32::from(self.pane_size.0.saturating_sub(1).max(1));
        match chord {
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
            if matches!(ev.kind, MouseEventKind::ScrollUp) {
                browser.scroll_offset = browser.scroll_offset.saturating_sub(3);
            } else if matches!(ev.kind, MouseEventKind::ScrollDown) {
                browser.scroll_offset = browser
                    .scroll_offset
                    .saturating_add(3)
                    .min(browser.max_scroll());
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
        // Left click on a sidebar row selects that session (and follows it
        // when attached). Only a fresh Down, never a drag that slid over.
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self.selection.as_ref().is_none_or(|a| !a.dragging)
            && ev.column > 0
            && ev.column < ui::SIDEBAR_WIDTH.saturating_sub(1)
            && ev.row >= 1
        {
            let visible = usize::from(self.pane_size.0);
            let row = usize::from(ev.row) - 1;
            if row < visible {
                let idx = ui::sidebar_window(self.selected, self.sessions.len(), visible) + row;
                if idx < self.sessions.len() && idx != self.selected {
                    self.selection = None;
                    self.selected = idx;
                    if matches!(self.mode, Mode::Attached)
                        && let Some(s) = self.sessions.get_mut(idx)
                    {
                        s.tracker.on_attach();
                    }
                }
            }
            return;
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
        let (lcol, lrow) = match ui::pane_local(ev.column, ev.row, self.pane_size) {
            Some(p) => p,
            None if finalizing_drag => ui::pane_clamped(ev.column, ev.row, self.pane_size),
            None => return,
        };
        let attached = matches!(self.mode, Mode::Attached);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        // read child terminal state up front so no borrow is held across
        // the mutating calls below
        let Some((mouse_mode, enc, alt, app_cursor)) = self.sessions.get(self.selected).map(|s| {
            let sc = s.parser.screen();
            (
                sc.mouse_protocol_mode(),
                sc.mouse_protocol_encoding(),
                sc.alternate_screen(),
                sc.application_cursor(),
            )
        }) else {
            return;
        };
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                match route_wheel(shift, attached, mouse_mode, alt) {
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
            Action::MoveDown => {
                self.selection = None;
                if !self.sessions.is_empty() {
                    self.selected = (self.selected + 1).min(self.sessions.len() - 1);
                }
            }
            Action::MoveUp => {
                self.selection = None;
                self.selected = self.selected.saturating_sub(1);
            }
            Action::SelectSession(idx) => {
                if idx < self.sessions.len() {
                    self.selection = None;
                    self.selected = idx;
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
            }
            Action::RemoveSelected => {
                self.selection = None;
                if self.selected < self.sessions.len() {
                    self.sessions.remove(self.selected);
                    if self.selected >= self.sessions.len() {
                        self.selected = self.sessions.len().saturating_sub(1);
                    }
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
                let home = self.tracing.as_ref().map(|rt| rt.home().to_path_buf());
                self.mode = Mode::TraceBrowser(Box::new(
                    TraceBrowserState::new(self.trace_db_path.as_deref(), cur_dir.as_deref())
                        .with_home(home),
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
                        self.mode = Mode::Control;
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
            KeyCode::Char('K') => browser.toggle_skills_pane(),
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
                BrowserPane::Sessions if browser.skills_pane => browser.filter_by_skill(),
                BrowserPane::Sessions => browser.focused = BrowserPane::Turns,
                BrowserPane::Turns => browser.focused = BrowserPane::Detail,
                BrowserPane::Detail => browser.toggle_expanded(),
            },
            KeyCode::Down | KeyCode::Char('j') => match browser.focused {
                BrowserPane::Sessions if browser.skills_pane => browser.step_skill(1),
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
                BrowserPane::Sessions if browser.skills_pane => browser.step_skill(-1),
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

    fn resume_history_session(&mut self, summary: &SessionSummary) {
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
                self.mode = Mode::Control;
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
        // Fresh id: a stale reader thread from the dead session may still
        // send PtyExit for the old id; it must not find the new session.
        // Launch extras are re-planned fresh too (never stored in the
        // profile), so a respawned Claude tab gets a NEW session uuid.
        let id = self.next_id;
        match self.spawn_traced(id, profile, dir) {
            Ok(session) => {
                self.next_id += 1;
                self.sessions[self.selected] = session;
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
        if self.search.is_some() && self.sessions.get(self.selected).map(|s| s.id) == Some(id) {
            self.rerun_search();
        }
    }

    pub fn handle_pty_exit(&mut self, id: usize) {
        if let Some(i) = self.session_index(id) {
            self.sessions[i].mark_exited();
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
            // if we were attached to it, drop back to Control
            if self.attached() == Some(i) {
                self.mode = Mode::Control;
            }
        }
    }

    pub fn set_pane_size(&mut self, rows: u16, cols: u16) {
        if self.pane_size == (rows, cols) {
            return;
        }
        self.pane_size = (rows, cols);
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
            DialogField::Profile,
        ] {
            d.handle_key(&key(KeyCode::Tab), &ps);
            assert_eq!(d.field, expected);
        }
        d.handle_key(&key(KeyCode::BackTab), &ps);
        assert_eq!(d.field, DialogField::OneShot);

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
        assert!(d.dir_entries.contains(&"sub_a".to_string()));
        assert!(d.dir_entries.contains(&"sub_b".to_string()));

        // Down arrow selects first entry
        d.handle_key(&key(KeyCode::Down), &profiles);
        assert_eq!(d.dir_selected_idx, Some(0));

        // Up arrow goes back to text field
        d.handle_key(&key(KeyCode::Up), &profiles);
        assert_eq!(d.dir_selected_idx, None);

        // Find index of sub_a
        let sub_a_idx = d.dir_entries.iter().position(|e| e == "sub_a").unwrap();
        d.dir_selected_idx = Some(sub_a_idx);

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
            is_error: false,
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
}
