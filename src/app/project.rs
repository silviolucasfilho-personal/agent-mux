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
use crate::project::{Item, ItemType, Snapshot, cli, order, snapshot, stage, store};
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
    Ready,
    Milestones,
    Health,
}

impl Screen {
    pub const ALL: [Screen; 6] = [
        Screen::Home,
        Screen::Board,
        Screen::Backlog,
        Screen::Ready,
        Screen::Milestones,
        Screen::Health,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Screen::Home => "Home",
            Screen::Board => "Board",
            Screen::Backlog => "Backlog",
            Screen::Ready => "Ready",
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
    pub cursors: [usize; 6],
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
            cursors: [0; 6],
            tab: 0,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            collapsed,
            item: None,
            thread: None,
            checks: None,
            syncing_since: None,
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
        self.filtering
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
            Screen::Ready => s
                .items
                .iter()
                .filter(|i| {
                    i.open
                        && matches!(i.item_type, Some(ItemType::Story | ItemType::Bug))
                        && stage::shown(i).stage == STAGE_READY
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
        Screen::Health => {
            if let Some(c) = checks {
                rows.extend((0..c.len()).map(Row::Check));
            }
        }
    }
    rows
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
        if notice.is_some() {
            self.notice = notice;
        }
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
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('r')).clone()),
            Outcome::None
        );
        assert_eq!(
            v.handle_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Outcome::Sync
        );
    }
}
