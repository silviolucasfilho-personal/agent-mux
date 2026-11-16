//! The Factory screen (`Mode::Factory`): the work items of one
//! repository on the left, grouped by what they wait on, and the selected
//! item in tabs on the right (Item, Line, Thread, Result). The last row
//! is the repository's brain.
//!
//! The screen starts nothing itself: `n`, `a` and `r` start
//! `agent-mux factory …` as a process of its own (`factory::intake::spawn_cli`),
//! which runs the line headless and delivers. The screen reads the store
//! and the run's journal, so an item started from a shell looks the same.
//!
//! Drawing: `crate::ui::factory`.

use super::{App, Mode, Notice};
use crate::factory::{self, Item, Source, State, brain, intake, store};
use crate::workflows::{context, journal};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the open screen reads the store again.
const REFRESH: Duration = Duration::from_secs(2);
/// Closed items older than this are hidden.
const CLOSED_TTL_NS: i64 = 14 * 24 * 3600 * 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Running,
    NeedsYou,
    Queued,
    Delivered,
    Closed,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Running,
        Group::NeedsYou,
        Group::Queued,
        Group::Delivered,
        Group::Closed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Running => "running",
            Group::NeedsYou => "needs you",
            Group::Queued => "queued",
            Group::Delivered => "delivered",
            Group::Closed => "closed",
        }
    }

    pub fn of(state: State) -> Group {
        match state {
            State::Running => Group::Running,
            s if s.needs_you() => Group::NeedsYou,
            State::Queued => Group::Queued,
            State::Delivered | State::Answered => Group::Delivered,
            _ => Group::Closed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Heading(Group, usize),
    /// An index into `FactoryView.items`.
    Item(usize),
    Brain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Item,
    Line,
    Thread,
    Result,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Item, Tab::Line, Tab::Thread, Tab::Result];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Item => "Item",
            Tab::Line => "Line",
            Tab::Thread => "Thread",
            Tab::Result => "Result",
        }
    }
}

/// A line the operator is typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A new item: what is wanted, or `#<issue>`.
    New(String),
    /// The answer to triage's questions for the item with this id.
    Answer { item: String, text: String },
}

impl Input {
    pub fn text(&self) -> &str {
        match self {
            Input::New(t) | Input::Answer { text: t, .. } => t,
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self {
            Input::New(t) | Input::Answer { text: t, .. } => t,
        }
    }
}

/// What the selected item's tabs show, read when the selection changes
/// and on every refresh.
#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub item: String,
    pub events: Vec<store::Event>,
    /// The latest run's journal: one entry per finished session.
    pub journal: Vec<journal::Entry>,
    /// The run's result (`build`), when it has one.
    pub result: Value,
    /// The process working the item is there.
    pub process: bool,
}

/// What the App does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    Reload,
    /// Start `agent-mux factory <args>`; `what` names it in the notice.
    Start {
        what: String,
        args: Vec<String>,
    },
    Stop(String),
    OpenRun(String),
    EditBrain,
    OpenUrl(String),
    Notice(String),
}

#[derive(Debug)]
pub struct FactoryView {
    /// The repository the screen shows; `None` lists every item.
    pub workspace: Option<PathBuf>,
    pub repo_name: String,
    pub items: Vec<Item>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub tab: Tab,
    pub scroll: u16,
    pub filter: String,
    pub filtering: bool,
    pub input: Option<Input>,
    /// The id of the item `x` asked to stop, waiting on `y`.
    pub confirm_stop: Option<String>,
    pub detail: Detail,
    pub brain: String,
    pub error: Option<String>,
    pub last_refresh: Instant,
}

