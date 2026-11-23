//! The Project screens (`Mode::Project`): a repository configured with
//! spec-wave, read from GitHub. Phase 1 of the Projects spec: Home, Board,
//! Backlog, Ready, Milestones and Health, and the item screen, with no
//! verb that writes.
//!
//! The left column holds the screens and, under them, the selected
//! screen's rows; the right pane shows the selected row. `Enter` on an
//! item opens the item screen, which is part of this view and closes back
//! to the row it came from.
//!
//! Nothing here calls GitHub on the UI thread: a read runs on a thread of
//! its own and its result is picked up on the tick. The screen opens on
//! the cached snapshot, when there is one.
//!
//! Drawing: `crate::ui::project`.

use super::{App, Mode, Notice};
use crate::factory::deliver::Tools;
use crate::project::config::{self, ProjectConfig, Role};
use crate::project::contract::*;
use crate::project::doctor::{self, Check};
use crate::project::next::{self, Decision};
use crate::project::{Item, ItemType, Snapshot, build, cli, ops, order, snapshot, stage, store};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Board,
    Backlog,
    Spec,
    Review,
    Decompose,
    Ready,
    Development,
    CodeReview,
    Bugs,
    Uat,
    Milestones,
    Health,
}

impl Screen {
    pub const ALL: [Screen; 13] = [
        Screen::Home,
        Screen::Board,
        Screen::Backlog,
        Screen::Spec,
        Screen::Review,
        Screen::Decompose,
        Screen::Ready,
        Screen::Development,
        Screen::CodeReview,
        Screen::Bugs,
        Screen::Uat,
        Screen::Milestones,
        Screen::Health,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Screen::Home => "Home",
            Screen::Board => "Board",
            Screen::Backlog => "Backlog",
            Screen::Spec => "Specification",
            Screen::Review => "Technical review",
            Screen::Decompose => "Decomposition",
            Screen::Ready => "Ready",
            Screen::Development => "Development",
            Screen::CodeReview => "Code review",
            Screen::Bugs => "Bugs",
            Screen::Uat => "UAT and release",
            Screen::Milestones => "Milestones",
            Screen::Health => "Health",
        }
    }

    fn index(self) -> usize {
        Screen::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// The pane's tabs for a row of this screen.
    pub fn tabs(self) -> &'static [&'static str] {
        match self {
            Screen::Ready => &["Order", "Card"],
            Screen::Milestones => &["Features", "Order"],
            Screen::Health => &["Check"],
            _ => &["Card"],
        }
    }
}

/// A row of a screen's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A group: `key` names it for collapsing.
    Heading {
        key: String,
        text: String,
        count: usize,
    },
    Item {
        number: u64,
        depth: u8,
    },
    /// An index into the snapshot's milestones; `None` is "no milestone".
    Milestone(Option<usize>),
    /// An index into the health checks.
    Check(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The list of screens.
    Nav,
    /// The selected screen's rows.
    List,
}

/// The item screen's tabs.
pub const ITEM_TABS: [&str; 5] = ["Card", "Documents", "Children", "Pull requests", "Thread"];

/// The item screen: one work item over the list it was opened from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemScreen {
    pub number: u64,
    pub tab: usize,
    /// The cursor of the tab's list (Documents, Children, Pull requests).
    pub cursor: usize,
    pub scroll: u16,
    /// The items opened before this one, to go back to.
    pub back: Vec<u64>,
}

/// The issue's own text and comments, read when the Thread tab opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub number: u64,
    pub lines: Result<Vec<String>, String>,
}

/// What the App does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    /// Read the Project from GitHub again.
    Sync,
    Notice(String),
    OpenUrl(String),
    /// Open a document of the hub's checkout in the Markdown viewer.
    OpenDoc(PathBuf),
    /// Read the thread of this issue.
    LoadThread(u64),
    /// Make this change on GitHub: the operator confirmed it.
    Run(Op),
}

#[derive(Debug)]
pub struct ProjectView {
    /// The folder the screen was opened on.
    pub dir: PathBuf,
    pub cfg: Option<ProjectConfig>,
    /// Why there is no Project here, when there is none.
    pub error: Option<String>,
    pub snap: Option<Snapshot>,
    /// The last read failed with this; the cached snapshot stays shown.
    pub sync_error: Option<String>,
    pub screen: Screen,
    pub focus: Focus,
    pub rows: Vec<Row>,
    /// The list cursor of each screen.
    pub cursors: [usize; Screen::ALL.len()],
    pub tab: usize,
    pub scroll: u16,
    pub filter: String,
    pub filtering: bool,
    /// Collapsed headings, by key.
    pub collapsed: BTreeSet<String>,
    pub item: Option<ItemScreen>,
    pub thread: Option<Thread>,
    pub checks: Option<Vec<Check>>,
    pub syncing_since: Option<Instant>,
    /// A dialog over the screen: a picker, a line of text, the new-item
    /// form or a confirmation.
    pub modal: Option<Modal>,
    /// A change is being made on GitHub: what it is.
    pub busy: Option<String>,
    op_rx: Option<Receiver<OpDone>>,
    sync_rx: Option<Receiver<Result<Snapshot, String>>>,
    health_rx: Option<Receiver<Vec<Check>>>,
    thread_rx: Option<Receiver<Thread>>,
}

