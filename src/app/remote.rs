//! The `App` side of the remote control: it applies one command per turn
//! of the main loop, and taps three points to publish what changed.
//!
//! Everything here runs on the main loop, so it may touch sessions freely;
//! nothing in `crate::remote` may. The taps are deliberately few: output,
//! exit, and a fingerprint check on the tick. A session list that is a
//! second stale is harmless; a terminal stream with a hole is not, which is
//! why output is the one thing pushed synchronously.

use super::{App, NoticeLevel};
use crate::remote::bridge::{ClientId, Outbound, RemoteCommand, RemoteHub};
use crate::remote::protocol::{
    KIND_OUTPUT, KIND_RESYNC, LoopInfo, ProfileInfo, RemoteSession, ServerMsg, SkillInfo,
    WorkflowInfo, encode_binary,
};
use std::sync::Arc;
use std::time::Instant;

/// What the session list looked like last time it was published. Cheap to
/// compare, so the list can be pushed the moment anything changes without
/// instrumenting every call site that adds or removes a session.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionsFingerprint {
    rows: Vec<(usize, &'static str)>,
    selected: Option<usize>,
    pane: (u16, u16),
    attached: bool,
}

impl App {
    fn hub(&self) -> Option<Arc<RemoteHub>> {
        self.remote.clone()
    }

    /// One command from a remote client.
    pub fn handle_remote(&mut self, cmd: RemoteCommand, now: Instant) {
        let Some(hub) = self.hub() else { return };
        match cmd {
            RemoteCommand::Hello { client, focus } => {
                let focus = focus
                    .filter(|id| self.session_index(*id).is_some())
                    .or_else(|| self.sessions.get(self.selected).map(|s| s.id));
                hub.set_focus(client, focus);
                hub.send_to(
                    client,
                    Outbound::text(&ServerMsg::Hello {
                        version: crate::build_info::short(),
                        run_id: self.app_run_id.clone(),
                        cols: self.pane_size.1,
                        rows: self.pane_size.0,
                        allow_kill: hub.settings.allow_kill,
                        allow_launch: hub.settings.allow_launch,
                        client: client.0,
                    }),
                );
                hub.send_to(client, Outbound::text(&self.remote_sessions_msg(now)));
                hub.send_to(client, Outbound::text(&self.remote_catalog_msg()));
                if let Some(id) = focus {
                    self.remote_send_resync(&hub, client, id);
                }
            }
            RemoteCommand::Goodbye { client } => hub.unregister(client),
            RemoteCommand::Select { client, session } => {
                hub.set_focus(client, Some(session));
                self.remote_send_resync(&hub, client, session);
            }
            RemoteCommand::Resync { client, session } => {
                self.remote_send_resync(&hub, client, session)
            }
            RemoteCommand::Catalog { client } => {
                hub.send_to(client, Outbound::text(&self.remote_catalog_msg()));
            }
            RemoteCommand::Input {
                client,
                session,
                bytes,
            } => self.remote_write(&hub, client, session, &bytes),
            RemoteCommand::Key {
                client,
                session,
                key,
            } => {
                // The one key table in `crate::keys`, with DECCKM read off
                // the same vt100 screen the client is mirroring.
                let app_cursor = self.remote_application_cursor(session);
                if let Some(bytes) =
                    crate::keys::encode_key_with_mode(&key.to_key_event(), app_cursor)
                {
                    self.remote_write(&hub, client, session, &bytes);
                }
            }
            RemoteCommand::Paste {
                client,
                session,
                text,
            } => {
                let bracketed = self
                    .session_index(session)
                    .and_then(|i| self.sessions.get(i))
                    .is_some_and(|s| s.parser.screen().bracketed_paste());
                let bytes = super::paste_bytes(&text, bracketed);
                self.remote_write(&hub, client, session, &bytes);
            }
            RemoteCommand::Launch {
                client,
                req_id,
                profile,
                dir,
            } => {
                let allowed = hub.settings.allow_launch;
                self.remote_guarded(&hub, client, req_id, allowed, "launching", |app| {
                    app.remote_launch(&profile, &dir).map(Outcome::session)
                });
            }
            RemoteCommand::Kill {
                client,
                req_id,
                session,
            } => {
                let allowed = hub.settings.allow_kill;
                self.remote_guarded(&hub, client, req_id, allowed, "killing", |app| {
                    app.remote_kill(session).map(|()| Outcome::default())
                });
            }
            RemoteCommand::LaunchSkill {
                client,
                req_id,
                skill,
                harness,
                dir,
            } => {
                let allowed = hub.settings.allow_launch;
                self.remote_guarded(&hub, client, req_id, allowed, "launching", |app| {
                    app.remote_launch_skill(&skill, harness.as_deref(), dir.as_deref())
                        .map(Outcome::session)
                });
            }
            RemoteCommand::StartLoop {
                client,
                req_id,
                loop_id,
            } => {
                let allowed = hub.settings.allow_launch;
                self.remote_guarded(&hub, client, req_id, allowed, "launching", |app| match app
                    .start_loop_run(&loop_id)
                {
                    Some(run) => Ok(Outcome::run(run)),
                    None => Err(format!("loop '{loop_id}' did not start")),
                });
            }
            RemoteCommand::StartWorkflow {
                client,
                req_id,
                workflow,
                workspace,
                harness,
                args,
            } => {
                let allowed = hub.settings.allow_launch;
                self.remote_guarded(&hub, client, req_id, allowed, "launching", |app| {
                    app.remote_start_workflow(&workflow, &workspace, harness.as_deref(), args)
                        .map(Outcome::run)
                });
            }
        }
        self.remote_publish_sessions(now);
    }