impl FactoryView {
    pub fn new(workspace: Option<PathBuf>) -> FactoryView {
        // the folder's name: what the operator calls the repository
        let repo_name = workspace
            .as_deref()
            .map(|w| {
                w.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| w.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "every repository".into());
        FactoryView {
            workspace,
            repo_name,
            items: Vec::new(),
            rows: vec![Row::Brain],
            cursor: 0,
            tab: Tab::Item,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            input: None,
            confirm_stop: None,
            detail: Detail::default(),
            brain: String::new(),
            error: None,
            last_refresh: Instant::now(),
        }
    }

    pub fn text_entry_active(&self) -> bool {
        self.filtering || self.input.is_some()
    }

    pub fn confirmation_active(&self) -> bool {
        self.confirm_stop.is_some()
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    pub fn selected_item(&self) -> Option<&Item> {
        match self.selected_row()? {
            Row::Item(i) => self.items.get(*i),
            _ => None,
        }
    }

    /// How many items wait on the operator.
    pub fn needs_you(&self) -> usize {
        self.items.iter().filter(|i| i.state.needs_you()).count()
    }

    fn matches(&self, it: &Item) -> bool {
        let f = self.filter.trim().to_lowercase();
        f.is_empty()
            || it.title.to_lowercase().contains(&f)
            || it.handle().to_lowercase().contains(&f)
            || it.state.as_str().contains(&f)
            || it.kind.as_deref().is_some_and(|k| k.contains(&f))
    }

    /// Replaces the items and rebuilds the rows, keeping the cursor on
    /// the item it was on.
    pub fn set_items(&mut self, items: Vec<Item>, now_ns: i64) {
        let anchor = self.selected_item().map(|i| i.id.clone());
        // with no item listed the cursor has nowhere else to be: that is not a choice
        let on_brain = self.rows.len() > 1 && matches!(self.selected_row(), Some(Row::Brain));
        self.items = items;
        let mut rows = Vec::new();
        for group in Group::ALL {
            let of: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| Group::of(it.state) == group && self.matches(it))
                .filter(|(_, it)| {
                    !matches!(group, Group::Delivered | Group::Closed)
                        || now_ns - it.updated_ns <= CLOSED_TTL_NS
                })
                .map(|(i, _)| i)
                .collect();
            if of.is_empty() {
                continue;
            }
            rows.push(Row::Heading(group, of.len()));
            rows.extend(of.into_iter().map(Row::Item));
        }
        rows.push(Row::Brain);
        self.rows = rows;
        let found = anchor.and_then(|id| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item(i) if self.items[*i].id == id))
        });
        self.cursor = match found {
            Some(i) => i,
            None if on_brain => self.rows.len() - 1,
            None => self.cursor.min(self.rows.len() - 1),
        };
        if matches!(self.rows.get(self.cursor), Some(Row::Heading(..))) {
            self.step(1);
        }
    }

    /// Moves the cursor by `delta` rows, never onto a heading.
    fn step(&mut self, delta: isize) {
        let len = self.rows.len() as isize;
        let mut at = self.cursor as isize;
        loop {
            at += delta.signum();
            if at < 0 || at >= len {
                return;
            }
            if !matches!(self.rows[at as usize], Row::Heading(..)) {
                self.cursor = at as usize;
                self.scroll = 0;
                return;
            }
        }
    }

    /// Puts the cursor on item `id`. False when it is not listed.
    pub fn select(&mut self, id: &str) -> bool {
        let at = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if self.items[*i].id == id));
        if let Some(i) = at {
            self.cursor = i;
            self.scroll = 0;
        }
        at.is_some()
    }

    fn typed(&mut self, key: &KeyEvent) -> Outcome {
        let Some(input) = self.input.as_mut() else {
            return Outcome::None;
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                input.text_mut().pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.text_mut().push(c)
            }
            KeyCode::Enter => {
                let done = self.input.take();
                let Some(done) = done else {
                    return Outcome::None;
                };
                let text = done.text().trim().to_string();
                if text.is_empty() {
                    return Outcome::None;
                }
                let mut args: Vec<String> = Vec::new();
                let what = match &done {
                    Input::New(_) => {
                        args.push("run".into());
                        // `#12` or `12` names an issue; anything else is the request
                        let issue = text.trim_start_matches('#');
                        if !issue.is_empty() && issue.chars().all(|c| c.is_ascii_digit()) {
                            args.extend(["--issue".into(), issue.to_string()]);
                            format!("issue #{issue}")
                        } else {
                            args.push(text.clone());
                            factory::title_of(&text, 40)
                        }
                    }
                    Input::Answer { item, .. } => {
                        args.extend(["answer".into(), item.clone(), text.clone()]);
                        format!("the answer for {item}")
                    }
                };
                if let Some(ws) = &self.workspace {
                    args.extend(["--workspace".into(), ws.to_string_lossy().into_owned()]);
                }
                return Outcome::Start { what, args };
            }
            _ => {}
        }
        Outcome::None
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.input.is_some() {
            return self.typed(key);
        }
        if let Some(id) = self.confirm_stop.clone() {
            self.confirm_stop = None;
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Outcome::Stop(id),
                _ => Outcome::None,
            };
        }
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                    return Outcome::Reload;
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                    return Outcome::Reload;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.push(c);
                    return Outcome::Reload;
                }
                _ => {}
            }
            return Outcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let item = self.selected_item().cloned();
        let on_brain = matches!(self.selected_row(), Some(Row::Brain));
        match key.code {
            KeyCode::Char('r') if ctrl => return Outcome::Reload,
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                return Outcome::Reload;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.step(1);
                return Outcome::Reload;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(-1);
                return Outcome::Reload;
            }
            KeyCode::Char(c @ '1'..='4') => {
                self.tab = Tab::ALL[c as usize - '1' as usize];
                self.scroll = 0;
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                let at = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(at + 1) % Tab::ALL.len()];
                self.scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                let at = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(at + Tab::ALL.len() - 1) % Tab::ALL.len()];
                self.scroll = 0;
            }
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('n') => {
                if self.workspace.is_none() {
                    return Outcome::Notice(
                        "open the factory from a session in a git repository to start an item"
                            .into(),
                    );
                }
                self.input = Some(Input::New(String::new()));
            }
            KeyCode::Char('a') => match item {
                Some(it) if it.state == State::NeedsAnswer => {
                    self.tab = Tab::Result;
                    self.input = Some(Input::Answer {
                        item: it.id,
                        text: String::new(),
                    });
                }
                Some(it) => {
                    return Outcome::Notice(format!(
                        "{} is {}, not waiting for an answer",
                        it.handle(),
                        it.state.as_str()
                    ));
                }
                None => {}
            },
            KeyCode::Char('r') => {
                if let Some(it) = item {
                    // a running item whose process is gone is taken over
                    let lost = it.state == State::Running && !self.detail.process;
                    if !it.state.may_retry() && !lost {
                        return Outcome::Notice(match it.state {
                            State::Running => format!("{} is running", it.handle()),
                            State::NeedsAnswer => {
                                format!("{} waits for an answer: [a]", it.handle())
                            }
                            s => format!("{} is {} and is not run again", it.handle(), s.as_str()),
                        });
                    }
                    let mut args = vec!["retry".to_string(), it.id.clone()];
                    if lost {
                        args.push("--force".into());
                    }
                    return Outcome::Start {
                        what: it.handle(),
                        args,
                    };
                }
            }
            KeyCode::Char('x') => match item {
                Some(it)
                    if !matches!(
                        it.state,
                        State::Delivered | State::Answered | State::Cancelled
                    ) =>
                {
                    self.confirm_stop = Some(it.id)
                }
                Some(it) => {
                    return Outcome::Notice(format!(
                        "{} is {} already",
                        it.handle(),
                        it.state.as_str()
                    ));
                }
                None => {}
            },
            KeyCode::Char('o') => {
                if let Some(url) = item.and_then(|it| it.pr_url) {
                    return Outcome::OpenUrl(url);
                }
                return Outcome::Notice("this item has no pull request".into());
            }
            KeyCode::Char('e') if on_brain => return Outcome::EditBrain,
            KeyCode::Enter if on_brain => return Outcome::EditBrain,
            KeyCode::Enter => match item.and_then(|it| it.run_id) {
                Some(run) => return Outcome::OpenRun(run),
                None => return Outcome::Notice("this item has not run yet".into()),
            },
            _ => {}
        }
        Outcome::None
    }
}