impl ProjectView {
    pub fn new(dir: PathBuf) -> Self {
        let (cfg, error) = match config::load(&dir) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        };
        let mut collapsed = BTreeSet::new();
        collapsed.insert(format!("stage:{STAGE_DONE}"));
        ProjectView {
            dir,
            cfg,
            error,
            snap: None,
            sync_error: None,
            screen: Screen::Home,
            focus: Focus::List,
            rows: Vec::new(),
            cursors: [0; Screen::ALL.len()],
            tab: 0,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            collapsed,
            item: None,
            thread: None,
            checks: None,
            syncing_since: None,
            modal: None,
            busy: None,
            op_rx: None,
            sync_rx: None,
            health_rx: None,
            thread_rx: None,
        }
    }

    pub fn hub(&self) -> String {
        self.cfg
            .as_ref()
            .map(|c| c.hub_repo().slug())
            .unwrap_or_default()
    }

    pub fn text_entry_active(&self) -> bool {
        self.filtering || matches!(self.modal, Some(Modal::Text { .. } | Modal::New(_)))
    }

    pub fn confirmation_active(&self) -> bool {
        matches!(self.modal, Some(Modal::Confirm { .. }))
    }

    pub fn syncing(&self) -> bool {
        self.sync_rx.is_some()
    }

    pub fn cursor(&self) -> usize {
        self.cursors[self.screen.index()]
    }

    fn set_cursor(&mut self, to: usize) {
        self.cursors[self.screen.index()] = to;
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor())
    }

    /// The item the list cursor is on.
    pub fn selected_item(&self) -> Option<&Item> {
        match self.selected_row()? {
            Row::Item { number, .. } => self.snap.as_ref()?.item(*number),
            _ => None,
        }
    }

    /// The item the item screen shows.
    pub fn open_item(&self) -> Option<&Item> {
        self.snap.as_ref()?.item(self.item.as_ref()?.number)
    }

    /// The hub's checkout on this machine, when there is one: this
    /// repository, or the hub's folder beside a code repository.
    pub fn hub_root(&self) -> Option<PathBuf> {
        let cfg = self.cfg.as_ref()?;
        match cfg.role {
            Role::Hub => Some(cfg.root().to_path_buf()),
            Role::Code => {
                let beside = cfg.root().parent()?.join(&cfg.hub_repo().repo);
                beside.join(config::CONFIG_FILE).is_file().then_some(beside)
            }
        }
    }

    /// How many items wait on the operator.
    pub fn needs_you(&self) -> usize {
        self.snap.as_ref().map_or(0, |s| needs_you(s).len())
    }

    /// The count shown beside a screen in the navigation.
    pub fn screen_count(&self, screen: Screen) -> Option<usize> {
        let s = self.snap.as_ref()?;
        Some(match screen {
            Screen::Home => needs_you(s).len(),
            Screen::Board => s.items.iter().filter(|i| i.open).count(),
            Screen::Backlog => backlog_features(s).len(),
            Screen::Spec => spec_groups(s).iter().map(|(_, _, v)| v.len()).sum(),
            Screen::Review => review_groups(s).iter().map(|(_, _, v)| v.len()).sum(),
            Screen::Decompose => decompose_groups(s).iter().map(|(_, _, v)| v.len()).sum(),
            Screen::Development => development_groups(s).iter().map(|(_, _, v)| v.len()).sum(),
            Screen::CodeReview => code_review_groups(s).iter().map(|(_, _, v)| v.len()).sum(),
            Screen::Ready => s
                .items
                .iter()
                .filter(|i| {
                    i.open
                        && matches!(i.item_type, Some(ItemType::Story | ItemType::Bug))
                        && stage::shown(i).stage == STAGE_READY
                })
                .count(),
            Screen::Bugs => s
                .items
                .iter()
                .filter(|i| {
                    i.open
                        && i.item_type == Some(ItemType::Bug)
                        && stage::shown(i).stage == STAGE_TRIAGE
                })
                .count(),
            Screen::Uat => s
                .items
                .iter()
                .filter(|i| {
                    i.open && matches!(stage::shown(i).stage.as_str(), STAGE_UAT | STAGE_DEPLOY)
                })
                .count(),
            Screen::Milestones => s.milestones.iter().filter(|m| m.open).count(),
            Screen::Health => {
                return self.checks.as_ref().map(|c| {
                    c.iter()
                        .filter(|x| matches!(x.status, doctor::Status::Fail | doctor::Status::Warn))
                        .count()
                });
            }
        })
    }

    /// Builds the rows of the current screen again, keeping the cursor on
    /// its row when it is still there.
    pub fn rebuild(&mut self) {
        let keep = self.selected_row().cloned();
        self.rows = match &self.snap {
            Some(s) => rows_for(
                self.screen,
                s,
                &self.filter,
                &self.collapsed,
                self.checks.as_deref(),
            ),
            None => Vec::new(),
        };
        let at = keep
            .and_then(|k| self.rows.iter().position(|r| same_row(r, &k)))
            .unwrap_or_else(|| self.cursor().min(self.rows.len().saturating_sub(1)));
        self.set_cursor(at);
        self.tab = self.tab.min(self.screen.tabs().len().saturating_sub(1));
    }

    fn step(&mut self, by: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() as isize - 1;
        let to = (self.cursor() as isize + by).clamp(0, max) as usize;
        self.set_cursor(to);
        self.scroll = 0;
    }

    fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.tab = 0;
        self.scroll = 0;
        self.rebuild();
    }

    fn step_screen(&mut self, by: isize) {
        let n = Screen::ALL.len() as isize;
        let to = (self.screen.index() as isize + by).rem_euclid(n) as usize;
        self.set_screen(Screen::ALL[to]);
    }

    fn open_item_screen(&mut self, number: u64) {
        let mut back = self.item.take().map_or(Vec::new(), |it| {
            let mut b = it.back;
            b.push(it.number);
            b
        });
        back.retain(|n| *n != number);
        self.item = Some(ItemScreen {
            number,
            tab: 0,
            cursor: 0,
            scroll: 0,
            back,
        });
    }

    /// How many rows the item screen's current tab lists.
    fn item_tab_len(&self) -> usize {
        let (Some(it), Some(screen), Some(s)) = (self.open_item(), &self.item, &self.snap) else {
            return 0;
        };
        match screen.tab {
            1 => it.docs.len(),
            2 => s.children_of(it.number).len(),
            3 => it.pulls.len(),
            _ => 0,
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        if !self.filtering
            && !ctrl
            && let KeyCode::Char(c) = key.code
            && let Some(outcome) = self.verb_key(c)
        {
            return outcome;
        }
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) if !ctrl => self.filter.push(c),
                _ => return Outcome::None,
            }
            self.rebuild();
            return Outcome::None;
        }
        if self.item.is_some() {
            return self.item_key(key);
        }
        match key.code {
            KeyCode::Char('r') if ctrl => return Outcome::Sync,
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild();
            }
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Nav => Focus::List,
                    Focus::List => Focus::Nav,
                };
            }
            KeyCode::Char(']') => self.step_screen(1),
            KeyCode::Char('[') => self.step_screen(-1),
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Nav => self.step_screen(1),
                Focus::List => self.step(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Nav => self.step_screen(-1),
                Focus::List => self.step(-1),
            },
            KeyCode::Home | KeyCode::Char('g') if self.focus == Focus::List => {
                self.set_cursor(0);
                self.scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') if self.focus == Focus::List => {
                self.set_cursor(self.rows.len().saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab = (self.tab + 1) % self.screen.tabs().len();
                self.scroll = 0;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let n = self.screen.tabs().len();
                self.tab = (self.tab + n - 1) % n;
                self.scroll = 0;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let at = c as usize - '1' as usize;
                if at < self.screen.tabs().len() {
                    self.tab = at;
                    self.scroll = 0;
                }
            }
            KeyCode::Char('/') => {
                self.filtering = true;
                self.focus = Focus::List;
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == Focus::Nav => {
                self.focus = Focus::List;
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('i') => {
                match self.selected_row().cloned() {
                    Some(Row::Heading { key: group, .. }) if key.code != KeyCode::Char('i') => {
                        if !self.collapsed.remove(&group) {
                            self.collapsed.insert(group);
                        }
                        self.rebuild();
                    }
                    Some(Row::Item { number, .. }) => self.open_item_screen(number),
                    _ => {}
                }
            }
            KeyCode::Char('o') => {
                if let Some(it) = self.selected_item() {
                    return Outcome::OpenUrl(issue_url(&self.hub(), it.number));
                }
            }
            _ => {}
        }
        Outcome::None
    }

    fn item_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.item_tab_len();
        let hub = self.hub();
        let hub_root = self.hub_root();
        if !ctrl
            && let KeyCode::Char(c) = key.code
            && matches!(c, 'm' | 'M' | 't' | 'R' | 'D' | 'r')
        {
            return self.verb_key(c).unwrap_or(Outcome::None);
        }
        let Some(screen) = self.item.as_mut() else {
            return Outcome::None;
        };
        let lists = matches!(screen.tab, 1..=3);
        // the Thread tab is read when it opens
        fn set_tab(screen: &mut ItemScreen, to: usize) -> Option<u64> {
            screen.tab = to;
            screen.cursor = 0;
            screen.scroll = 0;
            (to == 4).then_some(screen.number)
        }
        match key.code {
            KeyCode::Char('r') if ctrl => return Outcome::Sync,
            KeyCode::Char('d') if ctrl => screen.scroll = screen.scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => screen.scroll = screen.scroll.saturating_sub(10),
            KeyCode::PageDown => screen.scroll = screen.scroll.saturating_add(10),
            KeyCode::PageUp => screen.scroll = screen.scroll.saturating_sub(10),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => match screen.back.pop() {
                Some(prev) => {
                    screen.number = prev;
                    screen.tab = 0;
                    screen.cursor = 0;
                    screen.scroll = 0;
                }
                None => self.item = None,
            },
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                let to = (screen.tab + 1) % ITEM_TABS.len();
                if let Some(n) = set_tab(screen, to) {
                    return Outcome::LoadThread(n);
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                let to = (screen.tab + ITEM_TABS.len() - 1) % ITEM_TABS.len();
                if let Some(n) = set_tab(screen, to) {
                    return Outcome::LoadThread(n);
                }
            }
            KeyCode::Char(c @ '1'..='5') => {
                if let Some(n) = set_tab(screen, c as usize - '1' as usize) {
                    return Outcome::LoadThread(n);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if lists => {
                screen.cursor = (screen.cursor + 1).min(len.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') if lists => {
                screen.cursor = screen.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => screen.scroll = screen.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => screen.scroll = screen.scroll.saturating_sub(1),
            KeyCode::Char('o') => return Outcome::OpenUrl(issue_url(&hub, screen.number)),
            KeyCode::Enter => {
                let (tab, cursor, number) = (screen.tab, screen.cursor, screen.number);
                let Some(snap) = &self.snap else {
                    return Outcome::None;
                };
                let Some(it) = snap.item(number) else {
                    return Outcome::None;
                };
                match tab {
                    1 => {
                        let Some(doc) = it.docs.values().nth(cursor) else {
                            return Outcome::None;
                        };
                        if let Some(pr) = doc.pr {
                            return Outcome::OpenUrl(format!("https://github.com/{hub}/pull/{pr}"));
                        }
                        return match (doc.state, hub_root) {
                            (crate::project::DocState::Base, Some(root))
                                if root.join(&doc.path).is_file() =>
                            {
                                Outcome::OpenDoc(root.join(&doc.path))
                            }
                            (crate::project::DocState::Base, Some(_)) => Outcome::Notice(format!(
                                "{} is on the default branch but not in the local checkout: pull it",
                                doc.path
                            )),
                            (crate::project::DocState::Base, None) => Outcome::OpenUrl(format!(
                                "https://github.com/{hub}/blob/HEAD/{}",
                                doc.path
                            )),
                            (state, _) => Outcome::Notice(format!(
                                "{} is {}: nothing to open",
                                doc.path,
                                state.as_str()
                            )),
                        };
                    }
                    2 => {
                        if let Some(child) = snap.children_of(number).get(cursor) {
                            let n = child.number;
                            self.open_item_screen(n);
                        }
                    }
                    3 => {
                        if let Some(p) = it.pulls.get(cursor) {
                            return Outcome::OpenUrl(p.url.clone());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Outcome::None
    }
}

/// A change on GitHub, as the operator asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    New(ops::NewItem),
    Move {
        number: u64,
        stage: String,
    },
    Repair {
        number: u64,
        stage: String,
        reason: String,
    },
    Triage {
        action: &'static str,
        number: u64,
        severity: Option<String>,
        reason: Option<String>,
        of: Option<u64>,
    },
    /// The board moves the pull requests already justify.
    Advance(Vec<ops::AutoMove>),
    /// Run the item's next document step, as `agent-mux project run`.
    RunStep {
        number: u64,
        /// Create the issues of the reviewed draft: the operator's approval.
        apply: bool,
    },
    /// Another `agent-mux project …` command for an item, as a process of
    /// its own: a fix-pr step, a merge.
    Process {
        number: u64,
        /// The command line after `project`, without `--dir`.
        args: Vec<String>,
        /// A few words: "the fix-pr step", "the merge".
        what: String,
    },
}

impl Op {
    /// A few words for the status line while it runs.
    pub fn label(&self) -> String {
        match self {
            Op::New(n) => format!(
                "creating the {}",
                n.item_type.map_or("item", ItemType::as_str)
            ),
            Op::Move { number, .. } => format!("moving #{number}"),
            Op::Repair { number, .. } => format!("repairing the stage of #{number}"),
            Op::Triage { number, .. } => format!("triaging #{number}"),
            Op::Advance(m) => format!("making {} board move(s)", m.len()),
            Op::RunStep { number, .. } => format!("starting the next step of #{number}"),
            Op::Process { number, what, .. } => format!("starting {what} of #{number}"),
        }
    }
}

/// What a picker's choice is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    MoveTo(u64),
    RepairTo(u64),
    /// The severity a Bug is accepted with; the first option keeps its own.
    Severity(u64),
}

/// What a typed line is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFor {
    RepairReason { number: u64, stage: String },
    RejectReason(u64),
    DuplicateOf(u64),
}

/// The new-item form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewForm {
    /// An index into [`ItemType::ALL`].
    pub item_type: usize,
    pub title: String,
    /// The parent's number, as typed.
    pub parent: String,
    /// 0 is none; then an index into the priorities, plus one.
    pub priority: usize,
    /// The field with the keyboard: type, title, parent, priority.
    pub field: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Pick {
        what: Pick,
        title: String,
        options: Vec<String>,
        cursor: usize,
    },
    Text {
        what: TextFor,
        label: String,
        text: String,
    },
    New(NewForm),
    /// What will be written, and the operator's yes.
    Confirm {
        lines: Vec<String>,
        op: Op,
    },
}

/// What a finished change left: its report, and the snapshot with the
/// changed issues read again.
pub struct OpDone {
    pub result: Result<ops::Report, String>,
    pub snap: Option<Snapshot>,
}

impl std::fmt::Debug for OpDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpDone")
            .field("result", &self.result)
            .finish()
    }
}

/// The type a new item under `parent` most likely is.
fn child_type(parent: Option<ItemType>) -> ItemType {
    match parent {
        Some(ItemType::Initiative) => ItemType::Epic,
        Some(ItemType::Epic) => ItemType::Feature,
        Some(ItemType::Feature) => ItemType::Story,
        Some(ItemType::Story) => ItemType::Task,
        _ => ItemType::Feature,
    }
}

impl ProjectView {
    /// The item a verb acts on: the item screen's, else the row's.
    pub fn target(&self) -> Option<&Item> {
        if self.item.is_some() {
            self.open_item()
        } else {
            self.selected_item()
        }
    }

    /// A verb's key. `None`: the key is not a verb here.
    fn verb_key(&mut self, c: char) -> Option<Outcome> {
        if !matches!(c, 'n' | 'm' | 'M' | 't' | 'R' | 'D' | 'A' | 'r' | 'f' | 'P') {
            return None;
        }
        if self.snap.is_none() {
            return Some(Outcome::Notice("the Project is not read yet".into()));
        }
        if let Some(what) = &self.busy {
            return Some(Outcome::Notice(format!("wait: {what}")));
        }
        let target = self.target().cloned();
        let need = |what: &str| Some(Outcome::Notice(format!("select an item to {what}")));
        match c {
            'f' => {
                let Some(it) = target else {
                    return need("fix its pull request");
                };
                let open: Vec<&crate::project::Pull> =
                    it.pulls.iter().filter(|p| p.open()).collect();
                let [pull] = open.as_slice() else {
                    return Some(Outcome::Notice(format!(
                        "#{}: fix-pr needs one open pull request, and it has {}",
                        it.number,
                        open.len()
                    )));
                };
                if !matches!(it.item_type, Some(ItemType::Story | ItemType::Bug)) {
                    return Some(Outcome::Notice(format!(
                        "#{}: fix-pr works on the pull request of a Story or a Bug",
                        it.number
                    )));
                }
                self.modal = Some(Modal::Confirm {
                    lines: vec![
                        format!(
                            "Fix what the review of pull request #{} asked for (#{})",
                            pull.number, it.number
                        ),
                        "It reads the review threads that are not resolved. With none, it stops and writes nothing.".into(),
                        format!("A session on Claude Code commits one fix per comment in a worktree on `{}`.", pull.head),
                        "A second session reviews the fixes; the work goes back to the first at most twice.".into(),
                        "If the review approves, agent-mux pushes to that branch, answers each comment with its commit and leaves a summary.".into(),
                    ],
                    op: Op::Process {
                        number: it.number,
                        args: vec![
                            "run".into(),
                            it.number.to_string(),
                            "--step".into(),
                            "fix-pr".into(),
                        ],
                        what: "the fix-pr step".into(),
                    },
                });
            }
            'P' => {
                let Some(it) = target else {
                    return need("merge");
                };
                let Some(snap) = &self.snap else {
                    return Some(Outcome::None);
                };
                // the plan as the cache has it; the command reads GitHub
                // again before it merges anything
                let sorted = match crate::project::merge::scope(snap, it.number) {
                    Ok(s) => s,
                    Err(e) => return Some(Outcome::Notice(e)),
                };
                let plan = crate::project::merge::plan(&sorted, snap, &Default::default());
                if let Some(block) = plan.blocks.first() {
                    return Some(Outcome::Notice(format!("#{}: {block}", it.number)));
                }
                if plan.queue.is_empty() {
                    return Some(Outcome::Notice(format!(
                        "#{}: {}",
                        it.number,
                        if plan.done.is_empty() {
                            "no open pull request to merge"
                        } else {
                            "everything is merged already"
                        }
                    )));
                }
                let mut lines = vec![format!("Merge #{}", it.number)];
                lines.extend(crate::project::merge::plan_lines(&plan, &snap.hub));
                lines.push("Each one is merged with a merge commit and its item moves to QA; the branches are deleted at the end.".into());
                lines.push("GitHub is read again first: a draft, or a merge GitHub refuses, stops the queue there.".into());
                self.modal = Some(Modal::Confirm {
                    lines,
                    op: Op::Process {
                        number: it.number,
                        args: vec!["merge".into(), it.number.to_string(), "--yes".into()],
                        what: "the merge".into(),
                    },
                });
            }
            'r' => {
                let Some(it) = target else {
                    return need("run its next step");
                };
                let Some(snap) = &self.snap else {
                    return Some(Outcome::None);
                };
                // development: a Story, or a Bug past its triage
                if let Some(d) = build::decision(&it, snap, false) {
                    let line = cli::decision_lines(&d).join(" · ");
                    if !d.runnable() {
                        return Some(Outcome::Notice(format!("#{}: {line}", it.number)));
                    }
                    let bug = it.item_type == Some(ItemType::Bug);
                    self.modal = Some(Modal::Confirm {
                        lines: vec![
                            format!(
                                "{} #{}: {line}",
                                if bug { "Fix Bug" } else { "Build Story" },
                                it.number
                            ),
                            "A session on Claude Code writes the code in a worktree of the code repository, forked from its default branch, and commits it.".into(),
                            "A second session reviews the committed diff; the work goes back to the first at most twice.".into(),
                            "If the review approves, agent-mux pushes the branch, opens a draft pull request and moves the board to Code Review.".into(),
                            "The board moves to Desenvolvimento as soon as it starts. It takes minutes; nothing is merged.".into(),
                        ],
                        op: Op::RunStep {
                            number: it.number,
                            apply: false,
                        },
                    });
                    return Some(Outcome::None);
                }
                let d = next::next_step(&it);
                let line = cli::decision_lines(&d).join(" · ");
                use next::Action::*;
                let runs = matches!(
                    d.action,
                    GenerateSpec
                        | GeneratePlan
                        | GenerateBug
                        | Validate
                        | Decompose
                        | DecomposeApply
                ) && d.blocked.is_none();
                if !runs {
                    let said = cli::decision_lines(&next::describe(&it, snap)).join(" · ");
                    return Some(Outcome::Notice(format!("#{}: {said}", it.number)));
                }
                let mut lines = vec![format!("Run the next step of #{}: {line}", it.number)];
                if d.action == Validate {
                    lines.push("It checks the merged documents and, if they pass, adds the approval label.".into());
                    lines.push("It comments on the issue either way.".into());
                } else if d.action == DecomposeApply {
                    lines.push("It creates the Story and Task issues of the reviewed draft, in Ready, linked and labelled.".into());
                    lines.push(
                        "Applying is your approval of the draft: it cannot be applied twice."
                            .into(),
                    );
                } else if d.action == Decompose {
                    lines.push("A read-only session on Claude Code drafts the Stories and Tasks, or the merged draft is audited as it is.".into());
                    lines.push("agent-mux publishes the draft as decomposition.md on a pull request. No issue is created.".into());
                    lines.push("A second session audits it; a grave finding adds `spec-wave:critique-failed`.".into());
                } else {
                    lines.push("A read-only session on Claude Code writes the document.".into());
                    lines.push(
                        "agent-mux then publishes it on a pull request and comments on the issue."
                            .into(),
                    );
                    if d.action != GenerateSpec {
                        lines.push("A second session audits it; a grave finding adds `spec-wave:critique-failed`.".into());
                    }
                }
                self.modal = Some(Modal::Confirm {
                    lines,
                    op: Op::RunStep {
                        number: it.number,
                        apply: d.action == DecomposeApply,
                    },
                });
            }
            'n' => {
                let parent = target.as_ref().filter(|t| {
                    matches!(
                        t.item_type,
                        Some(
                            ItemType::Initiative
                                | ItemType::Epic
                                | ItemType::Feature
                                | ItemType::Story
                        )
                    )
                });
                let t = child_type(parent.and_then(|p| p.item_type));
                self.modal = Some(Modal::New(NewForm {
                    item_type: ItemType::ALL.iter().position(|x| *x == t).unwrap_or(2),
                    title: String::new(),
                    parent: parent.map(|p| p.number.to_string()).unwrap_or_default(),
                    priority: 0,
                    field: 1,
                }));
            }
            'm' | 'M' => {
                let Some(it) = target else {
                    return need("move");
                };
                if it.item_type == Some(ItemType::Spike) {
                    return Some(Outcome::Notice(
                        "a Spike's stage is moved by hand on the board".into(),
                    ));
                }
                let repair = c == 'M';
                let options: Vec<String> = STAGE_ORDER
                    .iter()
                    .filter(|s| repair || stage::plan_move(it.stage.as_deref(), s).advance)
                    .filter(|s| it.stage.as_deref() != Some(**s))
                    .map(|s| s.to_string())
                    .collect();
                if options.is_empty() {
                    let why = match it.stage.as_deref().map(stage::kind) {
                        Some(stage::Kind::Canonical) => "it is at the last stage",
                        _ => "its column is not a stage of the flow: [M] repairs it",
                    };
                    return Some(Outcome::Notice(format!(
                        "#{} cannot advance: {why}",
                        it.number
                    )));
                }
                self.modal = Some(Modal::Pick {
                    title: if repair {
                        format!(
                            "Repair the stage of #{} (may go back; leaves a note)",
                            it.number
                        )
                    } else {
                        format!("Move #{} to", it.number)
                    },
                    what: if repair {
                        Pick::RepairTo(it.number)
                    } else {
                        Pick::MoveTo(it.number)
                    },
                    options,
                    cursor: 0,
                });
            }
            't' | 'R' | 'D' => {
                let Some(it) = target.filter(|t| t.item_type == Some(ItemType::Bug)) else {
                    return Some(Outcome::Notice("triage is for a Bug: select one".into()));
                };
                self.modal = Some(match c {
                    't' => {
                        let mut options = vec![match &it.priority {
                            Some(p) => format!("keep {p}"),
                            None => "no severity".to_string(),
                        }];
                        options.extend(PRIORITIES.iter().map(|p| p.to_string()));
                        Modal::Pick {
                            what: Pick::Severity(it.number),
                            title: format!("Accept Bug #{} with severity", it.number),
                            options,
                            cursor: 0,
                        }
                    }
                    'R' => Modal::Text {
                        what: TextFor::RejectReason(it.number),
                        label: format!("Reject Bug #{}: why", it.number),
                        text: String::new(),
                    },
                    _ => Modal::Text {
                        what: TextFor::DuplicateOf(it.number),
                        label: format!("Bug #{} is a duplicate of issue", it.number),
                        text: String::new(),
                    },
                });
            }
            _ => {
                let moves = self.snap.as_ref().map(ops::auto_moves).unwrap_or_default();
                if moves.is_empty() {
                    return Some(Outcome::Notice(
                        "the board is up to date with the pull requests".into(),
                    ));
                }
                let mut lines = vec!["The pull requests already justify these moves:".to_string()];
                lines.extend(
                    moves
                        .iter()
                        .map(|m| format!("#{}: {} → {} ({})", m.number, m.from, m.to, m.why)),
                );
                self.modal = Some(Modal::Confirm {
                    lines,
                    op: Op::Advance(moves),
                });
            }
        }
        Some(Outcome::None)
    }

    fn confirm(&mut self, lines: Vec<String>, op: Op) {
        self.modal = Some(Modal::Confirm { lines, op });
    }

    fn modal_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(modal) = self.modal.take() else {
            return Outcome::None;
        };
        if key.code == KeyCode::Esc {
            return Outcome::None;
        }
        match modal {
            Modal::Confirm { lines, op } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    return Outcome::Run(op);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {}
                _ => self.modal = Some(Modal::Confirm { lines, op }),
            },
            Modal::Pick {
                what,
                title,
                options,
                mut cursor,
            } => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(options.len().saturating_sub(1));
                    self.modal = Some(Modal::Pick {
                        what,
                        title,
                        options,
                        cursor,
                    });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    self.modal = Some(Modal::Pick {
                        what,
                        title,
                        options,
                        cursor,
                    });
                }
                KeyCode::Enter => {
                    let choice = options.get(cursor).cloned().unwrap_or_default();
                    match what {
                        Pick::MoveTo(number) => self.confirm(
                            vec![
                                format!("Move #{number} to {choice}, with status Todo."),
                                "This writes the board's Etapa and Status fields.".into(),
                            ],
                            Op::Move {
                                number,
                                stage: choice,
                            },
                        ),
                        Pick::RepairTo(number) => {
                            self.modal = Some(Modal::Text {
                                what: TextFor::RepairReason {
                                    number,
                                    stage: choice.clone(),
                                },
                                label: format!("Repair #{number} to {choice}: why"),
                                text: String::new(),
                            });
                        }
                        Pick::Severity(number) => {
                            let severity = (cursor > 0).then(|| choice.clone());
                            let mut lines =
                                vec![format!("Accept Bug #{number} for the technical queue.")];
                            if let Some(s) = &severity {
                                lines.push(format!("Its severity label becomes {s}."));
                            }
                            lines.push(
                                "This adds `spec-wave:triaged`, comments on the issue and moves it to Ready.".into(),
                            );
                            self.confirm(
                                lines,
                                Op::Triage {
                                    action: "accept",
                                    number,
                                    severity,
                                    reason: None,
                                    of: None,
                                },
                            );
                        }
                    }
                }
                _ => {
                    self.modal = Some(Modal::Pick {
                        what,
                        title,
                        options,
                        cursor,
                    })
                }
            },
            Modal::Text {
                what,
                label,
                mut text,
            } => {
                match key.code {
                    KeyCode::Backspace => {
                        text.pop();
                        self.modal = Some(Modal::Text { what, label, text });
                    }
                    KeyCode::Char(c) if !ctrl => {
                        text.push(c);
                        self.modal = Some(Modal::Text { what, label, text });
                    }
                    KeyCode::Enter => {
                        let typed = text.trim().to_string();
                        if typed.is_empty() {
                            self.modal = Some(Modal::Text { what, label, text });
                            return Outcome::Notice("it cannot be empty".into());
                        }
                        match what {
                        TextFor::RepairReason { number, stage } => self.confirm(
                            vec![
                                format!("Repair #{number}: put it at {stage}, whatever the order says."),
                                format!("Reason, left as a comment on the issue: {typed}"),
                            ],
                            Op::Repair { number, stage, reason: typed },
                        ),
                        TextFor::RejectReason(number) => self.confirm(
                            vec![
                                format!("Reject Bug #{number} and close it as not planned."),
                                format!("Reason, left as a comment on the issue: {typed}"),
                                "This adds `spec-wave:wont-fix` and `spec-wave:triaged`.".into(),
                            ],
                            Op::Triage {
                                action: "reject",
                                number,
                                severity: None,
                                reason: Some(typed),
                                of: None,
                            },
                        ),
                        TextFor::DuplicateOf(number) => {
                            match typed.trim_start_matches('#').parse::<u64>() {
                                Ok(original) if original != number => self.confirm(
                                    vec![
                                        format!("Close Bug #{number} as a duplicate of #{original}."),
                                        "This comments on both issues and adds `spec-wave:duplicate` and `spec-wave:triaged`.".into(),
                                    ],
                                    Op::Triage {
                                        action: "duplicate",
                                        number,
                                        severity: None,
                                        reason: None,
                                        of: Some(original),
                                    },
                                ),
                                _ => {
                                    self.modal = Some(Modal::Text { what: TextFor::DuplicateOf(number), label, text });
                                    return Outcome::Notice(
                                        "type the number of the original issue".into(),
                                    );
                                }
                            }
                        }
                    }
                    }
                    _ => self.modal = Some(Modal::Text { what, label, text }),
                }
            }
            Modal::New(mut form) => {
                match key.code {
                    KeyCode::Tab | KeyCode::Down => form.field = (form.field + 1) % 4,
                    KeyCode::BackTab | KeyCode::Up => form.field = (form.field + 3) % 4,
                    KeyCode::Left | KeyCode::Right if form.field == 0 => {
                        let n = ItemType::ALL.len();
                        form.item_type = if key.code == KeyCode::Right {
                            (form.item_type + 1) % n
                        } else {
                            (form.item_type + n - 1) % n
                        };
                    }
                    KeyCode::Left | KeyCode::Right if form.field == 3 => {
                        let n = PRIORITIES.len() + 1;
                        form.priority = if key.code == KeyCode::Right {
                            (form.priority + 1) % n
                        } else {
                            (form.priority + n - 1) % n
                        };
                    }
                    KeyCode::Backspace if form.field == 1 => {
                        form.title.pop();
                    }
                    KeyCode::Backspace if form.field == 2 => {
                        form.parent.pop();
                    }
                    KeyCode::Char(c) if !ctrl && form.field == 1 => form.title.push(c),
                    KeyCode::Char(c) if !ctrl && form.field == 2 && c.is_ascii_digit() => {
                        form.parent.push(c);
                    }
                    KeyCode::Enter => {
                        let t = ItemType::ALL[form.item_type.min(ItemType::ALL.len() - 1)];
                        let title = form.title.trim().to_string();
                        if title.is_empty() {
                            form.field = 1;
                            self.modal = Some(Modal::New(form));
                            return Outcome::Notice("a title is needed".into());
                        }
                        let parent = form.parent.trim().parse::<u64>().ok();
                        let priority = form
                            .priority
                            .checked_sub(1)
                            .and_then(|i| PRIORITIES.get(i))
                            .map(|p| p.to_string());
                        let stage = initial_stage(t, priority.as_deref());
                        let mut lines = vec![format!("Create {} «{title}»", t.as_str())];
                        if let Some(p) = parent {
                            lines.push(format!("as a sub-issue of #{p}"));
                        }
                        lines.push(format!("and put it on the board in {stage}."));
                        self.confirm(
                            lines,
                            Op::New(ops::NewItem {
                                item_type: Some(t),
                                title,
                                parent,
                                priority,
                                area: None,
                                body: String::new(),
                            }),
                        );
                        return Outcome::None;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::New(form));
            }
        }
        Outcome::None
    }
}

