//! The Memory screen (F1 → "Open memory"): ai-memory's setup at a glance
//! (`crate::memory::State`: the binary, the server, each harness's MCP
//! entry and hooks) and the install flow (`crate::memory::install`) run
//! from there. `i` shows the steps that are missing and `y` runs them;
//! `u` removes ai-memory's wiring and stops its server (`p` also deletes
//! the memory). Steps run on a thread; their output is the log below the
//! state, and the state is read again when they finish.

use crate::memory::install::{self, Options, Step};
use crate::memory::{State, home};
use crossterm::event::{KeyCode, KeyEvent};
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

#[derive(Debug)]
pub struct MemoryView {
    pub state: State,
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
}

impl MemoryView {
    pub fn open() -> MemoryView {
        MemoryView::with_state(State::detect())
    }

    pub fn with_state(state: State) -> MemoryView {
        MemoryView {
            state,
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

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
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
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('i') if !self.running() => self.ask(Pending::Install),
            KeyCode::Char('u') if !self.running() => self.ask(Pending::Uninstall { purge: false }),
            KeyCode::Char('r') if !self.running() => {
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