/// The argument words of a factory command as one line, for a notice.
fn words(args: &[String]) -> String {
    args.first().cloned().unwrap_or_default()
}

impl App {
    /// The repository the Factory screen opens on: the selected session's
    /// folder, else the launch directory, when it is a git repository.
    fn factory_workspace(&self) -> Option<PathBuf> {
        let dir = self
            .sessions
            .get(self.selected)
            .map(|s| s.dir.clone())
            .or_else(|| std::env::current_dir().ok())?;
        let dir = dir.canonicalize().ok()?;
        crate::loops::worktree::is_git_repo(&dir).then_some(dir)
    }

    pub fn open_factory(&mut self) {
        let ws = self
            .factory_workspace_override
            .clone()
            .or_else(|| self.factory_workspace());
        self.open_factory_in(ws);
    }

    pub fn open_factory_in(&mut self, workspace: Option<PathBuf>) {
        let mut view = FactoryView::new(workspace);
        self.fill_factory(&mut view);
        self.mode = Mode::Factory(Box::new(view));
    }

    /// The Factory screen on item `id`, wherever its repository is.
    pub fn open_factory_on(&mut self, id: &str, workspace: &str) {
        self.open_factory_in(Some(PathBuf::from(workspace)));
        if let Mode::Factory(v) = &mut self.mode
            && v.select(id)
        {
            let mut view = std::mem::replace(v, Box::new(FactoryView::new(None)));
            self.fill_factory(&mut view);
            self.mode = Mode::Factory(view);
        }
    }

