//! The Memory screen (F1 → "Open memory"): ai-memory's setup at a glance
//! (`crate::memory::State`: the binary, the server, each harness's MCP
//! entry and hooks), the install flow (`crate::memory::install`), and the
//! memory itself for the project folder the screen was opened on.
//!
//! - `i` shows the steps that are missing and `y` runs them; `u` removes
//!   ai-memory's wiring and stops its server (`p` also deletes the
//!   memory), `Ctrl+R` reads the state again. Steps run on a thread; their output is the log below the
//!   state, and the state is read again when they finish.
//! - `/` searches the project's memory (`memory::query::search`); `Enter`
//!   on a result opens the page in the Markdown viewer.
//! - `a` asks a question: a new session on a wired harness, its opening
//!   message the question (`memory::query::ask_prompt`); `Tab` picks the
//!   harness.
//! - `w` opens ai-memory's web view in the browser.

use crate::harness::Harness;
use crate::memory::install::{self, Options, Step};
use crate::memory::query::{self, Hit};
use crate::memory::{State, home};
use crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// What `y` would run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    Install,
    Uninstall { purge: bool },
}

/// A message from the thread running the steps.
#[derive(Debug)]
pub enum JobEvent {
    Line(String),
    Finished(Result<(), String>),
}

/// A line being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    Search,
    Ask(Harness),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
}

#[derive(Debug)]
pub struct MemoryView {
    pub state: State,
    /// The project folder the memory is read for.
    pub dir: PathBuf,
    /// Where opened pages are written for the viewer.
    pub pages_dir: PathBuf,
    pub input: Option<Input>,
    /// The last search: its query and results, and the one selected.
    pub hits: Option<(String, Vec<Hit>)>,
    pub selected: usize,
    /// The steps waiting for `y`, and what they are.
    pub pending: Option<(Pending, Vec<Step>)>,
    pub log: Vec<String>,
    job: Option<Receiver<JobEvent>>,
    /// Lines scrolled up from the end of the log.
    pub scroll: usize,
    pub error: Option<String>,
}

/// What a key asks of the App.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    OpenUrl(String),
    OpenPage(PathBuf),
    /// A new session on this harness, in `dir`, opening with this message.
    Ask {
        harness: Harness,
        dir: PathBuf,
        message: String,
    },
}

impl MemoryView {
    pub fn open(dir: PathBuf, pages_dir: PathBuf) -> MemoryView {
        let mut v = MemoryView::with_state(State::detect());
        v.dir = dir;
        v.pages_dir = pages_dir;
        v
    }