/// A work item that waits on the operator, as the inbox lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub hub: String,
    /// The folder the Project was read from.
    pub root: String,
    pub number: u64,
    pub title: String,
    /// `Feature`, `Bug`, `RFC`.
    pub kind: String,
    pub stage: String,
    /// What it waits on: a block's code, or the step a person gives.
    pub code: String,
    pub reason: String,
    pub unblock: String,
}

impl Need {
    /// The few words that say what the item wants.
    pub fn word(&self) -> &'static str {
        match self.code.as_str() {
            "pr-pending" => "document to merge",
            "critique-failed" => "critique failed",
            "needs-human" => "needs a person",
            "inconsistent-state" => "labels and files disagree",
            "validate" => "plan to validate",
            "decompose-apply" => "draft to apply",
            "triage" => "bug to triage",
            _ => "waits on you",
        }
    }
}

/// The needs of one snapshot.
pub fn needs_of(s: &Snapshot) -> Vec<Need> {
    needs_you(s)
        .into_iter()
        .map(|(it, d)| Need {
            hub: s.hub.clone(),
            root: s.root.clone().unwrap_or_default(),
            number: it.number,
            title: it.name().to_string(),
            kind: it.item_type.map_or("item", ItemType::as_str).to_string(),
            stage: stage::shown(it).stage,
            code: match &d.blocked {
                Some(b) if b.code != "manual" => b.code.to_string(),
                _ => d.action.as_str().to_string(),
            },
            reason: d.reason.clone(),
            unblock: d
                .blocked
                .as_ref()
                .map(|b| b.unblock.clone())
                .unwrap_or_default(),
        })
        .collect()
}