    /// Reads the items, the brain and the selected item's detail.
    pub fn fill_factory(&self, view: &mut FactoryView) {
        view.last_refresh = Instant::now();
        view.error = None;
        let ws = view
            .workspace
            .as_ref()
            .map(|w| w.to_string_lossy().into_owned());
        let conn = match self.trace_db_path.as_deref() {
            None => {
                view.error =
                    Some("tracing is off: the factory keeps its items in the store".into());
                None
            }
            Some(p) if !p.is_file() => None,
            Some(p) => match crate::tracing::store::open_ro(p) {
                Ok(c) => Some(c),
                Err(e) => {
                    view.error = Some(e);
                    None
                }
            },
        };
        let items = conn
            .as_ref()
            .map(|c| store::list(c, None, ws.as_deref(), 500))
            .transpose()
            .unwrap_or_else(|e| {
                view.error = Some(e.to_string());
                None
            })
            .unwrap_or_default();
        view.set_items(items, crate::tracing::store::now_ns());
        view.brain = view
            .workspace
            .as_deref()
            .map(|w| brain::read(&self.library_root(), w))
            .unwrap_or_default();
        let runtime = self.workflows_runtime_dir();
        view.detail = match view.selected_item() {
            Some(it) => {
                let run_dir = it.run_id.as_deref().map(|r| context::run_dir(&runtime, r));
                Detail {
                    item: it.id.clone(),
                    events: conn
                        .as_ref()
                        .and_then(|c| store::events_of(c, &it.id).ok())
                        .unwrap_or_default(),
                    journal: run_dir
                        .as_deref()
                        .map(|d| journal::load(&d.join("journal.jsonl")))
                        .unwrap_or_default(),
                    result: run_dir
                        .as_deref()
                        .and_then(|d| std::fs::read_to_string(d.join("result.json")).ok())
                        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                        .and_then(|v| v.get("result").cloned())
                        .unwrap_or(Value::Null),
                    process: intake::holder(&runtime, &it.id).is_some(),
                }
            }
            None => Detail::default(),
        };
    }

