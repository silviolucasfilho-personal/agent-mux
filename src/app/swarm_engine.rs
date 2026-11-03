//! The TUI side of an orchestrated swarm (schema v21,
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`): the
//! headless planner run, its answer turned into a board, the dispatcher's
//! pass on every tick ([`crate::bus::swarm::engine::next_actions`]) and the
//! operator's verbs. A swarm is dispatched by the TUI that started it
//! (`swarms.owner_run`); another TUI adopts it once that one is gone.

use crate::app::{App, Notice};
use crate::bus::TaskIsolation;
use crate::bus::control::{ControlRequest, ControlResponse};
use crate::bus::swarm::engine::{self, Action, Worker, WorkerState};
use crate::bus::swarm::plan::CheckEnv;
use crate::bus::swarm::prompt::{self, AgentInfo, PlannerContext, Replan, ReplanTask};
use crate::bus::swarm::record::{self, NewSwarm, SwarmRun, SwarmStatus, SwarmVerb};
use crate::harness::{Harness, LaunchOptions, compose};
use crate::status::Status;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The planner skill (`skills/swarm-plan`).
pub const SWARM_PLAN_SKILL: &str = "swarm-plan";

/// How often the dispatcher looks at the board. Spawns are rare and a
/// pass reads a few rows, so a second is plenty.
const ENGINE_EVERY: Duration = Duration::from_secs(1);

/// How often a TUI checks whether another TUI that owns a swarm is gone.
const ADOPT_EVERY: Duration = Duration::from_secs(10);

/// What a new swarm starts with (`ControlRequest::StartSwarm`, the screen's
/// launch form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSwarm {
    pub workspace: PathBuf,
    pub goal: String,
    pub planner: Option<String>,
    pub max_workers: usize,
    pub isolation: TaskIsolation,
    pub gate: bool,
    pub check: Option<String>,
    /// The workers' pool, comma separated (`agy,codex`), the first for
    /// tasks that name none; `None`: the coordinator's harness alone.
    pub workers: Option<String>,
}

impl App {
    /// Harnesses this machine can launch: a profile's command or the
    /// harness's own name resolves.
    pub fn installed_harnesses(&self) -> Vec<Harness> {
        Harness::ALL
            .into_iter()
            .filter(|h| {
                self.profiles.iter().any(|p| {
                    Harness::detect(&p.command) == Some(*h)
                        && crate::session::command_available(&p.command)
                }) || crate::session::command_available(h.as_str())
            })
            .collect()
    }

    /// The coordinator's harness when none is named: Claude when it is
    /// installed, else the selected session's, else the first installed
    /// (layout spec §4.1: agy's agents lost their tools in the first run).
    pub fn default_swarm_harness(&self) -> Harness {
        let installed = self.installed_harnesses();
        if installed.contains(&Harness::Claude) {
            return Harness::Claude;
        }
        self.sessions
            .get(self.selected)
            .and_then(|s| Harness::detect(&s.profile.command))
            .or_else(|| installed.into_iter().next())
            .unwrap_or(Harness::Claude)
    }

    /// What `plan::check` needs for this workspace.
    pub fn swarm_check_env(&self, workspace: &Path, isolation: TaskIsolation) -> CheckEnv {
        let catalog = crate::agents::Catalog::load(&self.library_root(), Some(workspace));
        CheckEnv {
            harnesses: self
                .installed_harnesses()
                .iter()
                .map(|h| h.as_str().to_string())
                .collect(),
            agents: catalog.entries.iter().map(|e| e.name.clone()).collect(),
            writing_agents: crate::bus::swarm::plan::writing_agents(&catalog),
            git_repo: crate::loops::worktree::is_git_repo(workspace),
            default_isolation: isolation,
            pool: Vec::new(),
        }
    }

    /// Starts a swarm: its row, then the planner run.
    pub fn start_orchestrated_swarm(&mut self, req: StartSwarm) -> Result<SwarmRun, String> {
        if req.goal.trim().is_empty() {
            return Err("the goal is empty".into());
        }
        if !req.workspace.is_dir() {
            return Err(format!(
                "workspace {} does not exist",
                req.workspace.display()
            ));
        }
        let planner = match req.planner.as_deref().map(str::trim) {
            Some(h) if !h.is_empty() => h.parse::<Harness>()?,
            _ => self.default_swarm_harness(),
        };
        // the workers' pool: "agy,codex", the first for tasks that name none
        let mut worker_pool = Vec::new();
        for h in req
            .workers
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            worker_pool.push(h.parse::<Harness>()?.as_str().to_string());
        }
        let db = self
            .trace_db_path
            .clone()
            .ok_or("a swarm needs the trace store (tracing is off)")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let swarm = record::create_swarm(
            &conn,
            Some(&db),
            &NewSwarm {
                workspace: req.workspace.to_string_lossy().into_owned(),
                goal: req.goal.clone(),
                planner_harness: planner.as_str().to_string(),
                max_workers: req.max_workers.clamp(1, 8),
                isolation: req.isolation,
                gate: req.gate,
                check_cmd: req.check.clone(),
                owner_run: Some(self.app_run_id.clone()),
                worker_pool,
            },
            crate::tracing::store::now_ns(),
        )?;
        if let Err(e) = self.start_swarm_coordinator(&swarm, None, None) {
            let _ = record::set_plan_result(
                &conn,
                Some(&db),
                &swarm.id,
                None,
                Some(&format!("the coordinator did not start: {e}")),
                SwarmStatus::Review,
                crate::tracing::store::now_ns(),
            );
            return Err(e);
        }
        Ok(swarm)
    }

    /// Writes the coordinator's planner context (`$AGENT_MUX_SWARM_PLAN`):
    /// the goal, the workspace's inventory, the harnesses and agents, and on
    /// a re-plan the board and the operator's notes.
    fn write_planner_context(
        &self,
        swarm: &SwarmRun,
        replan: Option<Replan>,
    ) -> Result<PathBuf, String> {
        let workspace = PathBuf::from(&swarm.workspace);
        let catalog = crate::agents::Catalog::load(&self.library_root(), Some(&workspace));
        let pool = self
            .trace_db_path
            .as_deref()
            .and_then(|db| crate::tracing::store::open_ro(db).ok())
            .map(|c| record::worker_pool(&c, swarm))
            .unwrap_or_else(|| vec![swarm.planner_harness.clone()]);
        let ctx = PlannerContext {
            goal: swarm.goal.clone(),
            workspace: crate::workflows::planner::inventory(&workspace),
            workspace_path: swarm.workspace.clone(),
            git_repo: crate::loops::worktree::is_git_repo(&workspace),
            // the workers' pool, the default first: a task names one of them
            harnesses: pool.clone(),
            default_harness: pool[0].clone(),
            agents: catalog
                .entries
                .iter()
                .filter_map(|e| {
                    Some(AgentInfo {
                        name: e.name.clone(),
                        description: e.spec.as_ref()?.description.clone(),
                    })
                })
                .collect(),
            max_workers: swarm.max_workers,
            default_isolation: swarm.isolation.as_str().to_string(),
            replan,
        };
        let dir = self.workflows_runtime_dir().join("swarms").join(&swarm.id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("planner context: {e}"))?;
        let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        let path = dir.join(format!("plan-{n}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&ctx).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("planner context: {e}"))?;
        Ok(path)
    }

    /// The board as the coordinator's context sees it on a re-plan or a
    /// restart.
    fn replan_board(&self, swarm: &SwarmRun, notes: &str) -> Replan {
        let board = self
            .trace_db_path
            .as_deref()
            .and_then(|db| crate::tracing::store::open_ro(db).ok())
            .and_then(|c| record::board(&c, &swarm.id).ok())
            .unwrap_or_default();
        Replan {
            notes: notes.trim().to_string(),
            board: board
                .iter()
                .map(|t| ReplanTask {
                    key: t.ext.plan_key.clone(),
                    title: t.task.title.clone(),
                    status: t.task.status.as_str().to_string(),
                    started: t.ext.attempts > 0,
                })
                .collect(),
        }
    }

    /// Starts the swarm's coordinator agent: an interactive session in the
    /// workspace on the swarm's coordinator harness, whose opening prompt is
    /// its role and the planner context (§3 of the layout spec). It plans by
    /// calling `agent_mux_swarm_plan`, then advises: agent-mux keeps
    /// dispatching whatever it does. `restart` is the board summary for a
    /// coordinator that replaces one that exited.
    pub fn start_swarm_coordinator(
        &mut self,
        swarm: &SwarmRun,
        replan: Option<Replan>,
        restart: Option<String>,
    ) -> Result<String, String> {
        let harness: Harness = swarm.planner_harness.parse()?;
        let workspace = PathBuf::from(&swarm.workspace);
        let def = self
            .find_skill(SWARM_PLAN_SKILL)
            .cloned()
            .ok_or("the swarm-plan skill is not available")?;
        crate::skill::install::install(&def, harness, &self.skill_home(), false)
            .map_err(|e| format!("installing swarm-plan for {}: {e}", harness.as_str()))?;
        let ctx_path = self.write_planner_context(swarm, replan)?;
        let prompts = crate::prompts::Prompts::current();
        let opening = prompt::coordinator_prompt(
            swarm,
            &crate::skill::render::invocation(SWARM_PLAN_SKILL, harness),
            &ctx_path.display().to_string(),
            &prompts.swarm_plan,
            restart.as_deref(),
        );
        let mut profile = self
            .profiles
            .iter()
            .find(|p| Harness::detect(&p.command) == Some(harness))
            .cloned()
            .unwrap_or_else(|| crate::config::Profile {
                name: harness.as_str().to_string(),
                command: harness.as_str().to_string(),
                args: Vec::new(),
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: Some(self.bypass_approvals_default),
            });
        profile.name = format!("{} · coordinator", harness.as_str());
        let options = LaunchOptions {
            model: profile.model.clone(),
            bypass_approvals: profile
                .bypass_approvals
                .unwrap_or(self.bypass_approvals_default),
            ..Default::default()
        };
        if !options.is_empty() {
            profile.args = compose(&profile.args, &options.render(harness));
        }
        let opening_args = harness.opening_args(opening);
        profile.args.extend(opening_args.iter().cloned());
        let env: Vec<(String, String)> = vec![
            (
                "AGENT_MUX_SWARM_PLAN".into(),
                ctx_path.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_SWARM_ID".into(), swarm.id.clone()),
            ("AGENT_MUX_WORKSPACE".into(), swarm.workspace.clone()),
        ];
        let id = self.next_id;
        let mut session = self
            .spawn_traced_full(
                id,
                profile,
                workspace,
                &env,
                Some(SWARM_PLAN_SKILL),
                None,
                None,
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        session.opening_args = opening_args;
        session.group = Some(crate::tree::GroupRef {
            kind: crate::tree::GroupKind::Bus,
            id: swarm.id.clone(),
            title: "swarm".into(),
            detail: crate::bus::short_id(&swarm.id).to_string(),
            label: "coordinator".into(),
        });
        let launch = session
            .trace
            .as_ref()
            .map(|t| t.launch_id.clone())
            .unwrap_or_else(|| format!("session-{id}"));
        self.next_id += 1;
        self.sessions.push(session);
        let _ = self.save_active_sessions();
        if let Some(db) = self.trace_db_path.clone()
            && let Ok(conn) = crate::tracing::store::open_aux(&db)
        {
            let now = crate::tracing::store::now_ns();
            let _ = record::set_coordinator_launch(&conn, Some(&db), &swarm.id, &launch, now);
        }
        Ok(launch)
    }

    /// The coordinator's launch while its session is open here.
    fn live_coordinator(&self, swarm: &SwarmRun) -> Option<String> {
        let launch = swarm.planner_launch.clone()?;
        self.sessions
            .iter()
            .any(|s| {
                s.trace.as_ref().is_some_and(|t| t.launch_id == launch)
                    && !matches!(s.status(Instant::now()), Status::Exited(_))
            })
            .then_some(launch)
    }

    /// Writes to the coordinator on the bus and wakes it with `wake`.
    fn tell_coordinator(&mut self, swarm: &SwarmRun, topic: &str, body: &str, wake: &str) -> bool {
        let Some(launch) = self.live_coordinator(swarm) else {
            return false;
        };
        if let Some(db) = self.trace_db_path.clone()
            && let Ok(conn) = crate::tracing::store::open_aux(&db)
        {
            let _ = crate::bus::store::send_message(
                &conn,
                Some(&db),
                &swarm.workspace,
                "user",
                Some(&launch),
                topic,
                body,
                None,
                crate::tracing::store::now_ns(),
            );
        }
        let _ = self.handle_bus_control(
            ControlRequest::WakeSession {
                launch_id: launch,
                prompt: wake.to_string(),
            },
            Instant::now(),
        );
        true
    }

    /// Carries out an operator verb on `workspace`'s only live swarm (see
    /// [`App::swarm_action_on`] to name one).
    pub fn swarm_action(
        &mut self,
        workspace: &Path,
        verb: SwarmVerb,
        task: Option<&str>,
        notes: Option<&str>,
    ) -> Result<String, String> {
        self.swarm_action_on(None, workspace, verb, task, notes)
    }

    /// Carries out an operator verb on the swarm `swarm_id` names, else the
    /// one the task id is on, else `workspace`'s only live swarm.
    pub fn swarm_action_on(
        &mut self,
        swarm_id: Option<&str>,
        workspace: &Path,
        verb: SwarmVerb,
        task: Option<&str>,
        notes: Option<&str>,
    ) -> Result<String, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let norm = crate::bus::store::normalize_workspace(workspace);
        let of_task = match (swarm_id, task) {
            (None, Some(t)) => record::swarm_of_task(&conn, t).ok().flatten(),
            _ => None,
        };
        let swarm = match of_task {
            Some(s) => s,
            None => record::resolve_swarm(&conn, &norm, swarm_id)?,
        };
        if swarm.ended_ns.is_some() {
            return Err(format!("{} has ended", swarm.id));
        }
        let now = crate::tracing::store::now_ns();
        match verb {
            SwarmVerb::Close => {
                let (stopped, records) = self.stop_swarm_run(&swarm);
                let (cancelled, released) = records?;
                Ok(format!(
                    "closed: stopped {stopped} session(s), cancelled {cancelled} task(s), released {released} lease(s)"
                ))
            }
            SwarmVerb::Replan => {
                if !matches!(swarm.status, SwarmStatus::Review | SwarmStatus::Planning) {
                    return Err(format!(
                        "re-plan from review only (the swarm is {})",
                        swarm.status.as_str()
                    ));
                }
                let notes = notes.unwrap_or_default().trim().to_string();
                let told = self.tell_coordinator(
                    &swarm,
                    "plan-changes",
                    &format!("The operator sent the plan back: {notes}"),
                    &format!(
                        "[agent-mux swarm] The operator sent your plan back: {notes} Revise it and call agent_mux_swarm_plan again."
                    ),
                );
                let _ = record::set_planner(
                    &conn,
                    Some(&db),
                    &swarm.id,
                    swarm.planner_launch.as_deref(),
                    now,
                );
                if told {
                    Ok("sent back to the coordinator".into())
                } else {
                    let replan = self.replan_board(&swarm, &notes);
                    self.start_swarm_coordinator(&swarm, Some(replan), None)?;
                    Ok("the coordinator had exited: a new one re-plans".into())
                }
            }
            SwarmVerb::Restart => {
                if self.live_coordinator(&swarm).is_some() {
                    return Err("the coordinator is still running: x stops it first".into());
                }
                let summary = record::board(&conn, &swarm.id)
                    .unwrap_or_default()
                    .iter()
                    .map(|t| {
                        format!(
                            "- {} ({}): {}",
                            t.ext.plan_key,
                            t.task.status.as_str(),
                            t.task.title
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let replan = self.replan_board(&swarm, "");
                self.start_swarm_coordinator(&swarm, Some(replan), Some(summary))?;
                Ok("a new coordinator started".into())
            }
            SwarmVerb::Approve => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let done = record::apply(&conn, Some(&db), &swarm, verb, task, &env, now)?;
                self.tell_coordinator(
                    &swarm,
                    "plan-approved",
                    "The operator approved the plan. agent-mux starts the workers now.",
                    "[agent-mux swarm] The operator approved your plan: agent-mux starts the workers. Wait for questions and events; do not start workers yourself.",
                );
                self.last_swarm_engine_pass = None;
                Ok(done)
            }
            _ => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let done = record::apply(&conn, Some(&db), &swarm, verb, task, &env, now)?;
                self.last_swarm_engine_pass = None; // act on it at the next tick
                Ok(done)
            }
        }
    }

    /// `StartSwarm` and `SwarmAction` from the control socket.
    pub(crate) fn handle_swarm_control(&mut self, req: ControlRequest) -> ControlResponse {
        let ok = |detail: String, launch: Option<String>| ControlResponse {
            ok: true,
            launch_id: launch,
            session_id: None,
            harness: None,
            worktree: None,
            branch: None,
            detail: Some(detail),
            error: None,
        };
        match req {
            ControlRequest::StartSwarm {
                workspace,
                goal,
                planner,
                max_workers,
                isolation,
                gate,
                check,
                workers,
            } => match self.start_orchestrated_swarm(StartSwarm {
                workspace: PathBuf::from(workspace),
                goal,
                planner,
                max_workers,
                isolation,
                gate: gate.unwrap_or(true),
                check,
                workers,
            }) {
                Ok(s) => ok(
                    format!("{} started: {} is planning", s.id, s.planner_harness),
                    None,
                ),
                Err(e) => ControlResponse::err(e),
            },
            ControlRequest::SwarmAction {
                workspace,
                verb,
                task,
                notes,
                swarm,
            } => match self.swarm_action_on(
                swarm.as_deref(),
                Path::new(&workspace),
                verb,
                task.as_deref(),
                notes.as_deref(),
            ) {
                Ok(d) => ok(d, None),
                Err(e) => ControlResponse::err(e),
            },
            _ => ControlResponse::err("not a swarm request"),
        }
    }

    /// The dispatcher's pass: for each live swarm this TUI owns (or adopts
    /// from a TUI that is gone), gather the board and the workers, ask
    /// `engine::next_actions` and carry them out.
    pub fn swarm_engine_pass(&mut self, now: Instant) {
        if self
            .last_swarm_engine_pass
            .is_some_and(|t| now.saturating_duration_since(t) < ENGINE_EVERY)
        {
            return;
        }
        self.last_swarm_engine_pass = Some(now);
        let Some(db) = self.trace_db_path.clone() else {
            return;
        };
        let Ok(conn) = crate::tracing::store::open_aux(&db) else {
            return;
        };
        let Ok(swarms) = record::live_swarms(&conn) else {
            return;
        };
        self.swarm_needs = 0;
        if swarms.is_empty() {
            return;
        }
        let mut live_runs: Option<HashSet<String>> = None;
        for swarm in swarms {
            if swarm.owner_run.as_deref() != Some(self.app_run_id.as_str()) {
                if live_runs.is_none() {
                    if self
                        .last_swarm_adopt_check
                        .is_some_and(|t| now.saturating_duration_since(t) < ADOPT_EVERY)
                    {
                        continue;
                    }
                    self.last_swarm_adopt_check = Some(now);
                    let root = crate::tracing::analysis::default_snapshot_dir();
                    live_runs = Some(
                        crate::tracing::analysis::read_snapshots(
                            &root,
                            crate::tracing::store::now_ns(),
                        )
                        .map(|v| v.into_iter().map(|s| s.run_id).collect())
                        .unwrap_or_default(),
                    );
                }
                let runs = live_runs.as_ref().expect("filled above");
                if swarm.owner_run.as_ref().is_some_and(|o| runs.contains(o)) {
                    continue; // another TUI dispatches it
                }
                if record::set_owner(&conn, &swarm.id, &self.app_run_id).is_err() {
                    continue;
                }
            }
            self.dispatch_swarm(&conn, &db, &swarm, now);
        }
    }

    /// Live sessions as the dispatcher sees them, by launch id.
    fn swarm_workers(&self, now: Instant) -> HashMap<String, Worker> {
        self.sessions
            .iter()
            .filter_map(|s| {
                let launch = s.trace.as_ref()?.launch_id.clone();
                let state = match s.status(now) {
                    Status::Working => WorkerState::Working,
                    Status::Idle => WorkerState::Idle,
                    Status::NeedsAttention => WorkerState::Prompt,
                    Status::Exited(_) => WorkerState::Exited,
                };
                let quiet_ns = s
                    .quiet_for(now)
                    .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                Some((launch, Worker { state, quiet_ns }))
            })
            .collect()
    }

    fn dispatch_swarm(
        &mut self,
        conn: &rusqlite::Connection,
        db: &Path,
        swarm: &SwarmRun,
        now: Instant,
    ) {
        let Ok(board) = record::board(conn, &swarm.id) else {
            return;
        };
        let workers = self.swarm_workers(now);
        let messages = crate::bus::store::list_workspace_messages(conn, &swarm.workspace, 500)
            .unwrap_or_default();
        let asking: HashSet<String> = crate::bus::swarm::open_questions(&messages)
            .into_iter()
            .map(|m| m.from_launch.clone())
            .collect();
        let actions = engine::next_actions(&engine::Input {
            swarm,
            board: &board,
            workers: &workers,
            asking: &asking,
            now_ns: crate::tracing::store::now_ns(),
        });
        let data = crate::app::swarm_board::SwarmData {
            run: swarm.clone(),
            board: board.clone(),
            messages: messages.clone(),
            asking: asking.clone(),
            problems: Default::default(),
            needs: Vec::new(),
        };
        self.swarm_needs +=
            crate::app::swarm_board::needs_for(&data, &workers, crate::tracing::store::now_ns())
                .len();
        for action in actions {
            let now_ns = crate::tracing::store::now_ns();
            match action {
                Action::Spawn { task_id } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    let resp = self.handle_bus_control(
                        ControlRequest::SpawnWorker {
                            parent_launch: None,
                            workspace: swarm.workspace.clone(),
                            task_id: t.task.id.clone(),
                            title: t.task.title.clone(),
                            instructions: t.task.instructions.clone(),
                            harness: t.task.target_harness.clone(),
                            agent: t.task.target_agent.clone(),
                            isolation: t.task.isolation,
                            opening_prompt: Some(prompt::worker_prompt(swarm, t, &board)),
                        },
                        now,
                    );
                    let _ = record::mark_spawned(conn, &t.task.id);
                    if !resp.ok {
                        let why = resp.error.unwrap_or_else(|| "unknown error".into());
                        let _ = crate::bus::store::update_task(
                            conn,
                            Some(db),
                            &t.task.id,
                            &record::creator(&swarm.id),
                            crate::bus::TaskStatus::Failed,
                            None,
                            Some(format!("could not start a worker: {why}")),
                            None,
                            None,
                            now_ns,
                        );
                    }
                }
                Action::Nudge { task_id, launch_id } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    let _ = self.handle_bus_control(
                        ControlRequest::WakeSession {
                            launch_id,
                            prompt: prompt::nudge_prompt(t, &swarm.workspace),
                        },
                        now,
                    );
                    let _ = record::mark_nudged(conn, &task_id, now_ns);
                }
                Action::RunCheck => {
                    let Some(cmd) = swarm.check_cmd.clone() else {
                        continue;
                    };
                    let marker = crate::bus::swarm::check::CheckResult::started(now_ns);
                    let _ = record::set_check_result(
                        conn,
                        Some(db),
                        &swarm.id,
                        &marker.to_json(),
                        now_ns,
                    );
                    let (db, id, ws) =
                        (db.to_path_buf(), swarm.id.clone(), swarm.workspace.clone());
                    std::thread::spawn(move || {
                        let r = crate::bus::swarm::check::run(
                            Path::new(&ws),
                            &cmd,
                            crate::bus::swarm::check::CHECK_TIMEOUT,
                            now_ns,
                        );
                        if let Ok(conn) = crate::tracing::store::open_aux(&db) {
                            let _ = record::set_check_result(
                                &conn,
                                Some(&db),
                                &id,
                                &r.to_json(),
                                crate::tracing::store::now_ns(),
                            );
                        }
                    });
                    self.notice = Some(Notice::info(format!(
                        "swarm: running the check `{}`",
                        crate::app::workflows::short_task(
                            swarm.check_cmd.as_deref().unwrap_or_default()
                        )
                    )));
                }
                Action::Advance(status) => {
                    let _ = record::set_status(conn, Some(db), &swarm.id, status, now_ns);
                    if status == SwarmStatus::Finished {
                        let check = match (&swarm.check_cmd, &swarm.check_result) {
                            (Some(cmd), Some(raw)) => {
                                crate::bus::swarm::check::CheckResult::parse(raw)
                                    .map(|r| format!(" · {}", r.summary(cmd)))
                                    .unwrap_or_default()
                            }
                            _ => String::new(),
                        };
                        self.notice = Some(Notice::info(format!(
                            "swarm finished: {}{check} · y on the Swarm screen closes it",
                            crate::app::workflows::short_task(&swarm.goal)
                        )));
                    }
                }
            }
        }
        self.tell_coordinator_events(conn, db, swarm, &board, &workers);
    }

    /// Tells the coordinator what happened on the board since it was last
    /// told: tasks done, failed or cancelled, and proposals. One bus message
    /// and one wake for all of them, at most every [`EVENTS_EVERY_NS`], and
    /// only while it is idle and the swarm is not paused (decision 2).
    fn tell_coordinator_events(
        &mut self,
        conn: &rusqlite::Connection,
        db: &Path,
        swarm: &SwarmRun,
        board: &[record::BoardTask],
        workers: &HashMap<String, Worker>,
    ) {
        // a finished swarm's last events are told too, before it closes
        if !matches!(
            swarm.status,
            SwarmStatus::Running | SwarmStatus::Integrating | SwarmStatus::Finished
        ) {
            return;
        }
        let Some(c) = record::coordinator(conn, &swarm.id).ok().flatten() else {
            return;
        };
        let Some(launch) = c.launch.clone() else {
            return;
        };
        if !workers
            .get(&launch)
            .is_some_and(|w| w.state == WorkerState::Idle)
        {
            return;
        }
        let now_ns = crate::tracing::store::now_ns();
        if c.last_wake_ns.is_some_and(|t| now_ns - t < EVENTS_EVERY_NS) {
            return;
        }
        let mut events = Vec::new();
        let mut newest = c.events_seen_ns;
        for t in board {
            let key = &t.ext.plan_key;
            if let Some(ended) = t.task.ended_ns
                && ended > c.events_seen_ns
            {
                let what = match (&t.task.result, &t.task.error) {
                    (_, Some(e)) => {
                        format!("{} {}: {}", key, t.task.status.as_str(), first_line(e))
                    }
                    (Some(serde_json::Value::String(r)), _) => {
                        format!("{} {}: {}", key, t.task.status.as_str(), first_line(r))
                    }
                    _ => format!("{} {}", key, t.task.status.as_str()),
                };
                events.push(what);
                newest = newest.max(ended);
            }
            if t.ext.proposed && t.task.created_ns > c.events_seen_ns {
                events.push(format!("{key} proposed by a worker: {}", t.task.title));
                newest = newest.max(t.task.created_ns);
            }
        }
        if events.is_empty() {
            return;
        }
        let body = format!(
            "{} event(s) in swarm {}:\n{}",
            events.len(),
            swarm.id,
            events
                .iter()
                .map(|e| format!("- {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let _ = crate::bus::store::send_message(
            conn,
            Some(db),
            &swarm.workspace,
            "agent-mux",
            Some(&launch),
            "swarm-events",
            &body,
            None,
            now_ns,
        );
        let _ = self.handle_bus_control(
            ControlRequest::WakeSession {
                launch_id: launch,
                prompt: format!(
                    "[agent-mux swarm] {} event(s) in your swarm: read them with agent_mux_recv_messages. Advise the operator or propose tasks if something should change; do not start workers.",
                    events.len()
                ),
            },
            Instant::now(),
        );
        let _ = record::mark_events_told(conn, &swarm.id, newest, now_ns);
    }
}

/// At most one wake of the coordinator for events per minute (decision 2).
pub const EVENTS_EVERY_NS: i64 = 60 * 1_000_000_000;

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(160)
        .collect()
}