fn same_row(a: &Row, b: &Row) -> bool {
    match (a, b) {
        (Row::Heading { key: x, .. }, Row::Heading { key: y, .. }) => x == y,
        (Row::Item { number: x, .. }, Row::Item { number: y, .. }) => x == y,
        _ => a == b,
    }
}

pub fn issue_url(hub: &str, number: u64) -> String {
    format!("https://github.com/{hub}/issues/{number}")
}

/// A type that is worked on: it has a pull request at the end.
fn is_work(i: &Item) -> bool {
    matches!(
        i.item_type,
        Some(ItemType::Story | ItemType::Task | ItemType::Bug)
    )
}

/// Open items that wait on the operator, with what they wait on.
pub fn needs_you(s: &Snapshot) -> Vec<(&Item, Decision)> {
    s.items
        .iter()
        .filter(|i| {
            i.open
                && matches!(
                    i.item_type,
                    Some(ItemType::Feature | ItemType::Bug | ItemType::Rfc)
                )
        })
        .map(|i| (i, next::describe(i, s)))
        .filter(|(_, d)| d.needs_operator() && !matches!(d.action, next::Action::Implement))
        .collect()
}

/// Open Features, RFCs and Spikes still before Spec.
fn backlog_features(s: &Snapshot) -> Vec<&Item> {
    s.items
        .iter()
        .filter(|i| {
            i.open
                && matches!(
                    i.item_type,
                    Some(ItemType::Feature | ItemType::Rfc | ItemType::Spike)
                )
                && matches!(
                    stage::shown(i).stage.as_str(),
                    STAGE_BACKLOG | STAGE_PRIORITIZED
                )
        })
        .collect()
}

fn matches_filter(i: &Item, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    let f = f.trim_start_matches('#');
    i.title.to_lowercase().contains(f) || i.number.to_string().contains(f)
}

fn by_priority(a: &&Item, b: &&Item) -> std::cmp::Ordering {
    let p = |i: &Item| i.priority.clone().unwrap_or_else(|| "P9".to_string());
    p(a).cmp(&p(b)).then(a.number.cmp(&b.number))
}