    /// Back from an item's run: the Factory screen as it was, read again.
    pub(crate) fn return_to_factory(&mut self) {
        match self.factory_return.take() {
            Some(view) => {
                self.mode = Mode::Factory(view);
                self.reload_factory();
            }
            None => self.open_factory(),
        }
    }

    fn reload_factory(&mut self) {
        if let Mode::Factory(v) = &mut self.mode {
            let mut view = std::mem::replace(v, Box::new(FactoryView::new(None)));
            self.fill_factory(&mut view);
            self.mode = Mode::Factory(view);
        }
    }

    /// On the tick: the open screen follows the store.
    pub fn factory_tick(&mut self, now: Instant) {
        let due = matches!(&self.mode, Mode::Factory(v)
            if !v.text_entry_active() && now.saturating_duration_since(v.last_refresh) >= REFRESH);
        if due {
            self.reload_factory();
        }
        self.factory_poll_tick(now);
        self.factory_needs_tick(now);
    }

    /// The binary a factory process is started from.
    fn factory_exe(&self) -> Result<PathBuf, String> {
        match &self.factory_exe {
            Some(p) => Ok(p.clone()),
            None => std::env::current_exe().map_err(|e| e.to_string()),
        }
    }

    /// Starts `agent-mux factory <args>` as its own process.
    pub fn start_factory_process(
        &mut self,
        name: &str,
        args: &[String],
    ) -> Result<PathBuf, String> {
        let exe = self.factory_exe()?;
        intake::spawn_cli(
            &exe,
            &self.workflows_runtime_dir(),
            self.trace_db_path.as_deref(),
            name,
            args,
        )
    }

    pub fn handle_factory_key(&mut self, key: &KeyEvent) {
        let Mode::Factory(view) = &mut self.mode else {
            return;
        };
        let workspace = view.workspace.clone();
        match view.handle_key(key) {
            Outcome::None => {}
            Outcome::Close => self.mode = Mode::Main,
            Outcome::Reload => self.reload_factory(),
            Outcome::Notice(text) => self.notice = Some(Notice::info(text)),
            Outcome::OpenUrl(url) => self.open_url(&url),
            Outcome::Start { what, args } => {
                let name = format!("{}-{}", words(&args), crate::tracing::store::now_ns());
                self.notice = Some(match self.start_factory_process(&name, &args) {
                    Ok(log) => Notice::info(format!(
                        "started {what}; its output is in {}",
                        log.display()
                    )),
                    Err(e) => Notice::error(format!("factory: {e}")),
                });
                self.reload_factory();
            }
            Outcome::Stop(id) => {
                let done = self
                    .trace_db_path
                    .as_deref()
                    .ok_or_else(|| "tracing is off".to_string())
                    .and_then(crate::tracing::store::open_aux)
                    .and_then(|conn| {
                        let mut item = store::get(&conn, &id)
                            .map_err(|e| e.to_string())?
                            .ok_or_else(|| format!("no work item {id}"))?;
                        Ok(intake::cancel(
                            &conn,
                            &self.workflows_runtime_dir(),
                            &mut item,
                        ))
                    });
                self.notice = Some(match done {
                    Ok(true) => {
                        Notice::info(format!("{id} cancelled; its process was told to stop"))
                    }
                    Ok(false) => Notice::info(format!("{id} cancelled")),
                    Err(e) => Notice::error(format!("factory: {e}")),
                });
                self.reload_factory();
            }
            Outcome::OpenRun(run) => {
                let view = match std::mem::replace(&mut self.mode, Mode::Main) {
                    Mode::Factory(v) => v,
                    other => {
                        self.mode = other;
                        return;
                    }
                };
                if self.open_run_screen(&run, super::run_screen::Origin::Factory) {
                    self.factory_return = Some(view);
                } else {
                    self.mode = Mode::Factory(view);
                    self.notice = Some(Notice::info(
                        "the run's steps are not readable yet; the Line tab follows it",
                    ));
                }
            }
            Outcome::EditBrain => match workspace {
                Some(ws) => self.edit_factory_brain(&ws),
                None => {
                    self.notice = Some(Notice::info("a brain belongs to one repository"));
                }
            },
        }
    }