    fn remote_application_cursor(&self, session: usize) -> bool {
        self.session_index(session)
            .and_then(|i| self.sessions.get(i))
            .is_some_and(|s| s.parser.screen().application_cursor())
    }

    fn remote_write(&mut self, hub: &RemoteHub, client: ClientId, session: usize, bytes: &[u8]) {
        if self.session_index(session).is_none() {
            return self.remote_fail(hub, client, None, "no such session");
        }
        // A remote keystroke must not move the desktop's own view, so this
        // deliberately skips the scroll-snap `forward_bytes` does.
        let saved = self.notice.take();
        let ok = self.forward_bytes_to(session, bytes);
        let produced = std::mem::replace(&mut self.notice, saved);
        if !ok {
            let text = produced
                .map(|n| n.text)
                .unwrap_or_else(|| "write failed".into());
            self.remote_fail(hub, client, None, &text);
        }
    }

    fn remote_fail(&self, hub: &RemoteHub, client: ClientId, req_id: Option<String>, error: &str) {
        hub.send_to(
            client,
            Outbound::text(&ServerMsg::Result {
                id: req_id,
                ok: false,
                s: None,
                run: None,
                error: Some(error.to_string()),
            }),
        );
    }

    /// Runs a command behind its `[remote] allow_*` gate and answers with a
    /// `result`. The App's own notice is captured so the client sees the
    /// same reason the desktop would have shown.
    fn remote_guarded(
        &mut self,
        hub: &RemoteHub,
        client: ClientId,
        req_id: Option<String>,
        allowed: bool,
        what: &str,
        f: impl FnOnce(&mut App) -> Result<Outcome, String>,
    ) {
        if !allowed {
            return self.remote_fail(
                hub,
                client,
                req_id,
                &format!("{what} is disabled on this server"),
            );
        }
        let saved = self.notice.take();
        let outcome = f(self);
        let produced = std::mem::replace(&mut self.notice, saved);
        let msg = match outcome {
            Ok(out) => ServerMsg::Result {
                id: req_id,
                ok: true,
                s: out.session,
                run: out.run,
                error: None,
            },
            Err(e) => {
                let detail = produced
                    .filter(|n| matches!(n.level, NoticeLevel::Error))
                    .map(|n| n.text);
                ServerMsg::Result {
                    id: req_id,
                    ok: false,
                    s: None,
                    run: None,
                    error: Some(detail.unwrap_or(e)),
                }
            }
        };
        hub.send_to(client, Outbound::text(&msg));
    }

    fn remote_launch(&mut self, profile_name: &str, dir: &str) -> Result<usize, String> {
        // The profile's own `bypass_approvals` applies, exactly as it does
        // for the headless runner; the remote adds no permission of its own.
        let profile = self
            .profiles
            .iter()
            .find(|p| p.name == profile_name)
            .cloned()
            .ok_or_else(|| format!("no profile named '{profile_name}'"))?;
        let dir = super::resolve_working_dir(dir);
        let idx = self.launch(profile, dir).map_err(|e| e.to_string())?;
        self.sessions
            .get(idx)
            .map(|s| s.id)
            .ok_or_else(|| "session vanished after launch".into())
    }

