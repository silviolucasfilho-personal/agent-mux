//! Saved on close (`memory::save`): the App's half. A session that
//! closes, and every session still open when agent-mux quits, hands one
//! background save its conversation.

use super::App;

impl App {
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
        let source = match self.session_source(s) {
            Ok(src) => src,
            Err(e) => {
                crate::memory::log::append(
                    &memory.dir,
                    crate::harness::Harness::detect(&s.profile.command)
                        .map(|h| h.as_str())
                        .unwrap_or("-"),
                    s.trace.as_ref().map(|t| t.launch_id.as_str()),
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
        let settings = crate::memory::Settings::from_config(self.memory.as_ref());
        let job = crate::memory::save::Job {
            command: s.profile.command.clone(),
            model: crate::memory::save::model_arg(&s.profile.args),
            workspace: s.dir.clone(),
            conversation: source.conversation,
            transcript,
            memory: memory.dir.clone(),
            user_dir: Some(memory.user_dir.clone()),
            launch: s.trace.as_ref().map(|t| t.launch_id.clone()),
            index_lines: settings.index_lines,
        };
        if job.unchanged() {
            return false;
        }
        // only the real binary runs the job; a test's App leaves it written
        let exe = crate::tracing::hooks::register::current_exe().filter(|e| {
            e.file_stem()
                .is_some_and(|s| s.to_string_lossy() == "agent-mux")
        });
        match job.write().and_then(|path| match exe {
            Some(exe) => crate::memory::save::spawn(&exe, &path),
            None => Ok(()),
        }) {
            Ok(()) => true,
            Err(e) => {
                crate::memory::log::append(
                    &memory.dir,
                    job.harness().map(|h| h.as_str()).unwrap_or("-"),
                    job.launch.as_deref(),
                    "save-failed",
                    &e.to_string(),
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
