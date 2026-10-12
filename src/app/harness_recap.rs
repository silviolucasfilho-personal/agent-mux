//! Harness recap queries run off the UI thread; drawing reads cached cards only.
//! A recap is loaded once each time a harness row is selected (and again when
//! its sessions change), not on a timer while the row stays selected.

use super::{App, Mode, agents_list::AgentKind};
use crate::events::AppEvent;
use crate::tracing::analysis::model::{RuntimeState, SessionCard};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    profile: usize,
    name: String,
    db: Option<PathBuf>,
    // Stable session and launch identities, rather than mutable vector positions.
    sessions: Vec<(usize, Option<String>)>,
}

#[derive(Debug, Clone)]
pub struct HarnessRecap {
    key: Key,
    cards: Vec<Option<SessionCard>>,
    warning: Option<String>,
}

#[derive(Debug, Default)]
pub struct RecapState {
    /// The last recap of each harness, by profile.
    cached: std::collections::HashMap<usize, HarnessRecap>,
    pending: Option<Key>,
    /// The harness row the cursor was on at the last check: arriving on
    /// one loads its recap again.
    shown: Option<usize>,
}

fn runtime_state(s: crate::status::Status) -> RuntimeState {
    use crate::status::Status;
    match s {
        Status::Working => RuntimeState::Working,
        Status::Idle => RuntimeState::Idle,
        Status::NeedsAttention => RuntimeState::WaitingForUser,
        Status::Exited(_) => RuntimeState::Exited,
    }
}

impl App {
    pub fn harness_sessions(&self, p: usize) -> Vec<usize> {
        let Some(profile) = self.profiles.get(p) else {
            return Vec::new();
        };
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.profile.name == profile.name && s.group.is_none() && s.skill_id.is_none()
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn recap_key(&self, p: usize) -> Option<Key> {
        Some(Key {
            profile: p,
            name: self.profiles.get(p)?.name.clone(),
            db: self.trace_db_path.clone(),
            sessions: self
                .harness_sessions(p)
                .into_iter()
                .map(|i| {
                    let s = &self.sessions[i];
                    (s.id, s.trace.as_ref().map(|t| t.launch_id.clone()))
                })
                .collect(),
        })
    }

    /// Rendering never opens the store or schedules work.
    pub fn harness_recap(&self, p: usize, _now: Instant) -> Vec<(usize, Option<SessionCard>)> {
        let cached = self
            .harness_recap
            .cached
            .get(&p)
            .filter(|r| Some(&r.key) == self.recap_key(p).as_ref());
        self.harness_sessions(p)
            .into_iter()
            .enumerate()
            .map(|(n, i)| (i, cached.and_then(|r| r.cards.get(n)).cloned().flatten()))
            .collect()
    }

    pub fn harness_recap_loading(&self, p: usize) -> bool {
        let Some(key) = self.recap_key(p) else {
            return false;
        };
        if key.db.is_none() || !key.sessions.iter().any(|(_, launch)| launch.is_some()) {
            return false;
        }
        self.harness_recap.pending.as_ref() == Some(&key)
            || !self
                .harness_recap
                .cached
                .get(&p)
                .is_some_and(|r| r.key == key)
    }

    pub fn harness_recap_warning(&self, p: usize) -> Option<&str> {
        self.harness_recap
            .cached
            .get(&p)
            .filter(|r| Some(&r.key) == self.recap_key(p).as_ref())
            .and_then(|r| r.warning.as_deref())
    }

    pub fn refresh_harness_recap_if_needed(&mut self, now: Instant) {
        if !matches!(self.mode, Mode::Main) || self.sidebar_hidden || !self.list_focused() {
            return;
        }
        let Some(AgentKind::Harness(p)) = self.agent_row() else {
            self.harness_recap.shown = None;
            return;
        };
        let Some(key) = self.recap_key(p) else { return };
        // a load in flight for this harness answers its arrival; one for
        // another leaves the arrival to be answered when it lands
        if let Some(pending) = &self.harness_recap.pending {
            if pending.profile == p {
                self.harness_recap.shown = Some(p);
            }
            return;
        }
        let arrived = self.harness_recap.shown.replace(p) != Some(p);
        if !arrived
            && self
                .harness_recap
                .cached
                .get(&p)
                .is_some_and(|r| r.key == key)
        {
            return;
        }
        let inputs: Vec<_> = self
            .harness_sessions(p)
            .into_iter()
            .map(|i| {
                let s = &self.sessions[i];
                let provider = crate::harness::Harness::detect(&s.profile.command)
                    .and_then(|h| crate::handoff::provider_of(h.as_str()))
                    .map(|p| p.as_str().to_string());
                (s.dir.clone(), provider, runtime_state(s.status(now)))
            })
            .collect();
        if key.db.is_none() || !key.sessions.iter().any(|(_, launch)| launch.is_some()) {
            self.harness_recap.cached.insert(
                p,
                HarnessRecap {
                    cards: vec![None; key.sessions.len()],
                    key,
                    warning: None,
                },
            );
            return;
        }
        self.harness_recap.pending = Some(key.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut warning = None;
            let conn =
                key.db
                    .as_deref()
                    .and_then(|path| match crate::tracing::store::open_ro(path) {
                        Ok(conn) => Some(conn),
                        Err(e) => {
                            warning = Some(e);
                            None
                        }
                    });
            let cards = key
                .sessions
                .iter()
                .zip(inputs)
                .map(|((_, launch), (cwd, provider, state))| {
                    let (conn, launch) = (conn.as_ref()?, launch.as_ref()?);
                    match crate::tracing::analysis::query::launch_card(
                        conn,
                        launch,
                        provider.as_deref(),
                        &cwd,
                        state,
                    ) {
                        Ok(c) => (c.completed_turns + c.open_turns > 0).then_some(c),
                        Err(e) => {
                            warning = Some(e.to_string());
                            None
                        }
                    }
                })
                .collect();
            let _ = tx.blocking_send(AppEvent::HarnessRecapUpdated(HarnessRecap {
                key,
                cards,
                warning,
            }));
        });
    }

    pub fn handle_harness_recap_updated(&mut self, recap: HarnessRecap) {
        if self.harness_recap.pending.as_ref() != Some(&recap.key) {
            return;
        }
        self.harness_recap.pending = None;
        if self.agent_row() == Some(AgentKind::Harness(recap.key.profile))
            && self.recap_key(recap.key.profile).as_ref() == Some(&recap.key)
        {
            self.harness_recap.cached.insert(recap.key.profile, recap);
        }
        self.refresh_harness_recap_if_needed(Instant::now());
    }
}