/// The rows of a screen. Pure over the snapshot.
pub fn rows_for(
    screen: Screen,
    s: &Snapshot,
    filter: &str,
    collapsed: &BTreeSet<String>,
    checks: Option<&[Check]>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let group = |rows: &mut Vec<Row>, key: String, text: String, items: Vec<&Item>, depth: u8| {
        let items: Vec<&Item> = items
            .into_iter()
            .filter(|i| matches_filter(i, filter))
            .collect();
        if items.is_empty() && !filter.is_empty() {
            return;
        }
        let closed = collapsed.contains(&key);
        rows.push(Row::Heading {
            key,
            text,
            count: items.len(),
        });
        if !closed {
            rows.extend(items.iter().map(|i| Row::Item {
                number: i.number,
                depth,
            }));
        }
    };
    match screen {
        Screen::Home => {
            let needs: Vec<&Item> = needs_you(s).into_iter().map(|(i, _)| i).collect();
            group(&mut rows, "home:needs".into(), "needs you".into(), needs, 0);
            let can_run: Vec<&Item> = s
                .items
                .iter()
                .filter(|i| i.open && next::describe(i, s).runnable())
                .collect();
            group(
                &mut rows,
                "home:next".into(),
                "a step can run".into(),
                can_run,
                0,
            );
            for (key, text, st) in [
                ("home:dev", "in development", STAGE_DEVELOPMENT),
                ("home:review", "in code review", STAGE_CODE_REVIEW),
                ("home:qa", "in QA", STAGE_QA),
            ] {
                let mut items: Vec<&Item> = s
                    .items
                    .iter()
                    .filter(|i| {
                        i.open
                            && stage::shown(i).stage == st
                            && matches!(
                                i.item_type,
                                Some(ItemType::Feature | ItemType::Story | ItemType::Bug)
                            )
                    })
                    .collect();
                items.sort_by(by_priority);
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Board => {
            let mut stages: Vec<String> = if s.board_stages.is_empty() {
                STAGE_ORDER.iter().map(|x| x.to_string()).collect()
            } else {
                s.board_stages.clone()
            };
            // a column made by hand, or a retired one, after the flow's own
            stages.sort_by_key(|st| stage::kind(st) != stage::Kind::Canonical);
            for st in stages {
                let mut items: Vec<&Item> = s
                    .items
                    .iter()
                    .filter(|i| i.item_type.is_some() && stage::shown(i).stage == st)
                    .filter(|i| i.open || st == STAGE_DONE)
                    .collect();
                if items.is_empty() && stage::kind(&st) != stage::Kind::Canonical {
                    continue;
                }
                items.sort_by(by_priority);
                let mark = match stage::kind(&st) {
                    stage::Kind::Canonical => "",
                    stage::Kind::Retired => "  (retired)",
                    stage::Kind::Unknown => "  (not in the flow)",
                };
                group(
                    &mut rows,
                    format!("stage:{st}"),
                    format!("{st}{mark}"),
                    items,
                    0,
                );
            }
        }
        Screen::Backlog => {
            // Initiative → Epic → Feature, for what is still before Spec
            let features = backlog_features(s);
            let mut shown: BTreeSet<u64> = features.iter().map(|i| i.number).collect();
            for f in &features {
                let mut cur = f.parent;
                for _ in 0..8 {
                    let Some(p) = cur.and_then(|n| s.item(n)) else {
                        break;
                    };
                    shown.insert(p.number);
                    cur = p.parent;
                }
            }
            // a grouping item with nothing under it yet is backlog too
            for i in &s.items {
                if i.open
                    && matches!(i.item_type, Some(ItemType::Initiative | ItemType::Epic))
                    && s.children_of(i.number).is_empty()
                {
                    shown.insert(i.number);
                }
            }
            fn walk(
                s: &Snapshot,
                n: u64,
                depth: u8,
                shown: &BTreeSet<u64>,
                filter: &str,
                out: &mut Vec<Row>,
            ) {
                let Some(it) = s.item(n) else { return };
                let kids: Vec<u64> = s
                    .children_of(n)
                    .iter()
                    .filter(|k| shown.contains(&k.number))
                    .map(|k| k.number)
                    .collect();
                let mut below = Vec::new();
                for k in kids {
                    walk(s, k, depth + 1, shown, filter, &mut below);
                }
                if matches_filter(it, filter) || !below.is_empty() {
                    out.push(Row::Item { number: n, depth });
                    out.extend(below);
                }
            }
            let roots: Vec<u64> = s
                .items
                .iter()
                .filter(|i| {
                    shown.contains(&i.number) && i.parent.is_none_or(|p| !shown.contains(&p))
                })
                .map(|i| i.number)
                .collect();
            for r in roots {
                walk(s, r, 0, &shown, filter, &mut rows);
            }
        }
        Screen::Ready => {
            // Stories in Ready under their Feature, in execution order
            let mut features: Vec<&Item> = s
                .items
                .iter()
                .filter(|i| {
                    i.open && matches!(i.item_type, Some(ItemType::Feature | ItemType::Rfc))
                })
                .collect();
            features.sort_by(|a, b| {
                a.milestone
                    .is_none()
                    .cmp(&b.milestone.is_none())
                    .then(a.milestone.cmp(&b.milestone))
                    .then(a.number.cmp(&b.number))
            });
            let mut placed: BTreeSet<u64> = BTreeSet::new();
            for f in features {
                let Ok(o) = order::feature_order(s, f.number) else {
                    continue;
                };
                let mut numbers: Vec<u64> = o.rows.iter().map(|r| r.number).collect();
                numbers.extend(o.cycle.iter().copied());
                let items: Vec<&Item> = numbers
                    .iter()
                    .filter_map(|n| s.item(*n))
                    .filter(|i| i.open && stage::shown(i).stage == STAGE_READY)
                    .collect();
                if items.is_empty() {
                    continue;
                }
                placed.extend(items.iter().map(|i| i.number));
                let text = format!(
                    "#{} {}{}",
                    f.number,
                    f.name(),
                    f.milestone
                        .as_deref()
                        .map(|m| format!("  [{m}]"))
                        .unwrap_or_default()
                );
                group(&mut rows, format!("feature:{}", f.number), text, items, 1);
            }
            let mut rest: Vec<&Item> = s
                .items
                .iter()
                .filter(|i| {
                    i.open
                        && is_work(i)
                        && stage::shown(i).stage == STAGE_READY
                        && !placed.contains(&i.number)
                        && i.item_type != Some(ItemType::Task)
                })
                .collect();
            rest.sort_by(by_priority);
            if !rest.is_empty() {
                group(
                    &mut rows,
                    "ready:other".into(),
                    "bugs, and stories with no open Feature".into(),
                    rest,
                    1,
                );
            }
        }
        Screen::Milestones => {
            let mut order: Vec<usize> = (0..s.milestones.len())
                .filter(|i| s.milestones[*i].open)
                .collect();
            order.sort_by(|a, b| {
                let (x, y) = (&s.milestones[*a], &s.milestones[*b]);
                x.due
                    .is_none()
                    .cmp(&y.due.is_none())
                    .then(x.due.cmp(&y.due))
            });
            rows.extend(order.into_iter().map(|i| Row::Milestone(Some(i))));
            if s.items
                .iter()
                .any(|i| i.open && i.item_type == Some(ItemType::Feature) && i.milestone.is_none())
            {
                rows.push(Row::Milestone(None));
            }
        }
        Screen::Spec => {
            for (key, text, items) in spec_groups(s) {
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Review => {
            for (key, text, items) in review_groups(s) {
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Decompose => {
            for (key, text, items) in decompose_groups(s) {
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Development => {
            for (key, text, items) in development_groups(s) {
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::CodeReview => {
            for (key, text, items) in code_review_groups(s) {
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Bugs => {
            // triage first, then the flow's order
            let stages: Vec<&str> = track(ItemType::Bug).unwrap_or_default();
            for st in stages {
                if st == STAGE_DONE {
                    continue;
                }
                let mut items: Vec<&Item> = s
                    .items
                    .iter()
                    .filter(|i| {
                        i.open && i.item_type == Some(ItemType::Bug) && stage::shown(i).stage == st
                    })
                    .collect();
                items.sort_by(by_priority);
                let text = if st == STAGE_TRIAGE {
                    "to triage".to_string()
                } else {
                    stage_plain(st).to_string()
                };
                if !items.is_empty() || st == STAGE_TRIAGE {
                    group(&mut rows, format!("bugs:{st}"), text, items, 0);
                }
            }
        }
        Screen::Uat => {
            for (key, text, st) in [
                ("uat:uat", "in acceptance (Homologação)", STAGE_UAT),
                ("uat:deploy", "waiting for deploy", STAGE_DEPLOY),
            ] {
                let mut items: Vec<&Item> = s
                    .items
                    .iter()
                    .filter(|i| i.open && i.item_type.is_some() && stage::shown(i).stage == st)
                    .collect();
                items.sort_by(|a, b| {
                    a.milestone
                        .is_none()
                        .cmp(&b.milestone.is_none())
                        .then(a.milestone.cmp(&b.milestone))
                        .then(a.number.cmp(&b.number))
                });
                group(&mut rows, key.into(), text.into(), items, 0);
            }
        }
        Screen::Health => {
            if let Some(c) = checks {
                rows.extend((0..c.len()).map(Row::Check));
            }
        }
    }
    rows
}

type Groups<'a> = Vec<(&'static str, &'static str, Vec<&'a Item>)>;

fn open_features(s: &Snapshot) -> Vec<&Item> {
    let mut out: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| i.open && i.item_type == Some(ItemType::Feature))
        .collect();
    out.sort_by(by_priority);
    out
}

/// Specification: Features prioritized or in Spec, by where their
/// spec.md is.
pub fn spec_groups(s: &Snapshot) -> Groups<'_> {
    use crate::project::DocState::*;
    let at = |i: &Item| {
        matches!(
            stage::shown(i).stage.as_str(),
            STAGE_PRIORITIZED | STAGE_SPEC
        ) && i.doc_state("plan") == Missing
    };
    let of = |want: fn(crate::project::DocState) -> bool| -> Vec<&Item> {
        open_features(s)
            .into_iter()
            .filter(|i| at(i) && want(i.doc_state("spec")))
            .collect()
    };
    vec![
        (
            "spec:write",
            "spec to write",
            of(|d| matches!(d, Missing | Unknown)),
        ),
        (
            "spec:review",
            "spec in review (merge its pull request)",
            of(|d| matches!(d, Pr | Branch)),
        ),
        (
            "spec:merged",
            "spec merged: the plan is next",
            of(|d| d == Base),
        ),
    ]
}

/// Technical review: Features whose plan is being written, audited or
/// waits for its validation.
pub fn review_groups(s: &Snapshot) -> Groups<'_> {
    use crate::project::DocState::*;
    let mut failed = Vec::new();
    let mut in_review = Vec::new();
    let mut validate = Vec::new();
    for i in open_features(s) {
        if i.has_label(LABEL_PLAN_APPROVED) || i.has_label(LABEL_DECOMPOSED) {
            continue;
        }
        let plan = i.doc_state("plan");
        let in_plan = stage::shown(i).stage == STAGE_PLAN;
        if i.has_label(LABEL_CRITIQUE_FAILED) || i.has_label(LABEL_NEEDS_HUMAN) {
            failed.push(i);
        } else if matches!(plan, Pr | Branch) {
            in_review.push(i);
        } else if plan == Base {
            validate.push(i);
        } else if in_plan {
            // in the Plan column with no plan yet
            in_review.push(i);
        }
    }
    vec![
        (
            "review:failed",
            "the critique failed: correct the plan",
            failed,
        ),
        ("review:plan", "plan being written or in review", in_review),
        (
            "review:validate",
            "plan merged: validate to approve",
            validate,
        ),
    ]
}

/// Development: the Stories and Bugs whose code is to be written, by what
/// a build of each would meet. An item with an open pull request is the
/// Code review screen's.
pub fn development_groups(s: &Snapshot) -> Groups<'_> {
    let mut items: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| i.open && matches!(i.item_type, Some(ItemType::Story | ItemType::Bug)))
        .collect();
    items.sort_by(by_priority);
    let (mut going, mut ready, mut waiting) = (Vec::new(), Vec::new(), Vec::new());
    for i in items {
        let Some(d) = build::decision(i, s, false) else {
            continue;
        };
        let code = d.blocked.as_ref().map(|b| b.code);
        if d.action != next::Action::Implement || code == Some("pr-pending") {
            continue;
        }
        if code == Some("dependency-pending") {
            waiting.push(i);
        } else if stage::shown(i).stage == STAGE_DEVELOPMENT {
            going.push(i);
        } else if d.runnable() {
            ready.push(i);
        }
    }
    vec![
        (
            "development:going",
            "in development (r builds again from the default branch)",
            going,
        ),
        ("development:ready", "ready to build", ready),
        (
            "development:waiting",
            "waiting on a dependency that is not merged",
            waiting,
        ),
    ]
}

/// Code review: every item with an open pull request for its code, by
/// what the pull request waits on, and the items the board says are in
/// review with none.
pub fn code_review_groups(s: &Snapshot) -> Groups<'_> {
    let mut items: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| {
            i.open
                && i.item_type.is_some()
                && (i.pulls.iter().any(|p| p.open()) || stage::shown(i).stage == STAGE_CODE_REVIEW)
        })
        .collect();
    items.sort_by(by_priority);
    let (mut changes, mut approved, mut waiting, mut draft, mut none) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in items {
        let open: Vec<&crate::project::Pull> = i.pulls.iter().filter(|p| p.open()).collect();
        if open.is_empty() {
            // a Feature follows its Stories: it has no pull request of its own
            if !matches!(i.item_type, Some(ItemType::Feature | ItemType::Rfc)) {
                none.push(i);
            }
        } else if open.iter().any(|p| p.review == "CHANGES_REQUESTED") {
            changes.push(i);
        } else if open.iter().all(|p| p.draft) {
            draft.push(i);
        } else if open.iter().all(|p| p.review == "APPROVED") {
            approved.push(i);
        } else {
            waiting.push(i);
        }
    }
    vec![
        ("review:changes", "changes requested", changes),
        ("review:approved", "approved: merge it", approved),
        ("review:waiting", "waiting for a person's review", waiting),
        (
            "review:draft",
            "draft pull requests (review, then mark ready)",
            draft,
        ),
        (
            "review:none",
            "in review on the board, with no open pull request",
            none,
        ),
    ]
}

/// Decomposition: approved Features and RFCs on their way to Stories and
/// Tasks, by where their draft is.
pub fn decompose_groups(s: &Snapshot) -> Groups<'_> {
    use crate::project::DocState::*;
    let mut items: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| {
            i.open
                && !i.has_label(LABEL_DECOMPOSED)
                && match i.item_type {
                    Some(ItemType::Feature) => i.has_label(LABEL_PLAN_APPROVED),
                    Some(ItemType::Rfc) => true,
                    _ => false,
                }
        })
        .collect();
    items.sort_by(by_priority);
    let (mut draft, mut review, mut failed, mut audit, mut apply) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in items {
        let doc = i.doc_state("decomposition");
        if i.has_label(LABEL_CRITIQUE_FAILED) || i.has_label(LABEL_NEEDS_HUMAN) {
            failed.push(i);
        } else if matches!(doc, Pr | Branch) {
            review.push(i);
        } else if doc == Base && i.has_label(LABEL_DECOMPOSE_READY) {
            apply.push(i);
        } else if doc == Base {
            audit.push(i);
        } else {
            draft.push(i);
        }
    }
    vec![
        (
            "decompose:apply",
            "draft reviewed: apply to create the issues",
            apply,
        ),
        (
            "decompose:review",
            "draft in review (merge its pull request)",
            review,
        ),
        (
            "decompose:failed",
            "the critique failed: correct the draft",
            failed,
        ),
        ("decompose:audit", "draft merged: audit it", audit),
        ("decompose:draft", "to draft", draft),
    ]
}

/// The open Features of a milestone (`None`: of no milestone).
pub fn milestone_features<'a>(s: &'a Snapshot, title: Option<&str>) -> Vec<&'a Item> {
    let mut out: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| i.item_type == Some(ItemType::Feature) && i.milestone.as_deref() == title)
        .filter(|i| i.open || title.is_some())
        .collect();
    out.sort_by_key(|i| i.number);
    out
}

