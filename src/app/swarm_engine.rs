//! The TUI side of an orchestrated swarm (schema v21,
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`): the
//! headless planner run, its answer turned into a board, the dispatcher's
//! pass on every tick ([`crate::bus::swarm::engine::next_actions`]) and the
//! operator's verbs. A swarm is dispatched by the TUI that started it
//! (`swarms.owner_run`); another TUI adopts it once that one is gone.

use crate::app::workflows::LivePlan;
use crate::app::{App, Notice};
use crate::bus::TaskIsolation;
use crate::bus::control::{ControlRequest, ControlResponse};
use crate::bus::swarm::engine::{self, Action, Worker, WorkerState};
use crate::bus::swarm::plan::{self, CheckEnv};
use crate::bus::swarm::prompt::{self, AgentInfo, PlannerContext, Replan, ReplanTask};
use crate::bus::swarm::record::{self, NewSwarm, SwarmRun, SwarmStatus, SwarmVerb};
use crate::harness::{Harness, LaunchOptions, Resume, compose};
use crate::status::Status;
use crate::workflows::harness as wfh;
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

    /// The planner's harness when none is named: the selected session's,
    /// else the first installed (decision 4 of the spec).
    pub fn default_swarm_harness(&self) -> Harness {
        self.sessions
            .get(self.selected)
            .and_then(|s| Harness::detect(&s.profile.command))
            .or_else(|| self.installed_harnesses().into_iter().next())
            .unwrap_or(Harness::Claude)
    }

    /// What `plan::check` needs for this workspace.
    pub fn swarm_check_env(&self, workspace: &Path, isolation: TaskIsolation) -> CheckEnv {
        CheckEnv {
            harnesses: self
                .installed_harnesses()
                .iter()
                .map(|h| h.as_str().to_string())
                .collect(),
            agents: crate::agents::Catalog::load(&self.library_root(), Some(workspace))
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            git_repo: crate::loops::worktree::is_git_repo(workspace),
            default_isolation: isolation,
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
            },
            crate::tracing::store::now_ns(),
        )?;
        if let Err(e) = self.start_swarm_planner(&swarm, None) {
            let _ = record::set_plan_result(
                &conn,
                Some(&db),
                &swarm.id,
                None,
                Some(&format!("the planner did not start: {e}")),
                SwarmStatus::Review,
                crate::tracing::store::now_ns(),
            );
            return Err(e);
        }
        Ok(swarm)
    }

    /// Launches the headless planner on `swarm`'s goal (and, on a re-plan,
    /// the board and the operator's notes), the way `start_workflow_plan`
    /// launches the workflow planner.
    fn start_swarm_planner(
        &mut self,
        swarm: &SwarmRun,
        replan: Option<Replan>,
    ) -> Result<(), String> {
        let harness: Harness = swarm.planner_harness.parse()?;
        let workspace = PathBuf::from(&swarm.workspace);
        let def = self
            .find_skill(SWARM_PLAN_SKILL)
            .cloned()
            .ok_or("the swarm-plan skill is not available")?;
        crate::skill::install::install(&def, harness, &self.skill_home(), false)
            .map_err(|e| format!("installing swarm-plan for {}: {e}", harness.as_str()))?;
        let catalog = crate::agents::Catalog::load(&self.library_root(), Some(&workspace));
        let ctx = PlannerContext {
            goal: swarm.goal.clone(),
            workspace: crate::workflows::planner::inventory(&workspace),
            workspace_path: swarm.workspace.clone(),
            git_repo: crate::loops::worktree::is_git_repo(&workspace),
            harnesses: self
                .installed_harnesses()
                .iter()
                .map(|h| h.as_str().to_string())
                .collect(),
            default_harness: swarm.planner_harness.clone(),
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
        let ctx_path = dir.join(format!("plan-{n}.json"));
        std::fs::write(
            &ctx_path,
            serde_json::to_vec_pretty(&ctx).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("planner context: {e}"))?;
        let out_file = dir.join(format!("plan-{n}.last.md"));
        let mut profile = self.workflow_profile(None, harness);
        let prompts = crate::prompts::Prompts::current();
        let prompt_text = crate::prompts::render_workflow(
            &prompts.swarm_plan,
            &crate::skill::render::invocation(SWARM_PLAN_SKILL, harness),
            "swarm",
            "plan",
            "",
            &ctx_path.display().to_string(),
        );
        let options = LaunchOptions {
            model: profile.model.clone(),
            bypass_approvals: true,
            resume: Resume::Off,
            one_shot: Some(prompt_text),
        };
        let mut args = compose(&profile.args, &options.render(harness));
        let timeout_s = self.workflows.session_timeout_s;
        wfh::insert_before_prompt(
            &mut args,
            harness,
            wfh::extra_args(harness, None, timeout_s, &out_file),
        );
        if Harness::detect(&profile.command) != Some(harness) {
            profile.command = harness.as_str().to_string();
        }
        profile.args = args;
        profile.name = format!(
            "swarm plan ▸ {}",
            crate::app::workflows::short_task(&swarm.goal)
        );
        let env: Vec<(String, String)> = vec![
            (
                "AGENT_MUX_SWARM_PLAN".into(),
                ctx_path.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_WORKSPACE".into(), swarm.workspace.clone()),
        ];
        let launch = crate::workflows::WorkflowLaunch {
            run_id: swarm.id.clone(),
            workflow: "swarm".into(),
            step: "plan".into(),
            phase: "Plan".into(),
            agent: None,
        };
        let sid = self.next_id;
        let session = self
            .spawn_traced_full(
                sid,
                profile,
                workspace.clone(),
                &env,
                Some(SWARM_PLAN_SKILL),
                None,
                Some(launch),
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        self.next_id += 1;
        let launch_id = session.trace.as_ref().map(|t| t.launch_id.clone());
        self.sessions.push(session);
        self.live_workflow_plans.push(LivePlan {
            id: swarm.id.clone(),
            session_id: sid,
            task: swarm.goal.clone(),
            workspace,
            harness,
            profile: None,
            budget_tokens: None,
            started: Instant::now(),
            timeout: Duration::from_secs(timeout_s),
            exited_at: None,
            stdout: Vec::new(),
            out_file,
            launch_id: launch_id.clone(),
            auto_run: false,
            swarm: Some(swarm.id.clone()),
        });
        if let Some(db) = self.trace_db_path.as_deref()
            && let Ok(conn) = crate::tracing::store::open_aux(db)
        {
            let _ = record::set_planner(
                &conn,
                Some(db),
                &swarm.id,
                launch_id.as_deref(),
                crate::tracing::store::now_ns(),
            );
        }
        Ok(())
    }

    /// The planner answered: its plan goes on the board (the tasks that ran
    /// stay) and the swarm waits for review, or runs at once when the gate
    /// is off and the plan has no errors. A broken answer leaves the board
    /// as it was and says why.
    pub(crate) fn complete_swarm_plan(&mut self, swarm_id: &str, text: &str) {
        let Some(db) = self.trace_db_path.clone() else {
            return;
        };
        let Ok(conn) = crate::tracing::store::open_aux(&db) else {
            return;
        };
        let now = crate::tracing::store::now_ns();
        let Some(swarm) = record::get_swarm(&conn, swarm_id).ok().flatten() else {
            return;
        };
        if swarm.ended_ns.is_some() {
            return;
        }
        let parsed = plan::parse(text)
            .and_then(|p| record::replace_draft(&conn, Some(&db), &swarm, &p, now).map(|_| p));
        match parsed {
            Err(e) => {
                let _ = record::set_plan_result(
                    &conn,
                    Some(&db),
                    &swarm.id,
                    None,
                    Some(&e),
                    SwarmStatus::Review,
                    now,
                );
                self.notice = Some(Notice::error(format!(
                    "swarm planner: {e} · re-plan with `agent-mux swarm replan`"
                )));
            }
            Ok(p) => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let board = record::board(&conn, &swarm.id).unwrap_or_default();
                let problems = plan::check(&record::board_plan(&swarm, &board), &env);
                let next = if !swarm.gate && problems.is_ok() {
                    SwarmStatus::Running
                } else {
                    SwarmStatus::Review
                };
                let _ = record::set_plan_result(
                    &conn,
                    Some(&db),
                    &swarm.id,
                    Some(&p.summary),
                    None,
                    next,
                    now,
                );
                self.notice = Some(Notice::info(match next {
                    SwarmStatus::Running => {
                        format!("swarm: {} task(s) planned, starting", board.len())
                    }
                    _ => format!(
                        "swarm: {} task(s) planned{} · review it, then `agent-mux swarm approve`",
                        board.len(),
                        if problems.is_ok() {
                            String::new()
                        } else {
                            format!(", {} error(s) to fix", problems.errors.len())
                        }
                    ),
                }));
            }
        }
    }

    /// Carries out an operator verb on `workspace`'s swarm.
    pub fn swarm_action(
        &mut self,
        workspace: &Path,
        verb: SwarmVerb,
        task: Option<&str>,
        notes: Option<&str>,
    ) -> Result<String, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let norm = crate::bus::store::normalize_workspace(workspace);
        let swarm = record::live_swarm(&conn, &norm)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no swarm runs in {norm}"))?;
        let now = crate::tracing::store::now_ns();
        match verb {
            SwarmVerb::Close => {
                let (stopped, records) = self.stop_swarm(Path::new(&swarm.workspace));
                let (cancelled, released) = records?;
                Ok(format!(
                    "closed: stopped {stopped} session(s), cancelled {cancelled} task(s), released {released} lease(s)"
                ))
            }
            SwarmVerb::Replan => {
                if swarm.status != SwarmStatus::Review {
                    return Err(format!(
                        "re-plan from review only (the swarm is {})",
                        swarm.status.as_str()
                    ));
                }
                let board = record::board(&conn, &swarm.id).map_err(|e| e.to_string())?;
                let replan = Replan {
                    notes: notes.unwrap_or_default().trim().to_string(),
                    board: board
                        .iter()
                        .map(|t| ReplanTask {
                            key: t.ext.plan_key.clone(),
                            title: t.task.title.clone(),
                            status: t.task.status.as_str().to_string(),
                            started: t.ext.attempts > 0,
                        })
                        .collect(),
                };
                self.start_swarm_planner(&swarm, Some(replan))?;
                Ok("re-planning".into())
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
            } => match self.start_orchestrated_swarm(StartSwarm {
                workspace: PathBuf::from(workspace),
                goal,
                planner,
                max_workers,
                isolation,
                gate: gate.unwrap_or(true),
                check,
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
            } => match self.swarm_action(
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
    }
}