    /// Opens the repository's brain in the Markdown editor, creating the
    /// file when it has none.
    pub fn edit_factory_brain(&mut self, workspace: &Path) {
        let file = brain::path(&self.library_root(), workspace);
        if !file.is_file() {
            let made = file
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| {
                    if let Some(dir) = file.parent() {
                        let _ = std::fs::write(
                            dir.join("repo"),
                            format!("{}\n", brain::repo_name(workspace)),
                        );
                    }
                    std::fs::write(&file, brain::heading(&brain::repo_name(workspace)))
                });
            if let Err(e) = made {
                self.notice = Some(Notice::error(format!("{}: {e}", file.display())));
                return;
            }
        }
        self.open_markdown(&file);
    }

    // ---- the label poll -----------------------------------------------------

    /// With `[factory] intake = "label"`, polls the launch directory's
    /// repository every `poll_interval_s`, in a process of its own.
    fn factory_poll_tick(&mut self, now: Instant) {
        if !self.factory.intake_label {
            return;
        }
        let every = Duration::from_secs(self.factory.poll_interval_s);
        if self
            .last_factory_poll
            .is_some_and(|t| now.saturating_duration_since(t) < every)
        {
            return;
        }
        self.last_factory_poll = Some(now);
        let Some(ws) = self
            .factory_workspace_override
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .and_then(|d| d.canonicalize().ok())
            .filter(|d| !brain::github_repo(d).is_empty())
        else {
            return;
        };
        let args = vec![
            "poll".to_string(),
            "--workspace".to_string(),
            ws.to_string_lossy().into_owned(),
        ];
        if let Err(e) = self.start_factory_process("poll", &args) {
            self.notice = Some(Notice::warn(format!("factory poll: {e}")));
        }
    }

    // ---- what waits on the operator ----------------------------------------

    /// Counts the items and brain notes that wait on the operator, at
    /// most every few seconds: the status bar draws the number every frame.
    fn factory_needs_tick(&mut self, now: Instant) {
        if self
            .last_factory_needs
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(5))
        {
            return;
        }
        self.last_factory_needs = Some(now);
        self.factory_needs = self.factory_inbox_items().len();
    }

    /// Work items that wait on the operator and brain notes to decide,
    /// as inbox items.
    pub fn factory_inbox_items(&self) -> Vec<super::inbox::InboxItem> {
        use super::inbox::InboxItem;
        let Some(conn) = self
            .trace_db_path
            .as_deref()
            .filter(|p| p.is_file())
            .and_then(|p| crate::tracing::store::open_ro(p).ok())
        else {
            return Vec::new();
        };
        let mut out: Vec<InboxItem> = store::list(&conn, None, None, 500)
            .unwrap_or_default()
            .into_iter()
            .filter(|it| it.state.needs_you())
            .map(|it| InboxItem::Work(Box::new(it)))
            .collect();
        out.extend(
            store::pending_brain_notes(&conn)
                .unwrap_or_default()
                .into_iter()
                .map(InboxItem::BrainNote),
        );
        out.retain(|i| !self.inbox_dismissed.contains(&i.key()));
        out
    }

    /// Adds a suggested note to its repository's brain (`add`), or drops
    /// the suggestion.
    pub fn decide_brain_note(
        &mut self,
        note: &store::PendingNote,
        add: bool,
    ) -> Result<(), String> {
        let db = self.trace_db_path.clone().ok_or("tracing is off")?;
        if add {
            let today: String = crate::loops::format_timestamp(crate::loops::now())
                .chars()
                .take(10)
                .collect();
            brain::append(
                &self.library_root(),
                Path::new(&note.workspace),
                &note.note,
                &today,
                self.factory.brain_max_chars,
            )?;
        }
        let conn = crate::tracing::store::open_aux(&db)?;
        store::resolve_brain_note(&conn, &note.item, &note.note, add).map_err(|e| e.to_string())?;
        self.last_factory_needs = None;
        Ok(())
    }
}