impl App {
    /// The folder a Project is opened on: the selected session's, else
    /// where agent-mux was started.
    fn project_dir(&self) -> Option<PathBuf> {
        self.sessions
            .get(self.selected)
            .map(|s| s.dir.clone())
            .or_else(|| std::env::current_dir().ok())
    }

    fn project_tools(&self) -> Tools {
        match &self.project_gh {
            Some(gh) => Tools { gh: gh.clone() },
            None => Tools::default(),
        }
    }

    pub fn open_project(&mut self) {
        let dir = self
            .project_dir_override
            .clone()
            .or_else(|| self.project_dir())
            .unwrap_or_else(|| PathBuf::from("."));
        self.open_project_in(&dir);
    }

    pub fn open_project_in(&mut self, dir: &Path) {
        let mut view = ProjectView::new(dir.to_path_buf());
        let mut stale = true;
        if let (Some(cfg), Some(db)) = (&view.cfg, self.trace_db_path.as_deref())
            && let Ok(conn) = crate::tracing::store::open_ro(db)
            && let Ok(Some(snap)) = store::load(&conn, &cfg.hub_repo().slug())
        {
            let age = (crate::project::now_ns() - snap.synced_ns) / 1_000_000_000;
            stale = age < 0 || age >= cli::ttl_secs(cfg);
            view.snap = Some(snap);
        }
        view.rebuild();
        self.mode = Mode::Project(Box::new(view));
        if stale {
            self.project_sync();
        }
    }

    /// Starts `agent-mux project <args>` as its own process and answers
    /// the file its output goes to.
    fn start_project_process(&self, name: &str, args: &[String]) -> Result<PathBuf, String> {
        let exe = match &self.project_exe {
            Some(p) => p.clone(),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        crate::factory::intake::spawn_cli_as(
            &exe,
            "project",
            &self.workflows_runtime_dir(),
            self.trace_db_path.as_deref(),
            name,
            args,
        )
    }

    /// The Project of `need`, on its item's screen.
    pub fn open_project_on(&mut self, need: &Need) {
        if need.root.is_empty() {
            self.notice = Some(Notice::info(
                "this Project was read before agent-mux kept its folder: open it from a session there",
            ));
            return;
        }
        self.open_project_in(Path::new(&need.root));
        if let Mode::Project(view) = &mut self.mode
            && view
                .snap
                .as_ref()
                .is_some_and(|s| s.item(need.number).is_some())
        {
            view.open_item_screen(need.number);
        }
    }

    /// Items of every Project read on this machine that wait on the
    /// operator, from the cache. Nothing is read from GitHub for this.
    pub fn project_inbox_items(&self) -> Vec<super::inbox::InboxItem> {
        let Some(conn) = self
            .trace_db_path
            .as_deref()
            .filter(|p| p.is_file())
            .and_then(|p| crate::tracing::store::open_ro(p).ok())
        else {
            return Vec::new();
        };
        let mut out: Vec<super::inbox::InboxItem> = store::hubs(&conn)
            .into_iter()
            .filter_map(|hub| store::load(&conn, &hub).ok().flatten())
            .flat_map(|s| needs_of(&s))
            .map(super::inbox::InboxItem::Project)
            .collect();
        out.retain(|i| !self.inbox_dismissed.contains(&i.key()));
        out
    }

    /// Reads the Project from GitHub on a thread of its own; the tick
    /// takes the result.
    pub fn project_sync(&mut self) {
        let tools = self.project_tools();
        let db = self.trace_db_path.clone();
        let Mode::Project(view) = &mut self.mode else {
            return;
        };
        let Some(cfg) = view.cfg.clone() else { return };
        if view.sync_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (htx, hrx) = channel();
        view.sync_rx = Some(rx);
        view.health_rx = Some(hrx);
        view.syncing_since = Some(Instant::now());
        std::thread::spawn(move || {
            let read = snapshot::load(&tools, &cfg);
            if let (Ok(snap), Some(db)) = (&read, &db)
                && let Ok(conn) = cli::open_store(db)
            {
                let _ = store::save(&conn, snap);
            }
            let _ = tx.send(read.clone());
            let probe = doctor::probe(&tools, &cfg);
            let _ = htx.send(doctor::checks(&cfg, &read, &probe));
        });
    }

    fn project_load_thread(&mut self, number: u64) {
        let tools = self.project_tools();
        let Mode::Project(view) = &mut self.mode else {
            return;
        };
        if view.thread.as_ref().is_some_and(|t| t.number == number) {
            return;
        }
        let Some(cfg) = view.cfg.clone() else { return };
        let (tx, rx) = channel();
        view.thread = None;
        view.thread_rx = Some(rx);
        std::thread::spawn(move || {
            let lines = crate::project::github::issue_thread(&tools, &cfg, number);
            let _ = tx.send(Thread { number, lines });
        });
    }

    /// On the tick: results of reads that finished.
    pub fn project_tick(&mut self) {
        let Mode::Project(view) = &mut self.mode else {
            return;
        };
        let mut notice = None;
        if let Some(rx) = &view.sync_rx
            && let Ok(result) = rx.try_recv()
        {
            view.sync_rx = None;
            view.syncing_since = None;
            match result {
                Ok(snap) => {
                    view.snap = Some(snap);
                    view.sync_error = None;
                }
                Err(e) => {
                    notice = Some(Notice::error(format!("project: {e}")));
                    view.sync_error = Some(e);
                }
            }
            view.rebuild();
        }
        if let Some(rx) = &view.health_rx
            && let Ok(checks) = rx.try_recv()
        {
            view.health_rx = None;
            view.checks = Some(checks);
            view.rebuild();
        }
        if let Some(rx) = &view.thread_rx
            && let Ok(thread) = rx.try_recv()
        {
            view.thread_rx = None;
            view.thread = Some(thread);
        }
        if let Some(rx) = &view.op_rx
            && let Ok(done) = rx.try_recv()
        {
            view.op_rx = None;
            view.busy = None;
            if let Some(snap) = done.snap {
                view.snap = Some(snap);
            }
            notice = Some(match done.result {
                Ok(report) => {
                    let text = report.lines.join(" · ");
                    if report.lines.iter().any(|l| l.starts_with('!')) {
                        Notice::warn(text)
                    } else {
                        Notice::info(text)
                    }
                }
                Err(e) => Notice::error(format!("project: {e}")),
            });
            view.rebuild();
        }
        if notice.is_some() {
            self.notice = notice;
        }
    }

    /// Makes a change the operator confirmed, on a thread of its own. The
    /// tick takes its report and the issues it changed, read again.
    pub fn project_run(&mut self, op: Op) {
        let tools = self.project_tools();
        let db = self.trace_db_path.clone();
        let Mode::Project(view) = &mut self.mode else {
            return;
        };
        let Some(cfg) = view.cfg.clone() else { return };
        if let Op::RunStep { number, apply } = &op {
            // a step takes minutes and runs a harness: it is a process of
            // its own, as the factory's are, and the runs view follows it
            let mut args = vec![
                "run".to_string(),
                number.to_string(),
                "--dir".to_string(),
                cfg.root().to_string_lossy().into_owned(),
            ];
            if *apply {
                args.push("--apply".to_string());
            }
            let name = format!("project-run-{number}-{}", crate::project::now_ns());
            self.notice = Some(match self.start_project_process(&name, &args) {
                Ok(log) => Notice::info(format!(
                    "started the next step of #{number}; its output is in {}. [Ctrl+R] reads the result",
                    log.display()
                )),
                Err(e) => Notice::error(format!("project: {e}")),
            });
            return;
        }
        if let Op::Process { number, args, what } = &op {
            let mut args = args.clone();
            args.push("--dir".to_string());
            args.push(cfg.root().to_string_lossy().into_owned());
            let name = format!("project-{}-{number}-{}", args[0], crate::project::now_ns());
            self.notice = Some(match self.start_project_process(&name, &args) {
                Ok(log) => Notice::info(format!(
                    "started {what} of #{number}; its output is in {}. [Ctrl+R] reads the result",
                    log.display()
                )),
                Err(e) => Notice::error(format!("project: {e}")),
            });
            return;
        }
        if view.op_rx.is_some() {
            return;
        }
        let mut snap = view.snap.clone();
        let (tx, rx) = channel();
        view.op_rx = Some(rx);
        view.busy = Some(op.label());
        std::thread::spawn(move || {
            let run = ops::Ops {
                tools: &tools,
                cfg: &cfg,
                seen_board: snap.as_ref().and_then(|s| s.board_id.clone()),
            };
            let result = match &op {
                Op::New(new) => run.new_item(new),
                Op::Move { number, stage } => run.move_item(*number, stage, None),
                Op::Repair {
                    number,
                    stage,
                    reason,
                } => run.repair_stage(&[*number], stage, None, true, reason),
                Op::Triage {
                    action,
                    number,
                    severity,
                    reason,
                    of,
                } => run.triage(action, *number, severity.as_deref(), reason.as_deref(), *of),
                Op::Advance(moves) => run.apply_moves(moves),
                Op::RunStep { .. } | Op::Process { .. } => {
                    Err("a step is started as its own process".to_string())
                }
            };
            // what changed is read again, into the screen and the cache
            let mut fresh = None;
            if let (Ok(report), Some(s)) = (&result, snap.as_mut())
                && !report.touched.is_empty()
                && let Ok(nodes) =
                    crate::project::github::issue_nodes(&tools, &cfg, &report.touched)
            {
                snapshot::merge(s, &nodes, &cfg);
                if let Some(db) = &db
                    && let Ok(conn) = cli::open_store(db)
                {
                    let _ = store::save(&conn, s);
                }
                fresh = snap;
            }
            let _ = tx.send(OpDone {
                result,
                snap: fresh,
            });
        });
    }

    pub fn handle_project_key(&mut self, key: &KeyEvent) {
        let Mode::Project(view) = &mut self.mode else {
            return;
        };
        match view.handle_key(key) {
            Outcome::None => {}
            Outcome::Close => self.mode = Mode::Main,
            Outcome::Sync => {
                if view.cfg.is_none() {
                    self.notice = Some(Notice::info("no .spec-wave.json here: nothing to read"));
                } else if view.syncing() {
                    self.notice = Some(Notice::info("the Project is being read"));
                } else {
                    self.project_sync();
                }
            }
            Outcome::Notice(text) => self.notice = Some(Notice::info(text)),
            Outcome::OpenUrl(url) => self.open_url(&url),
            Outcome::OpenDoc(path) => self.open_markdown(&path),
            Outcome::LoadThread(n) => self.project_load_thread(n),
            Outcome::Run(op) => self.project_run(op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::tests as fixture;

    fn view() -> ProjectView {
        let dir = tempfile::tempdir().unwrap();
        let mut v = ProjectView::new(dir.path().to_path_buf());
        assert!(
            v.error
                .as_deref()
                .unwrap()
                .contains("not a spec-wave repository")
        );
        v.cfg = Some(fixture::cfg());
        v.error = None;
        v.snap = Some(snapshot::build(&fixture::raw(), &fixture::cfg(), 1));
        v.rebuild();
        v
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn items(rows: &[Row]) -> Vec<u64> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Item { number, .. } => Some(*number),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_screen_builds_rows_from_the_fixture() {
        let s = snapshot::build(&fixture::raw(), &fixture::cfg(), 1);
        let none = BTreeSet::new();
        // Home: the Feature's decomposition waits for a merge, the Bug's document too
        let home = rows_for(Screen::Home, &s, "", &none, None);
        assert!(matches!(&home[0], Row::Heading { key, count: 2, .. } if key == "home:needs"));
        // Board: one heading per stage, the Feature under Ready, the Story under development
        let board = rows_for(Screen::Board, &s, "", &none, None);
        let at = |stage: &str| {
            board
                .iter()
                .position(
                    |r| matches!(r, Row::Heading { key, .. } if key == &format!("stage:{stage}")),
                )
                .unwrap()
        };
        assert_eq!(
            board[at(STAGE_READY) + 1],
            Row::Item {
                number: 1,
                depth: 0
            }
        );
        assert_eq!(
            board[at(STAGE_DEVELOPMENT) + 1],
            Row::Item {
                number: 2,
                depth: 0
            }
        );
        // not on the board: shown at its derived stage
        assert_eq!(
            board[at(STAGE_TRIAGE) + 1],
            Row::Item {
                number: 4,
                depth: 0
            }
        );
        // Ready: Story #3 is derived Ready, under its Feature
        let ready = rows_for(Screen::Ready, &s, "", &none, None);
        assert!(matches!(&ready[0], Row::Heading { key, .. } if key == "feature:1"));
        assert_eq!(items(&ready), vec![3]);
        // Milestones: the open one; no Feature is without a milestone
        assert_eq!(
            rows_for(Screen::Milestones, &s, "", &none, None),
            vec![Row::Milestone(Some(0))]
        );
        assert_eq!(milestone_features(&s, Some("v1")).len(), 1);
        // a filter keeps what matches
        let filtered = rows_for(Screen::Board, &s, "pix", &none, None);
        assert_eq!(items(&filtered), vec![4]);
        // a collapsed heading keeps its count and hides its rows
        let mut c = BTreeSet::new();
        c.insert(format!("stage:{STAGE_READY}"));
        let collapsed = rows_for(Screen::Board, &s, "", &c, None);
        assert!(!items(&collapsed).contains(&1));
    }

    #[test]
    fn the_backlog_is_a_tree_of_what_is_before_spec() {
        let mut raw = fixture::raw();
        raw.issues.push(serde_json::json!({
            "number": 20, "title": "[EPIC] Vendas", "state": "OPEN", "body": "",
            "labels": {"nodes": []}, "subIssues": {"nodes": [{"number": 21}]}}));
        raw.issues.push(serde_json::json!({
            "number": 21, "title": "[FEATURE] Cupom", "state": "OPEN", "body": "",
            "labels": {"nodes": []}, "parent": {"number": 20}}));
        let s = snapshot::build(&raw, &fixture::cfg(), 1);
        let rows = rows_for(Screen::Backlog, &s, "", &BTreeSet::new(), None);
        assert_eq!(
            rows,
            vec![
                Row::Item {
                    number: 20,
                    depth: 0
                },
                Row::Item {
                    number: 21,
                    depth: 1
                }
            ]
        );
        // a filter that matches the child keeps its parent
        let rows = rows_for(Screen::Backlog, &s, "cupom", &BTreeSet::new(), None);
        assert_eq!(items(&rows), vec![20, 21]);
    }

    #[test]
    fn keys_move_between_screens_rows_and_the_item_screen() {
        let mut v = view();
        assert_eq!(v.screen, Screen::Home);
        v.handle_key(&key(KeyCode::Char(']')));
        assert_eq!(v.screen, Screen::Board);
        v.handle_key(&key(KeyCode::Tab));
        assert_eq!(v.focus, Focus::Nav);
        v.handle_key(&key(KeyCode::Down));
        assert_eq!(v.screen, Screen::Backlog);
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(v.focus, Focus::List);
        v.handle_key(&key(KeyCode::Char('[')));
        assert_eq!(v.screen, Screen::Board);

        // onto the Feature's row, then into its item screen
        let at = v
            .rows
            .iter()
            .position(|r| matches!(r, Row::Item { number: 1, .. }))
            .unwrap();
        v.cursors[Screen::Board.index()] = at;
        assert_eq!(v.selected_item().unwrap().number, 1);
        assert_eq!(v.handle_key(&key(KeyCode::Enter)), Outcome::None);
        assert_eq!(v.item.as_ref().unwrap().number, 1);
        // Children, onto the second, into it, and back
        v.handle_key(&key(KeyCode::Char('3')));
        v.handle_key(&key(KeyCode::Down));
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(v.item.as_ref().unwrap().number, 3);
        assert_eq!(v.item.as_ref().unwrap().back, vec![1]);
        // the Thread tab asks for the issue's comments
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('5'))),
            Outcome::LoadThread(3)
        );
        v.handle_key(&key(KeyCode::Esc));
        assert_eq!(v.item.as_ref().unwrap().number, 1);
        v.handle_key(&key(KeyCode::Esc));
        assert!(v.item.is_none());
        assert_eq!(v.handle_key(&key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn a_document_opens_where_it_is() {
        let mut v = view();
        v.item = Some(ItemScreen {
            number: 4,
            tab: 1,
            cursor: 0,
            scroll: 0,
            back: vec![],
        });
        // the Bug's document is in pull request #40
        assert_eq!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::OpenUrl("https://github.com/acme/hub/pull/40".into())
        );
        // a document on base that the local checkout lacks
        v.item = Some(ItemScreen {
            number: 1,
            tab: 1,
            cursor: 2,
            scroll: 0,
            back: vec![],
        });
        let doc = v.open_item().unwrap().docs.values().nth(2).unwrap().clone();
        assert_eq!(doc.state, crate::project::DocState::Missing);
        assert!(matches!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::Notice(_)
        ));
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('o'))),
            Outcome::OpenUrl("https://github.com/acme/hub/issues/1".into())
        );
    }

