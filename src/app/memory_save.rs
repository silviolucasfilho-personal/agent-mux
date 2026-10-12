//! Saved on close (`memory::save`): the App's half. A session that
//! closes, and every session still open when agent-mux quits, hands one
//! background save its conversation. Each session also leaves a pending
//! marker from spawn until its save starts, which the next agent-mux
//! turns into a save if this one ends without starting it.

use super::App;
use crate::memory::save::{self, Job, Pending};
use crate::session::Session;

/// The marker's name: the launch, else this process and the session.
fn pending_key(s: &Session) -> String {
    match &s.trace {
        Some(t) => t.launch_id.clone(),
        None => format!("{}-{}", std::process::id(), s.id),
    }
}

impl App {
    /// The save of session `s`, as far as it is known; `conversation`
    /// is empty until the session has one.
    fn save_job(&self, s: &Session, m: &crate::memory::launch::Launch) -> Job {
        let settings = crate::memory::Settings::from_config(self.memory.as_ref());
        Job {
            command: s.profile.command.clone(),
            model: save::model_arg(&s.profile.args),
            workspace: s.dir.clone(),
            conversation: s.conversation.clone().unwrap_or_default(),
            transcript: None,
            memory: m.dir.clone(),
            user_dir: Some(m.user_dir.clone()),
            launch: s.trace.as_ref().map(|t| t.launch_id.clone()),
            index_lines: settings.index_lines,
            reason: None,
            brief: None,
        }
    }

    /// At spawn: the marker a crash leaves behind.
    pub(super) fn record_pending_save(&self, s: &Session) {
        let Some(m) = &s.memory_save else {
            return;
        };
        let started_at = s
            .started_at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let p = Pending {
            pid: std::process::id(),
            started_at,
            job: self.save_job(s, m),
        };
        let _ = save::write_pending(&self.library_root(), &pending_key(s), &p);
    }

    /// Starts the save on close of session `id`, once. Quiet: a session
    /// with no conversation yet has nothing to save, and a failure is
    /// logged in the memory's `log.md`, not shown.
    pub fn save_memory_on_close(&mut self, id: usize) -> bool {
        let Some(i) = self.session_index(id) else {
            return false;
        };
        let Some(memory) = self.sessions[i].memory_save.take() else {
            return false;
        };
        let s = &self.sessions[i];
        save::remove_pending(&self.library_root(), &pending_key(s));
        let harness = crate::harness::Harness::detect(&s.profile.command)
            .map(|h| h.as_str())
            .unwrap_or("-");
        let launch = s.trace.as_ref().map(|t| t.launch_id.clone());
        let source = match self.session_source(s) {
            Ok(src) => src,
            Err(e) => {
                crate::memory::log::append(
                    &memory.dir,
                    harness,
                    launch.as_deref(),
                    "closed",
                    &format!("not saved: {e}"),
                );
                return false;
            }
        };
        let transcript = source
            .transcript
            .clone()
            .or_else(|| crate::handoff::transcript_by_id(source.provider, &source.conversation));
        let job = Job {
            conversation: source.conversation,
            transcript,
            ..self.save_job(s, &memory)
        };
        match save::start(&job, save::agent_mux_exe().as_deref()) {
            Ok(started) => started,
            Err(e) => {
                crate::memory::log::append(
                    &memory.dir,
                    harness,
                    launch.as_deref(),
                    "save-failed",
                    &e,
                );
                false
            }
        }
    }

    /// At quit: a save for every session that has not had one. Returns
    /// how many were started.
    pub fn save_memory_of_all(&mut self) -> usize {
        let ids: Vec<usize> = self
            .sessions
            .iter()
            .filter(|s| s.memory_save.is_some())
            .map(|s| s.id)
            .collect();
        ids.into_iter()
            .filter(|id| self.save_memory_on_close(*id))
            .count()
    }
}