    fn remote_kill(&mut self, session: usize) -> Result<(), String> {
        let idx = self
            .session_index(session)
            .ok_or_else(|| "no such session".to_string())?;
        if let Some(s) = self.sessions.get_mut(idx) {
            s.kill();
        }
        let _ = self.save_active_sessions();
        Ok(())
    }

    fn remote_launch_skill(
        &mut self,
        skill_id: &str,
        harness: Option<&str>,
        dir: Option<&str>,
    ) -> Result<usize, String> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.id == skill_id)
            .ok_or_else(|| format!("no skill named '{skill_id}'"))?;
        let harness = match harness {
            Some(name) => {
                let h = crate::harness::Harness::detect(name)
                    .ok_or_else(|| format!("unknown harness '{name}'"))?;
                if !skill.harnesses.contains(&h) {
                    return Err(format!("{skill_id} does not run on {name}"));
                }
                h
            }
            None => skill.default_harness,
        };
        let dir = dir
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(super::resolve_working_dir);
        let idx = self
            .launch_skill_session(skill_id, harness, dir)
            .map_err(|e| e.to_string())?;
        self.sessions
            .get(idx)
            .map(|s| s.id)
            .ok_or_else(|| "session vanished after launch".into())
    }

    fn remote_start_workflow(
        &mut self,
        name: &str,
        workspace: &str,
        harness: Option<&str>,
        args: serde_json::Value,
    ) -> Result<String, String> {
        let entries = self.workflow_entries();
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("no workflow named '{name}'"))?;
        if !entry.valid() {
            return Err(format!("workflow '{name}' does not check out"));
        }
        // A step skill has to be installed for the harness that runs it, so
        // an unnamed harness takes the document's own first choice.
        let harness = match harness {
            Some(h) => crate::harness::Harness::detect(h)
                .ok_or_else(|| format!("unknown harness '{h}'"))?,
            None => first_allowed_harness(entry),
        };
        self.start_workflow_run(super::workflows::WorkflowRunRequest {
            name: entry.name.clone(),
            source: entry.source.label(),
            document: entry.text.clone(),
            workspace: super::resolve_working_dir(workspace),
            profile: None,
            harness,
            args,
            budget_tokens: None,
            usd_cap: None,
            isolation: None,
            resume_from: None,
        })
    }

    // ------------------------------------------------------------- publishing

    /// Terminal output for one session. Called from `handle_pty_output`
    /// *after* the parser has consumed the bytes, so a `key` command that
    /// arrives next reads the same DECCKM state the client is seeing.
    pub(crate) fn remote_tap_output(&mut self, id: usize, bytes: &[u8]) {
        let Some(hub) = self.hub() else { return };
        hub.push_output(id, Arc::from(encode_binary(KIND_OUTPUT, id, bytes)));
        self.remote_flush_resyncs(&hub);
    }

    /// A session ended: tell the clients, then refresh the list.
    pub(crate) fn remote_tap_exit(&mut self, id: usize, now: Instant) {
        let Some(hub) = self.hub() else { return };
        let code = self
            .session_index(id)
            .and_then(|i| self.sessions.get(i))
            .and_then(|s| match s.status(now) {
                crate::status::Status::Exited(code) => code,
                _ => None,
            });
        hub.broadcast(Outbound::text(&ServerMsg::Exit { s: id, code }));
        self.remote_publish_sessions(now);
    }

    /// Called every tick: publishes the session list when it changed.
    pub(crate) fn remote_tick(&mut self, now: Instant) {
        let Some(hub) = self.hub() else { return };
        self.remote_publish_sessions(now);
        self.remote_flush_resyncs(&hub);
    }

    fn remote_publish_sessions(&mut self, now: Instant) {
        let Some(hub) = self.hub() else { return };
        if hub.client_count() == 0 {
            return;
        }
        let fingerprint = self.remote_fingerprint(now);
        if self.remote_last_sessions.as_ref() == Some(&fingerprint) {
            return;
        }
        self.remote_last_sessions = Some(fingerprint);
        hub.broadcast(Outbound::text(&self.remote_sessions_msg(now)));
    }

    fn remote_fingerprint(&self, now: Instant) -> SessionsFingerprint {
        SessionsFingerprint {
            rows: self
                .sessions
                .iter()
                .map(|s| (s.id, status_tag(s.status(now))))
                .collect(),
            selected: self.sessions.get(self.selected).map(|s| s.id),
            pane: self.pane_size,
            attached: self.attached().is_some(),
        }
    }

    fn remote_flush_resyncs(&mut self, hub: &RemoteHub) {
        for (client, session) in hub.take_resync_requests() {
            self.remote_send_resync(hub, client, session);
        }
    }

    /// The whole screen as escape sequences, including the input modes, so
    /// the client can reset and redraw from one frame.
    fn remote_send_resync(&mut self, hub: &RemoteHub, client: ClientId, session: usize) {
        let Some(idx) = self.session_index(session) else {
            return self.remote_fail(hub, client, None, "no such session");
        };
        let Some(s) = self.sessions.get(idx) else {
            return;
        };
        let bytes = s.parser.screen().state_formatted();
        hub.deliver_resync(
            client,
            Arc::from(encode_binary(KIND_RESYNC, session, &bytes)),
        );
    }

    pub(crate) fn remote_sessions_msg(&self, now: Instant) -> ServerMsg {
        let live = self.collect_live_sessions(now);
        let sessions = live
            .into_iter()
            .map(|live| {
                let session = self.sessions.iter().find(|s| s.id == live.session_id);
                RemoteSession {
                    name: session
                        .map(|s| s.profile.name.clone())
                        .unwrap_or_else(|| format!("session {}", live.session_id)),
                    exit_code: session.and_then(|s| match s.status(now) {
                        crate::status::Status::Exited(code) => code,
                        _ => None,
                    }),
                    skill_id: session.and_then(|s| s.skill_id.clone()),
                    live,
                }
            })
            .collect();
        ServerMsg::Sessions {
            selected: self.sessions.get(self.selected).map(|s| s.id),
            attached: self.attached().is_some(),
            cols: self.pane_size.1,
            rows: self.pane_size.0,
            sessions,
        }
    }

    pub(crate) fn remote_catalog_msg(&self) -> ServerMsg {
        ServerMsg::Catalog {
            profiles: self
                .profiles
                .iter()
                .map(|p| ProfileInfo {
                    name: p.name.clone(),
                    command: p.command.clone(),
                    default_dir: p.default_dir.clone(),
                })
                .collect(),
            skills: self
                .skills
                .iter()
                .filter(|s| !s.hidden)
                .map(|s| SkillInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    harnesses: s.harnesses.iter().map(|h| h.as_str().to_string()).collect(),
                })
                .collect(),
            loops: self
                .loop_registry
                .loops
                .iter()
                .map(|l| LoopInfo {
                    id: l.id.clone(),
                    workspace: l.workspace.display().to_string(),
                    pattern: l.pattern.clone(),
                    enabled: l.enabled,
                    paused: l.paused(),
                })
                .collect(),
            workflows: self
                .workflow_entries()
                .iter()
                .map(|e| WorkflowInfo {
                    name: e.name.clone(),
                    source: e.source.label(),
                    valid: e.valid(),
                    args: e
                        .doc
                        .as_ref()
                        .map(|d| d.args.keys().cloned().collect())
                        .unwrap_or_default(),
                })
                .collect(),
        }
    }
}

/// What a `result` carries back: a new session id, a run id, or neither.
#[derive(Debug, Default)]
pub struct Outcome {
    pub session: Option<usize>,
    pub run: Option<String>,
}

impl Outcome {
    fn session(id: usize) -> Outcome {
        Outcome {
            session: Some(id),
            run: None,
        }
    }
    fn run(id: String) -> Outcome {
        Outcome {
            session: None,
            run: Some(id),
        }
    }
}

fn status_tag(status: crate::status::Status) -> &'static str {
    match status {
        crate::status::Status::Working => "working",
        crate::status::Status::Idle => "idle",
        crate::status::Status::NeedsAttention => "attention",
        crate::status::Status::Exited(_) => "exited",
    }
}

/// The first harness a workflow document allows, defaulting to Claude Code.
fn first_allowed_harness(entry: &crate::workflows::library::Entry) -> crate::harness::Harness {
    use crate::harness::Harness;
    const ORDER: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Antigravity];
    entry
        .doc
        .as_ref()
        .and_then(|d| ORDER.into_iter().find(|h| d.harness.allows(h.as_str())))
        .unwrap_or(Harness::Claude)
}