    #[test]
    fn a_filter_is_typed_and_cleared() {
        let mut v = view();
        v.handle_key(&key(KeyCode::Char(']')));
        v.handle_key(&key(KeyCode::Char('/')));
        assert!(v.text_entry_active());
        for c in "pix".chars() {
            v.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(items(&v.rows), vec![4]);
        v.handle_key(&key(KeyCode::Enter));
        assert!(!v.text_entry_active());
        // the first Esc clears the filter, the second closes
        assert_eq!(v.handle_key(&key(KeyCode::Esc)), Outcome::None);
        assert!(v.filter.is_empty());
        // plain `r` is a verb (run the next step): on a heading it only
        // says to select an item, and reloads nothing
        assert!(matches!(
            v.handle_key(&key(KeyCode::Char('r'))),
            Outcome::Notice(_)
        ));
        assert_eq!(
            v.handle_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Outcome::Sync
        );
    }
    fn typed(v: &mut ProjectView, text: &str) {
        for c in text.chars() {
            v.handle_key(&key(KeyCode::Char(c)));
        }
    }

    fn on_item(v: &mut ProjectView, screen: Screen, number: u64) {
        v.screen = screen;
        v.rebuild();
        let at = v
            .rows
            .iter()
            .position(|r| matches!(r, Row::Item { number: n, .. } if *n == number))
            .unwrap_or_else(|| panic!("#{number} is not on {screen:?}"));
        v.cursors[screen.index()] = at;
    }

    #[test]
    fn bugs_and_acceptance_have_their_own_screens() {
        let mut raw = fixture::raw();
        raw.issues.push(serde_json::json!({
            "number": 30, "title": "[STORY] aceitar", "state": "OPEN", "body": "",
            "labels": {"nodes": []},
            "projectItems": {"nodes": [{"id": "I30", "project": {"id": "PVT_x"}, "fieldValues": {"nodes": [
                {"name": "📋 Homologação", "field": {"name": "Etapa"}}]}}]}}));
        let s = snapshot::build(&raw, &fixture::cfg(), 1);
        let none = BTreeSet::new();
        let bugs = rows_for(Screen::Bugs, &s, "", &none, None);
        assert!(matches!(&bugs[0], Row::Heading { text, count: 1, .. } if text == "to triage"));
        assert_eq!(items(&bugs), vec![4]);
        let uat = rows_for(Screen::Uat, &s, "", &none, None);
        assert!(matches!(&uat[0], Row::Heading { count: 1, .. }));
        assert_eq!(items(&uat), vec![30]);
        assert!(matches!(&uat[2], Row::Heading { count: 0, .. }));
    }

    #[test]
    fn development_and_code_review_have_their_own_screens() {
        let mut raw = fixture::raw();
        let mut push = |n: u64, title: &str, body: &str, stage: &str| {
            raw.issues.push(serde_json::json!({
                "number": n, "title": title, "state": "OPEN", "body": body,
                "labels": {"nodes": []},
                "projectItems": {"nodes": [{"id": format!("I{n}"), "project": {"id": "PVT_x"},
                    "fieldValues": {"nodes": [{"name": stage, "field": {"name": "Etapa"}}]}}]}}));
        };
        push(31, "[STORY] pagar", "", STAGE_READY);
        push(32, "[STORY] estornar", "Depende de: #31", STAGE_READY);
        push(33, "[STORY] listar", "", STAGE_DEVELOPMENT);
        let s = snapshot::build(&raw, &fixture::cfg(), 1);
        let of = |groups: Groups, key: &str| -> Vec<u64> {
            groups
                .into_iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, _, v)| v.iter().map(|i| i.number).collect())
                .unwrap()
        };
        assert_eq!(of(development_groups(&s), "development:going"), vec![33]);
        assert!(of(development_groups(&s), "development:ready").contains(&31));
        // the fixture's Story #3 waits too: it depends on #2, still in review
        assert_eq!(
            of(development_groups(&s), "development:waiting"),
            vec![3, 32]
        );
        // the fixture's Story #2 has an open draft pull request: it is
        // review's, and a second build of it is refused
        assert!(
            development_groups(&s)
                .iter()
                .all(|(_, _, v)| v.iter().all(|i| i.number != 2))
        );
        assert_eq!(of(code_review_groups(&s), "review:draft"), vec![2]);

