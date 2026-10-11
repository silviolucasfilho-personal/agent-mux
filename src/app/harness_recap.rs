//! The recap a harness row shows: one card per session it runs, built
//! from the trace store (`tracing::analysis::query::launch_card`) and
//! cached for a few seconds, since the card is drawn every frame.

use super::App;
use crate::tracing::analysis::model::{RuntimeState, SessionCard};
use std::time::{Duration, Instant};

/// How long a recap is reused before the store is read again.
const FRESH: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct HarnessRecap {
    profile: usize,
    sessions: Vec<usize>,
    at: Instant,
    cards: Vec<Option<SessionCard>>,
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
    /// The sessions a harness row owns: its profile's own, not a loop's,
    /// a flow's or a skill's (the rows nested under it).
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

    /// One card per session of harness row `p`, in `harness_sessions`
    /// order; `None` for a session the store knows nothing of (tracing
    /// off, or no turn yet).
    pub fn harness_recap(&self, p: usize, now: Instant) -> Vec<(usize, Option<SessionCard>)> {
        let sessions = self.harness_sessions(p);
        if let Some(r) = self.harness_recap.borrow().as_ref()
            && r.profile == p
            && r.sessions == sessions
            && now.saturating_duration_since(r.at) < FRESH
        {
            return sessions.into_iter().zip(r.cards.clone()).collect();
        }
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let cards: Vec<Option<SessionCard>> = sessions
            .iter()
            .map(|&i| {
                let s = &self.sessions[i];
                let (conn, trace) = (conn.as_ref()?, s.trace.as_ref()?);
                let provider = crate::harness::Harness::detect(&s.profile.command)
                    .and_then(|h| crate::handoff::provider_of(h.as_str()))
                    .map(|p| p.as_str());
                crate::tracing::analysis::query::launch_card(
                    conn,
                    &trace.launch_id,
                    provider,
                    &s.dir,
                    runtime_state(s.status(now)),
                )
                .ok()
                .filter(|c| c.completed_turns + c.open_turns > 0)
            })
            .collect();
        *self.harness_recap.borrow_mut() = Some(HarnessRecap {
            profile: p,
            sessions: sessions.clone(),
            at: now,
            cards: cards.clone(),
        });
        sessions.into_iter().zip(cards).collect()
    }
}