/// What a row of the tree says on its right: where the item is.
pub fn badge(it: &Item, process: Option<bool>) -> String {
    match it.state {
        State::Running if process == Some(false) => "lost".into(),
        State::Running => "running".into(),
        State::NeedsAnswer => "?".into(),
        State::Unresolved => format!("✗ {}", it.cycles),
        State::Rejected => "rejected".into(),
        State::Failed => "failed".into(),
        State::Delivered => it
            .pr_number
            .map(|n| format!("PR #{n}"))
            .unwrap_or_else(|| "delivered".into()),
        State::Answered => "answered".into(),
        State::Queued => "queued".into(),
        State::Cancelled => "cancelled".into(),
    }
}

/// The item's name in the tree: `#12 title` for an issue, the title otherwise.
pub fn row_name(it: &Item) -> String {
    match it.source {
        Source::Manual => it.title.clone(),
        _ => format!("#{} {}", it.source_ref, it.title),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, state: State) -> Item {
        let mut it = Item::new(Path::new("/w"), "o/r", Source::Manual, "", title);
        it.state = state;
        it
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn view(items: Vec<Item>) -> FactoryView {
        let mut v = FactoryView::new(Some(PathBuf::from("/w")));
        v.set_items(items, crate::tracing::store::now_ns());
        v
    }

    #[test]
    fn items_are_grouped_by_what_they_wait_on_and_the_brain_is_last() {
        let v = view(vec![
            item("delivered one", State::Delivered),
            item("asks", State::NeedsAnswer),
            item("going", State::Running),
            item("stuck", State::Unresolved),
        ]);
        let labels: Vec<String> = v
            .rows
            .iter()
            .map(|r| match r {
                Row::Heading(g, n) => format!("{} {n}", g.label()),
                Row::Item(i) => v.items[*i].title.clone(),
                Row::Brain => "brain".into(),
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "running 1",
                "going",
                "needs you 2",
                "asks",
                "stuck",
                "delivered 1",
                "delivered one",
                "brain"
            ]
        );
        // the cursor never rests on a heading
        assert_eq!(v.selected_item().unwrap().title, "going");
        assert_eq!(v.needs_you(), 2);
    }

    #[test]
    fn old_closed_items_are_hidden_and_a_refresh_keeps_the_cursor() {
        let now = crate::tracing::store::now_ns();
        let mut old = item("old", State::Delivered);
        old.updated_ns = now - CLOSED_TTL_NS - 1;
        let mut stuck_old = item("stuck for weeks", State::Failed);
        stuck_old.updated_ns = now - CLOSED_TTL_NS - 1;
        let a = item("a", State::Running);
        let b = item("b", State::Running);
        let mut v = view(vec![a.clone(), b.clone(), old.clone(), stuck_old.clone()]);
        assert!(
            !v.items
                .iter()
                .enumerate()
                .any(|(i, it)| it.title == "old" && v.rows.contains(&Row::Item(i)))
        );
        // what waits on the operator is never hidden by age
        assert!(v.select(&stuck_old.id));
        assert!(v.select(&b.id));
        // b moves to another group; the cursor follows it
        let mut b2 = b.clone();
        b2.state = State::Unresolved;
        v.set_items(vec![a, b2, old, stuck_old], now);
        assert_eq!(v.selected_item().unwrap().id, b.id);
    }

    #[test]
    fn n_types_a_request_or_an_issue_number() {
        let mut v = view(vec![]);
        assert_eq!(v.handle_key(&key('n')), Outcome::None);
        assert!(v.text_entry_active());
        for c in "fix r".chars() {
            v.handle_key(&key(c));
        }
        let out = v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            out,
            Outcome::Start {
                what: "fix r".into(),
                args: vec![
                    "run".into(),
                    "fix r".into(),
                    "--workspace".into(),
                    "/w".into()
                ],
            }
        );
        assert!(!v.text_entry_active());

        v.handle_key(&key('n'));
        for c in "#12".chars() {
            v.handle_key(&key(c));
        }
        let out = v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            out,
            Outcome::Start {
                what: "issue #12".into(),
                args: vec![
                    "run".into(),
                    "--issue".into(),
                    "12".into(),
                    "--workspace".into(),
                    "/w".into()
                ],
            }
        );
    }

    #[test]
    fn verbs_follow_the_items_state() {
        let asks = item("asks", State::NeedsAnswer);
        let stuck = item("stuck", State::Unresolved);
        let going = item("going", State::Running);
        let done = item("done", State::Delivered);
        let mut v = view(vec![
            asks.clone(),
            stuck.clone(),
            going.clone(),
            done.clone(),
        ]);

        v.select(&asks.id);
        assert!(matches!(v.handle_key(&key('r')), Outcome::Notice(_)));
        assert_eq!(v.handle_key(&key('a')), Outcome::None);
        assert_eq!(v.tab, Tab::Result);
        v.handle_key(&key('c'));
        v.handle_key(&key('s'));
        v.handle_key(&key('v'));
        let out = v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            out,
            Outcome::Start {
                what: format!("the answer for {}", asks.id),
                args: vec![
                    "answer".into(),
                    asks.id.clone(),
                    "csv".into(),
                    "--workspace".into(),
                    "/w".into()
                ],
            }
        );

        v.select(&stuck.id);
        assert!(matches!(v.handle_key(&key('a')), Outcome::Notice(_)));
        assert_eq!(
            v.handle_key(&key('r')),
            Outcome::Start {
                what: stuck.id.clone(),
                args: vec!["retry".into(), stuck.id.clone()],
            }
        );

        // a running item: its process is there, so no retry; x asks first
        v.select(&going.id);
        v.detail.process = true;
        assert!(matches!(v.handle_key(&key('r')), Outcome::Notice(_)));
        assert_eq!(v.handle_key(&key('x')), Outcome::None);
        assert!(v.confirmation_active());
        assert_eq!(
            v.handle_key(&key('n')),
            Outcome::None,
            "anything but y keeps it"
        );
        assert!(!v.confirmation_active());
        v.handle_key(&key('x'));
        assert_eq!(v.handle_key(&key('y')), Outcome::Stop(going.id.clone()));
        // its process is gone: r takes it over
        v.detail.process = false;
        assert_eq!(
            v.handle_key(&key('r')),
            Outcome::Start {
                what: going.id.clone(),
                args: vec!["retry".into(), going.id.clone(), "--force".into()],
            }
        );

        v.select(&done.id);
        assert!(matches!(v.handle_key(&key('x')), Outcome::Notice(_)));
        assert!(matches!(v.handle_key(&key('o')), Outcome::Notice(_)));
    }

    #[test]
    fn the_filter_narrows_the_rows_and_the_brain_row_opens_the_brain() {
        let mut v = view(vec![
            item("export csv", State::Unresolved),
            item("login flake", State::Unresolved),
        ]);
        assert_eq!(v.handle_key(&key('/')), Outcome::None);
        for c in "csv".chars() {
            assert_eq!(v.handle_key(&key(c)), Outcome::Reload);
            let items = v.items.clone();
            v.set_items(items, crate::tracing::store::now_ns());
        }
        assert_eq!(
            v.rows.iter().filter(|r| matches!(r, Row::Item(_))).count(),
            1
        );
        v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!v.text_entry_active());
        v.cursor = v.rows.len() - 1;
        assert_eq!(v.handle_key(&key('e')), Outcome::EditBrain);
        assert_eq!(
            v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::EditBrain
        );
    }
}