        let mut v = view();
        v.snap = Some(s);
        on_item(&mut v, Screen::Development, 31);
        assert_eq!(v.handle_key(&key(KeyCode::Char('r'))), Outcome::None);
        let Some(Modal::Confirm { lines, op }) = &v.modal else {
            panic!("no confirmation: {:?}", v.modal)
        };
        assert!(lines[0].starts_with("Build Story #31"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("draft pull request")));
        assert_eq!(
            *op,
            Op::RunStep {
                number: 31,
                apply: false
            }
        );
        v.modal = None;
        on_item(&mut v, Screen::Development, 32);
        let Outcome::Notice(said) = v.handle_key(&key(KeyCode::Char('r'))) else {
            panic!("a blocked build asked for a confirmation")
        };
        assert!(said.contains("dependency-pending"), "{said}");
        on_item(&mut v, Screen::CodeReview, 2);
        let Outcome::Notice(said) = v.handle_key(&key(KeyCode::Char('r'))) else {
            panic!("a second build was offered")
        };
        assert!(said.contains("pr-pending"), "{said}");

        // its pull request is a draft: the merge is refused with the reason,
        // and fix-pr asks first
        let Outcome::Notice(said) = v.handle_key(&key(KeyCode::Char('P'))) else {
            panic!("a draft was offered for merge: {:?}", v.modal)
        };
        assert!(said.contains("draft"), "{said}");
        assert_eq!(v.handle_key(&key(KeyCode::Char('f'))), Outcome::None);
        let Some(Modal::Confirm { lines, op }) = &v.modal else {
            panic!("no confirmation: {:?}", v.modal)
        };
        assert!(
            lines[0].starts_with("Fix what the review of pull request #"),
            "{lines:?}"
        );
        assert_eq!(
            *op,
            Op::Process {
                number: 2,
                args: vec!["run".into(), "2".into(), "--step".into(), "fix-pr".into()],
                what: "the fix-pr step".into(),
            }
        );
        v.modal = None;
        // marked ready, the merge shows its plan and asks
        let snap = v.snap.as_mut().unwrap();
        let story = snap.items.iter_mut().find(|i| i.number == 2).unwrap();
        story.pulls.iter_mut().for_each(|p| p.draft = false);
        assert_eq!(v.handle_key(&key(KeyCode::Char('P'))), Outcome::None);
        let Some(Modal::Confirm { lines, op }) = &v.modal else {
            panic!("no merge confirmation: {:?}", v.modal)
        };
        assert_eq!(lines[0], "Merge #2");
        assert!(lines[1].starts_with("merge order"), "{lines:?}");
        assert_eq!(
            *op,
            Op::Process {
                number: 2,
                args: vec!["merge".into(), "2".into(), "--yes".into()],
                what: "the merge".into(),
            }
        );
        // an item with no pull request has nothing to fix
        v.modal = None;
        on_item(&mut v, Screen::Development, 31);
        let Outcome::Notice(said) = v.handle_key(&key(KeyCode::Char('f'))) else {
            panic!("fix-pr was offered with no pull request")
        };
        assert!(said.contains("one open pull request"), "{said}");
    }

    #[test]
    fn a_move_is_picked_then_confirmed() {
        let mut v = view();
        on_item(&mut v, Screen::Board, 1);
        assert_eq!(v.handle_key(&key(KeyCode::Char('m'))), Outcome::None);
        let Some(Modal::Pick { options, .. }) = &v.modal else {
            panic!("no picker: {:?}", v.modal)
        };
        // the Feature is in Ready: only later stages are offered
        assert_eq!(options.first().map(String::as_str), Some(STAGE_DEVELOPMENT));
        assert!(
            !options
                .iter()
                .any(|o| o == STAGE_READY || o == STAGE_BACKLOG)
        );
        v.handle_key(&key(KeyCode::Down));
        v.handle_key(&key(KeyCode::Enter));
        assert!(v.confirmation_active());
        // anything but yes keeps or cancels; `n` cancels and writes nothing
        assert_eq!(v.handle_key(&key(KeyCode::Char('n'))), Outcome::None);
        assert!(v.modal.is_none());

        v.handle_key(&key(KeyCode::Char('m')));
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('y'))),
            Outcome::Run(Op::Move {
                number: 1,
                stage: STAGE_DEVELOPMENT.into()
            })
        );
        assert!(v.modal.is_none());
    }

    #[test]
    fn a_repair_offers_every_stage_and_asks_why() {
        let mut v = view();
        on_item(&mut v, Screen::Board, 2);
        v.handle_key(&key(KeyCode::Char('M')));
        let Some(Modal::Pick { options, .. }) = &v.modal else {
            panic!()
        };
        // every stage but its own, earlier ones included
        assert_eq!(options.len(), STAGE_ORDER.len() - 1);
        assert_eq!(options[0], STAGE_BACKLOG);
        v.handle_key(&key(KeyCode::Enter));
        assert!(v.text_entry_active());
        // an empty reason is not accepted
        assert!(matches!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::Notice(_)
        ));
        typed(&mut v, "corrida");
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::Run(Op::Repair {
                number: 2,
                stage: STAGE_BACKLOG.into(),
                reason: "corrida".into()
            })
        );
    }

    #[test]
    fn triage_is_for_a_bug_and_each_outcome_is_confirmed() {
        let mut v = view();
        on_item(&mut v, Screen::Board, 1);
        assert!(matches!(
            v.handle_key(&key(KeyCode::Char('t'))),
            Outcome::Notice(_)
        ));
        on_item(&mut v, Screen::Bugs, 4);
        // accept, reclassified as P1
        v.handle_key(&key(KeyCode::Char('t')));
        v.handle_key(&key(KeyCode::Down));
        v.handle_key(&key(KeyCode::Down));
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('y'))),
            Outcome::Run(Op::Triage {
                action: "accept",
                number: 4,
                severity: Some("P1".into()),
                reason: None,
                of: None
            })
        );
        // reject needs its reason
        v.handle_key(&key(KeyCode::Char('R')));
        typed(&mut v, "esperado");
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('y'))),
            Outcome::Run(Op::Triage {
                action: "reject",
                number: 4,
                severity: None,
                reason: Some("esperado".into()),
                of: None
            })
        );
        // a duplicate needs another issue's number
        v.handle_key(&key(KeyCode::Char('D')));
        typed(&mut v, "4");
        assert!(matches!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::Notice(_)
        ));
        v.handle_key(&key(KeyCode::Backspace));
        typed(&mut v, "#17");
        v.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            v.handle_key(&key(KeyCode::Enter)),
            Outcome::Run(Op::Triage {
                action: "duplicate",
                number: 4,
                severity: None,
                reason: None,
                of: Some(17)
            })
        );
    }

    #[test]
    fn a_new_item_starts_under_the_selected_one() {
        let mut v = view();
        on_item(&mut v, Screen::Board, 1);
        v.handle_key(&key(KeyCode::Char('n')));
        let Some(Modal::New(form)) = &v.modal else {
            panic!()
        };
        // under a Feature, a Story, with the Feature as its parent
        assert_eq!(ItemType::ALL[form.item_type], ItemType::Story);
        assert_eq!(form.parent, "1");
        assert!(v.text_entry_active());
        // letters are the title, verbs included
        typed(&mut v, "mover item");
        // an empty title was refused before: here it is typed, so Enter confirms
        v.handle_key(&key(KeyCode::Enter));
        let Some(Modal::Confirm { lines, op }) = &v.modal else {
            panic!("{:?}", v.modal)
        };
        assert!(lines[0].contains("Story «mover item»"));
        assert!(lines.iter().any(|l| l.contains("sub-issue of #1")));
        assert!(
            matches!(op, Op::New(n) if n.parent == Some(1) && n.item_type == Some(ItemType::Story))
        );
        // Esc leaves without writing
        assert_eq!(v.handle_key(&key(KeyCode::Esc)), Outcome::None);
        assert!(v.modal.is_none());
    }

    #[test]
    fn a_change_in_flight_holds_the_next_one() {
        let mut v = view();
        on_item(&mut v, Screen::Board, 1);
        v.busy = Some("moving #1".into());
        assert!(
            matches!(v.handle_key(&key(KeyCode::Char('m'))), Outcome::Notice(t) if t.contains("moving #1"))
        );
        assert!(v.modal.is_none());
        v.busy = None;
        // nothing to catch up in the fixture
        assert!(matches!(
            v.handle_key(&key(KeyCode::Char('A'))),
            Outcome::Notice(_)
        ));
    }
    #[test]
    fn the_decomposition_screen_follows_the_draft() {
        use crate::project::{Doc, DocState};
        let mut s = snapshot::build(&fixture::raw(), &fixture::cfg(), 1);
        let doc = |state| Doc {
            state,
            path: String::new(),
            pr: None,
            branch: None,
        };
        let names = |s: &Snapshot| -> Vec<(&'static str, Vec<u64>)> {
            decompose_groups(s)
                .into_iter()
                .filter(|(_, _, v)| !v.is_empty())
                .map(|(k, _, v)| (k, v.iter().map(|i| i.number).collect()))
                .collect()
        };
        // the fixture's Feature is approved and its draft is on a branch
        assert_eq!(names(&s), vec![("decompose:review", vec![1])]);
        s.items[0]
            .docs
            .insert("decomposition".into(), doc(DocState::Missing));
        assert_eq!(names(&s), vec![("decompose:draft", vec![1])]);
        s.items[0]
            .docs
            .insert("decomposition".into(), doc(DocState::Base));
        assert_eq!(names(&s), vec![("decompose:audit", vec![1])]);
        s.items[0].labels.push(LABEL_DECOMPOSE_READY.to_string());
        assert_eq!(names(&s), vec![("decompose:apply", vec![1])]);
        s.items[0].labels.push(LABEL_CRITIQUE_FAILED.to_string());
        assert_eq!(names(&s), vec![("decompose:failed", vec![1])]);
        // once applied it is no longer here
        s.items[0].labels.push(LABEL_DECOMPOSED.to_string());
        assert!(names(&s).is_empty());

        // `r` on a reviewed draft asks before it creates anything
        let mut v = view();
        let it = &mut v.snap.as_mut().unwrap().items[0];
        it.labels.push(LABEL_DECOMPOSE_READY.to_string());
        for d in ["spec", "plan", "decomposition"] {
            it.docs.insert(d.into(), doc(DocState::Base));
        }
        on_item(&mut v, Screen::Decompose, 1);
        v.handle_key(&key(KeyCode::Char('r')));
        let Some(Modal::Confirm { lines, op }) = &v.modal else {
            panic!("{:?}", v.modal)
        };
        assert!(
            lines
                .iter()
                .any(|l| l.contains("your approval of the draft")),
            "{lines:?}"
        );
        assert_eq!(
            op,
            &Op::RunStep {
                number: 1,
                apply: true
            }
        );
    }
}