    pub fn with_state(state: State) -> MemoryView {
        MemoryView {
            state,
            dir: PathBuf::from("."),
            pages_dir: std::env::temp_dir().join("agent-mux-memory"),
            input: None,
            hits: None,
            selected: 0,
            pending: None,
            log: Vec::new(),
            job: None,
            scroll: 0,
            error: None,
        }
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// The harnesses a question can go to: installed and wired.
    pub fn askable(&self) -> Vec<Harness> {
        self.state
            .harnesses
            .iter()
            .filter(|w| w.cli && w.complete())
            .map(|w| w.harness)
            .collect()
    }

    /// Why the memory cannot be read now, if it cannot.
    fn unreadable(&self) -> Option<&'static str> {
        if self.state.binary.is_none() {
            Some("ai-memory is not installed: i installs it")
        } else if !self.state.server {
            Some("the ai-memory server is not running: i starts it")
        } else {
            None
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.input.is_some() {
            return self.input_key(key);
        }
        if self.pending.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.start(),
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    if let Some((Pending::Uninstall { purge }, _)) = &self.pending {
                        self.ask(Pending::Uninstall { purge: !purge });
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.pending = None,
                _ => {}
            }
            return Outcome::None;
        }
        if self.hits.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.hits = None;
                    return Outcome::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    return Outcome::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = self.hits.as_ref().map(|(_, h)| h.len()).unwrap_or(0);
                    self.selected = (self.selected + 1).min(n.saturating_sub(1));
                    return Outcome::None;
                }
                KeyCode::Enter => return self.open_selected(),
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('/') => match self.unreadable() {
                Some(why) => self.error = Some(why.into()),
                None => {
                    self.error = None;
                    self.input = Some(Input {
                        kind: InputKind::Search,
                        text: String::new(),
                    });
                }
            },
            KeyCode::Char('a') => match (self.unreadable(), self.askable().first()) {
                (Some(why), _) => self.error = Some(why.into()),
                (None, None) => {
                    self.error = Some("no harness is wired yet: i wires the installed ones".into())
                }
                (None, Some(&h)) => {
                    self.error = None;
                    self.input = Some(Input {
                        kind: InputKind::Ask(h),
                        text: String::new(),
                    });
                }
            },
            KeyCode::Char('w') => {
                if self.state.server {
                    return Outcome::OpenUrl(format!(
                        "http://{}/web",
                        crate::memory::server_addr()
                    ));
                }
                self.error = Some("the ai-memory server is not running: i starts it".into());
            }
            KeyCode::Char('i') if !self.running() => self.ask(Pending::Install),
            KeyCode::Char('u') if !self.running() => self.ask(Pending::Uninstall { purge: false }),
            // Ctrl+R: the shared Reload
            KeyCode::Char('r')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && !self.running() =>
            {
                self.state = State::detect();
                self.error = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = (self.scroll + 1).min(self.log.len().saturating_sub(1))
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_sub(1),
            _ => {}
        }
        Outcome::None
    }

    /// A key while a line is typed: letters are text.
    fn input_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(input) = self.input.as_mut() else {
            return Outcome::None;
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Tab => {
                if let InputKind::Ask(h) = input.kind {
                    let choices = self.askable();
                    let next = choices
                        .iter()
                        .position(|&c| c == h)
                        .map(|i| choices[(i + 1) % choices.len()])
                        .unwrap_or(h);
                    if let Some(input) = self.input.as_mut() {
                        input.kind = InputKind::Ask(next);
                    }
                }
            }
            KeyCode::Enter => {
                let Some(input) = self.input.take() else {
                    return Outcome::None;
                };
                let text = input.text.trim().to_string();
                if text.is_empty() {
                    return Outcome::None;
                }
                match input.kind {
                    InputKind::Search => self.search(&text),
                    InputKind::Ask(harness) => {
                        return Outcome::Ask {
                            harness,
                            dir: self.dir.clone(),
                            message: query::ask_prompt(&text),
                        };
                    }
                }
            }
            KeyCode::Char(c) => input.text.push(c),
            _ => {}
        }
        Outcome::None
    }

    fn search(&mut self, text: &str) {
        let Some(bin) = self.state.binary.as_ref().map(|b| b.path.clone()) else {
            return;
        };
        match query::search(&bin, &self.dir, text, 30) {
            Ok(hits) => {
                self.error = hits
                    .is_empty()
                    .then(|| format!("nothing in this project's memory matches {text:?}"));
                self.hits = (!hits.is_empty()).then(|| (text.to_string(), hits));
                self.selected = 0;
            }
            Err(e) => self.error = Some(format!("search: {e}")),
        }
    }

    fn open_selected(&mut self) -> Outcome {
        let Some(hit) = self
            .hits
            .as_ref()
            .and_then(|(_, h)| h.get(self.selected))
            .cloned()
        else {
            return Outcome::None;
        };
        let Some(bin) = self.state.binary.as_ref().map(|b| b.path.clone()) else {
            return Outcome::None;
        };
        match query::read_page(&bin, &self.dir, &hit.path, &self.pages_dir) {
            Ok(file) => Outcome::OpenPage(file),
            Err(e) => {
                self.error = Some(format!("{}: {e}", hit.path));
                Outcome::None
            }
        }
    }

    /// Plans `what` and asks for `y`; nothing to do is said instead.
    fn ask(&mut self, what: Pending) {
        let home = home();
        let steps = match &what {
            Pending::Install => install::plan(&self.state, &home, &Options::default()),
            Pending::Uninstall { purge } => Ok(install::plan_uninstall(&self.state, &home, *purge)),
        };
        self.error = None;
        match steps {
            Ok(steps) if steps.is_empty() => {
                self.pending = None;
                self.error = Some(match what {
                    Pending::Install => {
                        "nothing to install: every installed harness is wired".into()
                    }
                    Pending::Uninstall { .. } => "ai-memory is not installed".into(),
                });
            }
            Ok(steps) => self.pending = Some((what, steps)),
            Err(e) => {
                self.pending = None;
                self.error = Some(e);
            }
        }
    }

    /// Runs the pending steps on a thread.
    fn start(&mut self) {
        let Some((_, steps)) = self.pending.take() else {
            return;
        };
        let (tx, rx) = channel();
        self.log.clear();
        self.scroll = 0;
        std::thread::spawn(move || {
            let home = home();
            let total = steps.len();
            for (i, step) in steps.iter().enumerate() {
                let _ = tx.send(JobEvent::Line(format!(
                    "[{}/{total}] {}",
                    i + 1,
                    step.title
                )));
                let sender = tx.clone();
                if let Err(e) = install::run(step, &home, &mut |line| {
                    let _ = sender.send(JobEvent::Line(format!("  {line}")));
                }) {
                    let _ = tx.send(JobEvent::Finished(Err(format!("{}: {e}", step.title))));
                    return;
                }
            }
            let _ = tx.send(JobEvent::Finished(Ok(())));
        });
        self.job = Some(rx);
    }

    /// Takes what the thread sent; when it is done, reads the state again.
    /// True when something changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(JobEvent::Line(l)) => {
                    self.log.push(l);
                    changed = true;
                }
                Ok(JobEvent::Finished(result)) => {
                    self.finish(result);
                    return true;
                }
                Err(TryRecvError::Empty) => return changed,
                Err(TryRecvError::Disconnected) => {
                    self.finish(Err("the install stopped unexpectedly".into()));
                    return true;
                }
            }
        }
    }

    fn finish(&mut self, result: Result<(), String>) {
        self.job = None;
        self.state = State::detect();
        match result {
            Ok(()) => {
                self.log.push("done".into());
                for n in crate::memory::cli::notes(&self.state) {
                    self.log.push(format!("• {n}"));
                }
            }
            Err(e) => {
                self.log.push(format!("stopped: {e}"));
                self.error = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use crate::memory::{Binary, Wiring};
    use crossterm::event::KeyModifiers;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn wired(mcp: bool, hooks: bool) -> State {
        State {
            binary: Some(Binary {
                path: "/x/ai-memory".into(),
                version: None,
            }),
            initialized: true,
            server: true,
            launchd: true,
            harnesses: vec![Wiring {
                harness: Harness::Claude,
                cli: true,
                mcp,
                hooks,
            }],
        }
    }

    #[test]
    fn i_lists_the_missing_steps_and_esc_drops_them() {
        let mut v = MemoryView::with_state(wired(true, false));
        v.handle_key(&key('i'));
        let (what, steps) = v.pending.as_ref().unwrap();
        assert_eq!(*what, Pending::Install);
        assert_eq!(steps.len(), 1);
        assert!(
            steps[0]
                .command
                .contains("install-hooks --agent claude-code")
        );
        v.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(v.pending.is_none());
        assert_eq!(v.handle_key(&key('q')), Outcome::Close);
    }

    #[test]
    fn a_question_goes_to_a_wired_harness_and_tab_changes_it() {
        let mut state = wired(true, true);
        state.harnesses.push(Wiring {
            harness: Harness::Codex,
            cli: true,
            mcp: true,
            hooks: true,
        });
        let mut v = MemoryView::with_state(state);
        v.dir = PathBuf::from("/work/proj");
        v.handle_key(&key('a'));
        assert_eq!(
            v.input.as_ref().unwrap().kind,
            InputKind::Ask(Harness::Claude)
        );
        v.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            v.input.as_ref().unwrap().kind,
            InputKind::Ask(Harness::Codex)
        );
        for c in "how do we deploy? q".chars() {
            v.handle_key(&key(c));
        }
        match v.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Outcome::Ask {
                harness,
                dir,
                message,
            } => {
                assert_eq!(harness, Harness::Codex);
                assert_eq!(dir, PathBuf::from("/work/proj"));
                assert!(
                    message.ends_with("Question: how do we deploy? q"),
                    "q is text"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(v.input.is_none());
    }

    #[test]
    fn reading_needs_a_running_server_and_w_opens_the_web_view() {
        let mut down = wired(true, true);
        down.server = false;
        let mut v = MemoryView::with_state(down);
        v.handle_key(&key('/'));
        assert!(v.input.is_none());
        assert!(v.error.as_deref().unwrap().contains("not running"));
        let mut v = MemoryView::with_state(wired(true, true));
        v.handle_key(&key('/'));
        assert_eq!(v.input.as_ref().unwrap().kind, InputKind::Search);
        v.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        match v.handle_key(&key('w')) {
            Outcome::OpenUrl(u) => assert!(u.ends_with("/web")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_complete_setup_has_nothing_to_install() {
        let mut v = MemoryView::with_state(wired(true, true));
        v.handle_key(&key('i'));
        assert!(v.pending.is_none());
        assert!(
            v.error
                .as_deref()
                .unwrap()
                .starts_with("nothing to install")
        );
    }

    #[test]
    fn p_switches_an_uninstall_to_one_that_deletes_the_memory() {
        let mut v = MemoryView::with_state(wired(true, true));
        v.handle_key(&key('u'));
        assert!(matches!(
            v.pending,
            Some((Pending::Uninstall { purge: false }, _))
        ));
        v.handle_key(&key('p'));
        let (what, steps) = v.pending.as_ref().unwrap();
        assert_eq!(*what, Pending::Uninstall { purge: true });
        assert!(steps[0].command.contains("--purge-data"));
    }
}
