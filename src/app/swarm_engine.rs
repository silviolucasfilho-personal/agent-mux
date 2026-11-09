//! The TUI side of an orchestrated swarm (schema v21,
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`): the
//! headless planner run, its answer turned into a board, the dispatcher's
//! pass on every tick ([`crate::bus::swarm::engine::next_actions`]) and the
//! operator's verbs. A swarm is dispatched by the TUI that started it
//! (`swarms.owner_run`); another TUI adopts it once that one is gone.

use crate::app::{App, Notice};
use crate::bus::TaskIsolation;
use crate::bus::control::{ControlRequest, ControlResponse};
use crate::bus::swarm::SwarmLaunch;
use crate::bus::swarm::engine::{self, Action, Worker, WorkerState};
use crate::bus::swarm::plan::CheckEnv;
use crate::bus::swarm::prompt::{
    self, AgentInfo, Limits, PlannerContext, Replan, ReplanTask, Tools,
};
use crate::bus::swarm::record::{
    self, Decider, LaunchedBy, NewSwarm, SwarmRun, SwarmStatus, SwarmVerb,
};
use crate::harness::{Harness, LaunchOptions, compose};
use crate::status::Status;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The coordinator's skill (`skills/swarm-coordinator`).
pub const SWARM_PLAN_SKILL: &str = "swarm-coordinator";

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
    /// How finished tasks are reviewed: `auto` (`None`), `off` or a harness.
    pub review: Option<String>,
    /// Whether merges wait for a check: `off` (`None`), `on` (the check
    /// command) or a command of its own.
    pub merge_gate: Option<String>,
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

    /// Starts a swarm: its row, then its coordinator session. With a goal the
    /// swarm is headless (the coordinator plans it without asking); without
    /// one the operator says it in the coordinator's chat (coordinator-first
    /// design §4.1, §10).
    pub fn start_orchestrated_swarm(&mut self, req: StartSwarm) -> Result<SwarmRun, String> {
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
        let review =
            crate::bus::swarm::review::ReviewSettings::parse(req.review.as_deref().unwrap_or(""))?;
        let merge_gate = crate::bus::swarm::gate::MergeGateSettings::parse(
            req.merge_gate.as_deref().unwrap_or(""),
        );
        merge_gate.check(req.check.as_deref())?;
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
                headless: !req.goal.trim().is_empty(),
                review,
                merge_gate,
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

    /// Writes the coordinator's context (`$AGENT_MUX_SWARM_CONTEXT`, also
    /// `$AGENT_MUX_SWARM_PLAN`): the goal when there is one, the workspace's
    /// inventory, the harnesses and agents, the limits the operator fixed,
    /// the tools, and on a re-plan the board and the operator's notes.
    fn write_planner_context(
        &self,
        swarm: &SwarmRun,
        replan: Option<Replan>,
    ) -> Result<PathBuf, String> {
        let workspace = PathBuf::from(&swarm.workspace);
        let catalog = crate::agents::Catalog::load(&self.library_root(), Some(&workspace));
        let ro = self
            .trace_db_path
            .as_deref()
            .and_then(|db| crate::tracing::store::open_ro(db).ok());
        let pool = ro
            .as_ref()
            .map(|c| record::worker_pool(c, swarm))
            .unwrap_or_else(|| vec![swarm.planner_harness.clone()]);
        let review = ro
            .as_ref()
            .map(|c| crate::bus::swarm::review::settings(c, &swarm.id))
            .unwrap_or_else(crate::bus::swarm::review::ReviewSettings::off);
        let merge_gate = ro
            .as_ref()
            .map(|c| crate::bus::swarm::gate::settings(c, &swarm.id))
            .unwrap_or_default();
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
            limits: Limits {
                max_workers: Some(swarm.max_workers),
                pool: Some(pool),
                default_isolation: Some(swarm.isolation.as_str().to_string()),
                check: swarm.check_cmd.clone(),
                gate: swarm.gate_mode.as_str().to_string(),
                headless: swarm.headless,
                review: review.as_choice(),
                review_step: review.describe(),
                merge_gate: merge_gate.as_choice(),
                merge_gate_step: merge_gate.describe(swarm),
            },
            tools: Tools {
                dispatch_grace_s: self.swarm_dispatch_grace_ns / 1_000_000_000,
                ..Tools::default()
            },
            screen: "The operator sees the Swarm screen: your plan read-only (it says to confirm here, in this chat), each worker's terminal, and what needs them. They can also type into any worker."
                .into(),
        };
        let dir = self.workflows_runtime_dir().join("swarms").join(&swarm.id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("planner context: {e}"))?;
        let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        let path = dir.join(format!("context-{n}.json"));
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
            .ok_or("the swarm-coordinator skill is not available")?;
        crate::skill::install::install(&def, harness, &self.skill_home(), false)
            .map_err(|e| format!("installing swarm-coordinator for {}: {e}", harness.as_str()))?;
        let ctx_path = self.write_planner_context(swarm, replan)?;
        let prompts = crate::prompts::Prompts::current();
        let opening = prompt::coordinator_prompt(
            swarm,
            &crate::skill::render::invocation(SWARM_PLAN_SKILL, harness),
            &ctx_path.display().to_string(),
            &prompts.swarm_coordinator,
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
                "AGENT_MUX_SWARM_CONTEXT".into(),
                ctx_path.to_string_lossy().into_owned(),
            ),
            // the name before v24, for shadowed skills that still read it
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
                Some(SwarmLaunch::coordinator(&swarm.id, None)),
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        session.opening_args = opening_args;
        session.group = Some(crate::tree::GroupRef {
            kind: crate::tree::GroupKind::Swarm,
            id: swarm.id.clone(),
            title: swarm_group_title(swarm),
            detail: crate::bus::short_id(&swarm.id).to_string(),
            label: "★ coordinator".into(),
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
            let _ = record::event(
                &conn,
                &swarm.id,
                None,
                if restart.is_some() {
                    "coordinator.restarted"
                } else {
                    "coordinator.started"
                },
                Some(&launch),
                Some(serde_json::json!({ "harness": harness.as_str() })),
                now,
            );
        }
        Ok(launch)
    }

    /// `Enter` on a worker whose session ended (coordinator-first §9.5):
    /// the same harness resumes the same conversation in a new session on
    /// the task, in the swarm's group, with the swarm's launch metadata. A
    /// task that was not settled is tracked again under the new launch.
    /// agy cannot resume a conversation; `r` retries the task instead.
    pub fn reconnect_swarm_worker(
        &mut self,
        task_id: &str,
        launch: &str,
    ) -> Result<String, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let task = crate::bus::store::get_task(&conn, task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no task {task_id}"))?;
        let swarm = record::swarm_of_task(&conn, task_id)
            .map_err(|e| e.to_string())?
            .ok_or("the task is on no swarm's board")?;
        let key = record::board(&conn, &swarm.id)
            .ok()
            .and_then(|b| {
                b.into_iter()
                    .find(|t| t.task.id == task_id)
                    .map(|t| t.ext.plan_key)
            })
            .unwrap_or_default();
        if self.sessions.iter().any(|s| {
            s.trace.as_ref().is_some_and(|t| t.launch_id == launch)
                && !matches!(s.status(Instant::now()), Status::Exited(_))
        }) {
            return Err("its session is still open: Enter types into it".into());
        }
        let (provider, session_id): (String, Option<String>) = conn
            .query_row(
                "SELECT l.provider, s.session_id FROM launches l
                 LEFT JOIN sessions s ON s.key = l.session_key WHERE l.id = ?1",
                rusqlite::params![launch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| format!("no launch row for {launch}"))?;
        let harness: Harness = provider.parse()?;
        let session_id = session_id.ok_or_else(|| {
            format!(
                "{} never recorded its conversation id: r retries the task instead",
                harness.as_str()
            )
        })?;
        if harness == Harness::Antigravity {
            return Err(
                "agy cannot resume a conversation: r retries the task (a new worker gets its card)"
                    .into(),
            );
        }
        let dir = task
            .worktree
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from(&swarm.workspace));
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
        profile.name = format!("{} · {}", harness.as_str(), key);
        let options = LaunchOptions {
            model: profile.model.clone(),
            bypass_approvals: profile
                .bypass_approvals
                .unwrap_or(self.bypass_approvals_default),
            resume: crate::harness::Resume::Id(session_id.clone()),
            ..Default::default()
        };
        profile.args = compose(&profile.args, &options.render(harness));
        let coordinator = record::coordinator(&conn, &swarm.id)
            .ok()
            .flatten()
            .and_then(|c| c.launch);
        let env = vec![("AGENT_MUX_TASK_ID".to_string(), task_id.to_string())];
        let id = self.next_id;
        let mut session = self
            .spawn_traced_full(
                id,
                profile,
                dir,
                &env,
                None,
                None,
                None,
                Some(SwarmLaunch::worker(&swarm.id, task_id, &key, coordinator)),
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        session.continued_from = Some(format!(
            "{} · {}",
            harness.as_str(),
            crate::bus::short_id(launch)
        ));
        session.group = Some(crate::tree::GroupRef {
            kind: crate::tree::GroupKind::Swarm,
            id: swarm.id.clone(),
            title: swarm_group_title(&swarm),
            detail: crate::bus::short_id(&swarm.id).to_string(),
            label: format!("{key} (reconnected)"),
        });
        let new_launch = session
            .trace
            .as_ref()
            .map(|t| t.launch_id.clone())
            .unwrap_or_else(|| format!("session-{id}"));
        self.next_id += 1;
        self.sessions.push(session);
        let _ = self.save_active_sessions();
        let now = crate::tracing::store::now_ns();
        if !task.status.is_terminal() {
            let _ = crate::bus::store::update_task(
                &conn,
                Some(&db),
                task_id,
                &new_launch,
                crate::bus::TaskStatus::Running,
                None,
                None,
                None,
                None,
                now,
            );
        }
        let _ = record::event(
            &conn,
            &swarm.id,
            Some(task_id),
            "task.reconnected",
            Some("operator"),
            Some(
                serde_json::json!({ "key": key, "from": launch, "launch": new_launch, "session": session_id }),
            ),
            now,
        );
        Ok(format!(
            "reconnected to `{key}`: {} resumes its conversation",
            harness.as_str()
        ))
    }

    /// A saved session of a swarm, at agent-mux's start: a worker is
    /// reconnected to its task ([`App::reconnect_swarm_worker`]), the
    /// swarm's coordinator to the swarm ([`App::reconnect_swarm_coordinator`]),
    /// each resuming its conversation under a new launch the swarm records.
    /// `Err` when it cannot be (the swarm ended, the task moved on, agy):
    /// the caller restores it as a plain session.
    pub fn reconnect_saved_swarm_session(
        &mut self,
        link: &SwarmLaunch,
        launch: &str,
    ) -> Result<String, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let swarm = record::get_swarm(&conn, &link.swarm_id)
            .map_err(|e| e.to_string())?
            .filter(|s| s.ended_ns.is_none())
            .ok_or("the swarm is closed")?;
        match link.role {
            crate::bus::swarm::SwarmRole::Coordinator => {
                if swarm.planner_launch.as_deref() != Some(launch) {
                    return Err("the swarm has another coordinator now".into());
                }
                self.reconnect_swarm_coordinator(&swarm, launch)
            }
            crate::bus::swarm::SwarmRole::Worker => {
                let task_id = link.task_id.as_deref().ok_or("no task")?;
                let task = crate::bus::store::get_task(&conn, task_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("no such task")?;
                if task.assigned_to.as_deref() != Some(launch) {
                    return Err("the task has another worker now".into());
                }
                self.reconnect_swarm_worker(task_id, launch)
            }
        }
    }

    /// The coordinator of `swarm`, whose session `launch` ended (an
    /// agent-mux restart): the same harness resumes the same conversation in
    /// a new session, in the swarm's group, recorded as the swarm's
    /// coordinator, so `★` on the Swarm screen is its terminal again.
    pub fn reconnect_swarm_coordinator(
        &mut self,
        swarm: &SwarmRun,
        launch: &str,
    ) -> Result<String, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let (provider, session_id): (String, Option<String>) = conn
            .query_row(
                "SELECT l.provider, s.session_id FROM launches l
                 LEFT JOIN sessions s ON s.key = l.session_key WHERE l.id = ?1",
                rusqlite::params![launch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| format!("no launch row for {launch}"))?;
        let harness: Harness = provider.parse()?;
        if harness == Harness::Antigravity {
            return Err("agy cannot resume a conversation".into());
        }
        let session_id = session_id.ok_or("its conversation id was never recorded")?;
        let ctx_path = self.write_planner_context(swarm, None)?;
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
            resume: crate::harness::Resume::Id(session_id.clone()),
            ..Default::default()
        };
        profile.args = compose(&profile.args, &options.render(harness));
        let env: Vec<(String, String)> = vec![
            (
                "AGENT_MUX_SWARM_CONTEXT".into(),
                ctx_path.to_string_lossy().into_owned(),
            ),
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
                PathBuf::from(&swarm.workspace),
                &env,
                Some(SWARM_PLAN_SKILL),
                None,
                None,
                Some(SwarmLaunch::coordinator(&swarm.id, None)),
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        session.group = Some(crate::tree::GroupRef {
            kind: crate::tree::GroupKind::Swarm,
            id: swarm.id.clone(),
            title: swarm_group_title(swarm),
            detail: crate::bus::short_id(&swarm.id).to_string(),
            label: "★ coordinator".into(),
        });
        let new_launch = session
            .trace
            .as_ref()
            .map(|t| t.launch_id.clone())
            .unwrap_or_else(|| format!("session-{id}"));
        self.next_id += 1;
        self.sessions.push(session);
        let now = crate::tracing::store::now_ns();
        let _ = record::set_coordinator_launch(&conn, Some(&db), &swarm.id, &new_launch, now);
        let _ = record::event(
            &conn,
            &swarm.id,
            None,
            "coordinator.reconnected",
            Some(&new_launch),
            Some(serde_json::json!({ "from": launch, "harness": harness.as_str() })),
            now,
        );
        Ok(format!(
            "the coordinator is back: {} resumes its conversation",
            harness.as_str()
        ))
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

    /// Carries out an operator verb from the screen on `workspace`'s only
    /// live swarm (see [`App::swarm_action_on`] to name one).
    pub fn swarm_action(
        &mut self,
        workspace: &Path,
        verb: SwarmVerb,
        task: Option<&str>,
        notes: Option<&str>,
    ) -> Result<String, String> {
        self.swarm_action_on(None, workspace, verb, task, notes, &Decider::TUI)
    }

    /// Carries out an operator verb on the swarm `swarm_id` names, else the
    /// one the task id is on, else `workspace`'s only live swarm. `who`
    /// says who relays it (the screen, the CLI, or the coordinator from its
    /// chat with the operator's words), for the decision's record.
    pub fn swarm_action_on(
        &mut self,
        swarm_id: Option<&str>,
        workspace: &Path,
        verb: SwarmVerb,
        task: Option<&str>,
        notes: Option<&str>,
        who: &Decider<'_>,
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
        if swarm.ended_ns.is_some() && verb != SwarmVerb::Delete {
            return Err(format!("{} has ended", swarm.id));
        }
        let now = crate::tracing::store::now_ns();
        if who.by == record::DecidedBy::Chat && verb.needs_operator_words() && who.said.is_none() {
            return Err(format!(
                "quote what the operator said in operator_said: `{}` happens only on their word",
                verb.as_str()
            ));
        }
        match verb {
            SwarmVerb::Delete => self.delete_swarm_run(&conn, &db, &swarm),
            SwarmVerb::Close => {
                let _ = record::decide(&conn, &swarm.id, None, verb, who, now);
                let (stopped, records) = self.stop_swarm_run(&swarm, true);
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
            SwarmVerb::Approve | SwarmVerb::Launch => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let was_review = swarm.status == SwarmStatus::Review;
                let done = record::apply(&conn, Some(&db), &swarm, verb, task, &env, who, now)?;
                // The coordinator hears of an approval it did not relay itself.
                if was_review && who.by != record::DecidedBy::Chat {
                    self.tell_coordinator(
                        &swarm,
                        "plan-approved",
                        "The operator approved the plan; the ready tasks' workers start now.",
                        "[agent-mux swarm] The operator approved your plan and its ready tasks start now. Follow them with agent_mux_swarm_wait; answer questions, and start the next tasks with agent_mux_swarm_launch as their dependencies finish.",
                    );
                }
                self.last_swarm_engine_pass = None;
                Ok(done)
            }
            SwarmVerb::Cancel => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let worker = record::board(&conn, &swarm.id)
                    .ok()
                    .and_then(|b| task.and_then(|t| record::find_task(&b, t).ok().cloned()))
                    .and_then(|t| t.task.assigned_to);
                let done = record::apply(&conn, Some(&db), &swarm, verb, task, &env, who, now)?;
                // a cancelled task's worker is stopped; a finished one stays
                let mut stopped = 0;
                if let Some(launch) = worker {
                    for s in self.sessions.iter_mut() {
                        if s.trace.as_ref().is_some_and(|t| t.launch_id == launch)
                            && !matches!(s.status(Instant::now()), Status::Exited(_))
                        {
                            s.kill();
                            stopped += 1;
                        }
                    }
                }
                self.last_swarm_engine_pass = None;
                Ok(if stopped > 0 {
                    format!("{done}; its worker was stopped")
                } else {
                    done
                })
            }
            _ => {
                let env = self.swarm_check_env(Path::new(&swarm.workspace), swarm.isolation);
                let done = record::apply_with(
                    &conn,
                    Some(&db),
                    &swarm,
                    verb,
                    task,
                    notes,
                    &env,
                    who,
                    now,
                )?;
                // the coordinator hears of a merge gate it did not relay itself
                if verb == SwarmVerb::MergeGate
                    && who.by != record::DecidedBy::Chat
                    && !done.starts_with("the merge gate is already")
                {
                    self.tell_coordinator(
                        &swarm,
                        "merge-gate",
                        &format!("The operator changed the merge gate: {done}"),
                        &format!(
                            "[agent-mux swarm] The operator changed the merge gate: {done}. Merges follow it from now on."
                        ),
                    );
                }
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
                review,
                merge_gate,
            } => match self.start_orchestrated_swarm(StartSwarm {
                workspace: PathBuf::from(workspace),
                goal,
                planner,
                max_workers,
                isolation,
                gate: gate.unwrap_or(true),
                check,
                workers,
                review,
                merge_gate,
            }) {
                Ok(s) => ok(
                    if s.headless {
                        format!("{} started: {} is planning", s.id, s.planner_harness)
                    } else {
                        format!(
                            "{} started: say the goal in the {} coordinator's chat",
                            s.id, s.planner_harness
                        )
                    },
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
                by,
                said,
                from,
            } => match self.swarm_action_on(
                swarm.as_deref(),
                Path::new(&workspace),
                verb,
                task.as_deref(),
                notes.as_deref(),
                &Decider {
                    by: by.unwrap_or(record::DecidedBy::Cli),
                    said: said.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                    launch: from.as_deref(),
                },
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
        self.swarm_needs_first = None;
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
        let review = crate::bus::swarm::review::settings(conn, &swarm.id);
        let merge_gate = crate::bus::swarm::gate::settings(conn, &swarm.id);
        let actions = engine::next_actions(&engine::Input {
            swarm,
            board: &board,
            workers: &workers,
            asking: &asking,
            now_ns: crate::tracing::store::now_ns(),
            dispatch_grace_ns: self.swarm_dispatch_grace_ns,
            review: review.enabled,
            merge_check: merge_gate.command_for(swarm),
        });
        let data = crate::app::swarm_board::SwarmData {
            run: swarm.clone(),
            board: board.clone(),
            messages: messages.clone(),
            asking: asking.clone(),
            problems: Default::default(),
            needs: Vec::new(),
            launch_decision: None,
            agents: Default::default(),
            events: Vec::new(),
            timeline: None,
            review: review.clone(),
            merge_gate: merge_gate.clone(),
        };
        let needs =
            crate::app::swarm_board::needs_for(&data, &workers, crate::tracing::store::now_ns())
                .len();
        self.swarm_needs += needs;
        // swarms come oldest first: the newest one that needs the operator
        // is the one the badge and F1 open
        if needs > 0 {
            self.swarm_needs_first = Some(swarm.id.clone());
        }
        for action in actions {
            let now_ns = crate::tracing::store::now_ns();
            match action {
                Action::Offer { task_id } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    let _ = record::offer_task(conn, &t.task.id, now_ns);
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        Some(&t.task.id),
                        "task.offered",
                        Some("engine"),
                        Some(serde_json::json!({
                            "key": t.ext.plan_key,
                            "grace_s": self.swarm_dispatch_grace_ns / 1_000_000_000,
                        })),
                        now_ns,
                    );
                }
                Action::Spawn { task_id, by } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    let coordinator = record::coordinator(conn, &swarm.id)
                        .ok()
                        .flatten()
                        .and_then(|c| c.launch);
                    if by == LaunchedBy::Dispatcher {
                        let _ = record::request_launch(conn, None, &t.task.id, by, now_ns);
                    }
                    // a review runs in the folder of the task it reviews
                    // (its worktree, else the workspace), with its own prompt
                    let reviewed = t
                        .ext
                        .review_of
                        .as_deref()
                        .and_then(|id| board.iter().find(|o| o.task.id == id));
                    let (spawn_ws, opening) = match reviewed {
                        Some(r) => {
                            let dir = r
                                .task
                                .worktree
                                .clone()
                                .filter(|w| Path::new(w).is_dir())
                                .unwrap_or_else(|| swarm.workspace.clone());
                            let base = r
                                .task
                                .branch
                                .as_ref()
                                .and_then(|_| workspace_branch(Path::new(&swarm.workspace)));
                            (
                                dir,
                                crate::bus::swarm::review::prompt(swarm, t, r, base.as_deref()),
                            )
                        }
                        None => (
                            swarm.workspace.clone(),
                            prompt::worker_prompt(swarm, t, &board),
                        ),
                    };
                    let resp = self.handle_bus_control(
                        ControlRequest::SpawnWorker {
                            parent_launch: coordinator.clone(),
                            workspace: spawn_ws,
                            task_id: t.task.id.clone(),
                            title: t.task.title.clone(),
                            instructions: t.task.instructions.clone(),
                            harness: t.task.target_harness.clone(),
                            agent: t.task.target_agent.clone(),
                            isolation: t.task.isolation,
                            opening_prompt: Some(opening),
                            swarm: Some(SwarmLaunch::worker(
                                &swarm.id,
                                &t.task.id,
                                &t.ext.plan_key,
                                coordinator,
                            )),
                        },
                        now,
                    );
                    let _ = record::mark_spawned(conn, &t.task.id);
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        Some(&t.task.id),
                        if resp.ok {
                            "task.spawned"
                        } else {
                            "task.spawn_failed"
                        },
                        Some("engine"),
                        Some(serde_json::json!({
                            "key": t.ext.plan_key,
                            "by": by.as_str(),
                            "launch": resp.launch_id,
                            "harness": resp.harness,
                            "error": resp.error,
                        })),
                        now_ns,
                    );
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
                Action::AddReview { task_id } => {
                    if let Err(e) = record::add_review(conn, Some(db), swarm, &task_id, now_ns) {
                        let _ = record::event(
                            conn,
                            &swarm.id,
                            Some(&task_id),
                            "review.add_failed",
                            Some("engine"),
                            Some(serde_json::json!({ "error": e })),
                            now_ns,
                        );
                        // once: the operator can ask again with `review`
                        let _ = crate::bus::swarm::review::set_skip(conn, &task_id, true, now_ns);
                    }
                }
                Action::Nudge { task_id, launch_id } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    let _ = self.handle_bus_control(
                        ControlRequest::WakeSession {
                            launch_id: launch_id.clone(),
                            prompt: prompt::nudge_prompt(t, &swarm.workspace),
                        },
                        now,
                    );
                    let _ = record::mark_nudged(conn, &task_id, now_ns);
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        Some(&task_id),
                        "task.nudged",
                        Some("engine"),
                        Some(serde_json::json!({ "key": t.ext.plan_key, "launch": launch_id })),
                        now_ns,
                    );
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
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        None,
                        "check.started",
                        Some("engine"),
                        Some(serde_json::json!({ "command": cmd })),
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
                            let end_ns = crate::tracing::store::now_ns();
                            let _ = record::set_check_result(
                                &conn,
                                Some(&db),
                                &id,
                                &r.to_json(),
                                end_ns,
                            );
                            let _ = record::event(
                                &conn,
                                &id,
                                None,
                                "check.done",
                                Some("engine"),
                                Some(serde_json::json!({ "exit": r.exit, "tail": r.tail })),
                                end_ns,
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
                Action::MarkLost { task_id } => {
                    let _ = crate::bus::swarm::recovery::mark_lost(conn, &task_id, now_ns);
                }
                Action::ClearLost { task_id } => {
                    let _ = crate::bus::swarm::recovery::clear_lost(conn, &task_id, now_ns);
                }
                Action::Recover { task_id, cause } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    match crate::bus::swarm::recovery::retry(
                        conn,
                        Some(db),
                        swarm,
                        t,
                        cause,
                        now_ns,
                    ) {
                        Ok(r) => {
                            self.notice = Some(Notice::info(format!(
                                "swarm: `{}` {} — started again on {}",
                                r.key,
                                cause.as_str(),
                                r.harness.as_deref().unwrap_or("the workers' harness")
                            )));
                        }
                        Err(e) => {
                            let _ = record::event(
                                conn,
                                &swarm.id,
                                Some(&task_id),
                                "task.retry_failed",
                                Some("engine"),
                                Some(serde_json::json!({ "key": t.ext.plan_key, "error": e })),
                                now_ns,
                            );
                            // once: hold it for the operator
                            let _ = crate::bus::swarm::recovery::hold(conn, &task_id, now_ns);
                        }
                    }
                }
                Action::GiveUp { task_id } => {
                    let Some(t) = board.iter().find(|t| t.task.id == task_id) else {
                        continue;
                    };
                    if crate::bus::swarm::recovery::give_up(conn, Some(db), swarm, t, now_ns)
                        .is_err()
                    {
                        let _ = crate::bus::swarm::recovery::hold(conn, &task_id, now_ns);
                    }
                }
                Action::RunGate { task_id } => {
                    let (Some(t), Some(cmd)) = (
                        board.iter().find(|t| t.task.id == task_id),
                        merge_gate.command_for(swarm),
                    ) else {
                        continue;
                    };
                    let Some(branch) = t.task.branch.clone() else {
                        continue;
                    };
                    if crate::bus::swarm::gate::start(conn, Some(db), &task_id, now_ns).is_err() {
                        continue;
                    }
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        Some(&task_id),
                        "merge.started",
                        Some("engine"),
                        Some(
                            serde_json::json!({ "key": t.ext.plan_key, "branch": branch, "command": cmd }),
                        ),
                        now_ns,
                    );
                    self.notice = Some(Notice::info(format!(
                        "swarm: checking `{}` before its merge (`{}`)",
                        t.ext.plan_key,
                        crate::app::workflows::short_task(&cmd)
                    )));
                    let worktree = t.task.worktree.clone().map(PathBuf::from);
                    let (db, run) = (db.to_path_buf(), swarm.clone());
                    std::thread::spawn(move || {
                        let tested = crate::bus::swarm::gate::test_branch(
                            Path::new(&run.workspace),
                            worktree.as_deref(),
                            &branch,
                            &cmd,
                            crate::bus::swarm::check::CHECK_TIMEOUT,
                            now_ns,
                        );
                        if let Ok(conn) = crate::tracing::store::open_aux(&db) {
                            let _ = crate::bus::swarm::gate::land(
                                &conn,
                                Some(&db),
                                &run,
                                &task_id,
                                &tested,
                                crate::tracing::store::now_ns(),
                            );
                        }
                    });
                }
                Action::Advance(status) => {
                    let _ = record::set_status(conn, Some(db), &swarm.id, status, now_ns);
                    let _ = record::event(
                        conn,
                        &swarm.id,
                        None,
                        &format!("swarm.{}", status.as_str()),
                        Some("engine"),
                        None,
                        now_ns,
                    );
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
            // a ready task offered since: the coordinator launches it, or the
            // dispatcher does once the grace runs out
            if t.task.status == crate::bus::TaskStatus::Open
                && !t.ext.proposed
                && t.ext.launched_by.is_none()
                && let Some(offered) = t.ext.offered_ns
                && offered > c.events_seen_ns
            {
                events.push(format!(
                    "{key} ready: start it with agent_mux_swarm_launch(tasks = [\"{key}\"]), or agent-mux starts it in {}s",
                    self.swarm_dispatch_grace_ns / 1_000_000_000
                ));
                newest = newest.max(offered);
            }
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
                // a review's first line is only its verdict
                let what = match &t.ext.review_of {
                    Some(_) if t.task.status == crate::bus::TaskStatus::Done => format!(
                        "{what} (a review: tell the operator the verdict and the findings, in agent_mux_swarm_board's `reviews`)"
                    ),
                    _ => what,
                };
                // failed for good: its automatic retry is spent (v26)
                let what = match crate::bus::swarm::recovery::gave_up_note(t, board) {
                    Some(note) => format!("{what} — {note}"),
                    None => what,
                };
                events.push(what);
                newest = newest.max(ended);
            }
            // started again on its own (v26)
            if let Some(at) = t.ext.recovery.retried_ns
                && at > c.events_seen_ns
            {
                events.push(format!(
                    "{key} started again automatically on {}: {} (one automatic retry; tell the operator)",
                    t.task.target_harness.as_deref().unwrap_or("the workers' harness"),
                    t.ext
                        .recovery
                        .reason
                        .as_deref()
                        .map(first_line)
                        .unwrap_or_default()
                ));
                newest = newest.max(at);
            }
            // its merge gate ended (v26)
            if let Some(g) = &t.ext.gate
                && let Some(at) = g.ended_ns
                && at > c.events_seen_ns
            {
                use crate::bus::swarm::gate::GateState;
                match g.state {
                    GateState::Passed => events.push(format!(
                        "{key} merged ({}) after the check passed on its branch",
                        g.detail.as_deref().unwrap_or("?")
                    )),
                    GateState::Failed => events.push(format!(
                        "{key} NOT merged: {}. Tell the operator; to fix it, message its worker (its session stays open) and `merge` again once it committed, or force-merge / set-aside on their word",
                        g.detail.as_deref().unwrap_or("the check failed")
                    )),
                    _ => {}
                }
                newest = newest.max(at);
            }
            if t.ext.proposed && t.task.created_ns > c.events_seen_ns {
                events.push(format!("{key} proposed by a worker: {}", t.task.title));
                newest = newest.max(t.task.created_ns);
            }
        }
        if matches!(
            swarm.status,
            SwarmStatus::Integrating | SwarmStatus::Finished
        ) && swarm.updated_ns > c.events_seen_ns
        {
            events.push(format!(
                "every task is settled ({}): summarise for the operator, ask which branches to merge, and close when they say so",
                swarm.status.as_str()
            ));
            newest = newest.max(swarm.updated_ns);
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
                    "[agent-mux swarm] {} event(s) in your swarm: read them with agent_mux_recv_messages (or agent_mux_swarm_board). Tell the operator what happened in a few lines, start ready tasks with agent_mux_swarm_launch, answer questions, and propose follow-ups if something should change.",
                    events.len()
                ),
            },
            Instant::now(),
        );
        let _ = record::mark_events_told(conn, &swarm.id, newest, now_ns);
    }

    /// The operator stops session `idx` on purpose: when it is a swarm
    /// worker still holding its task, the task is held for them, so the
    /// engine does not start it again on its own (v26, `recovery::hold`).
    pub fn hold_swarm_worker(&mut self, idx: usize) {
        let Some(s) = self.sessions.get(idx) else {
            return;
        };
        let (Some(link), Some(launch)) = (&s.swarm, s.trace.as_ref().map(|t| t.launch_id.clone()))
        else {
            return;
        };
        let Some(task_id) = link.task_id.clone() else {
            return;
        };
        let Some(db) = self.trace_db_path.clone() else {
            return;
        };
        let Ok(conn) = crate::tracing::store::open_aux(&db) else {
            return;
        };
        let holds = crate::bus::store::get_task(&conn, &task_id)
            .ok()
            .flatten()
            .is_some_and(|t| !t.status.is_terminal() && t.assigned_to.as_deref() == Some(&launch));
        if holds {
            let _ =
                crate::bus::swarm::recovery::hold(&conn, &task_id, crate::tracing::store::now_ns());
        }
    }

    /// The indexes in `self.sessions` of `swarm`'s sessions: its
    /// coordinator, its workers and their reconnections (by sidebar group,
    /// by the launch's swarm metadata, or by a launch the board names).
    pub fn swarm_session_indexes(
        &self,
        conn: &rusqlite::Connection,
        swarm: &SwarmRun,
    ) -> Vec<usize> {
        let mut launches: Vec<String> = record::board(conn, &swarm.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.task.assigned_to)
            .collect();
        if let Some(c) = record::coordinator(conn, &swarm.id)
            .ok()
            .flatten()
            .and_then(|c| c.launch)
        {
            launches.push(c);
        }
        launches.extend(swarm.planner_launch.clone());
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.group
                    .as_ref()
                    .is_some_and(|g| g.kind == crate::tree::GroupKind::Swarm && g.id == swarm.id)
                    || s.swarm.as_ref().is_some_and(|l| l.swarm_id == swarm.id)
                    || s.trace
                        .as_ref()
                        .is_some_and(|t| launches.contains(&t.launch_id))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The confirmation `d` asks before deleting `swarm_id` (the Swarm
    /// screen's and the Agents sidebar's): what stops and what is removed.
    pub fn swarm_delete_confirm(
        &self,
        swarm_id: &str,
    ) -> Result<crate::app::swarm_board::Confirm, String> {
        let db = self.trace_db_path.clone().ok_or("no trace store")?;
        let conn = crate::tracing::store::open_aux(&db)?;
        let swarm = record::get_swarm(&conn, swarm_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no swarm {swarm_id}"))?;
        let board = record::board(&conn, &swarm.id).unwrap_or_default();
        let now = Instant::now();
        let sessions = self
            .swarm_session_indexes(&conn, &swarm)
            .into_iter()
            .filter(|&i| !matches!(self.sessions[i].status(now), Status::Exited(_)))
            .count();
        Ok(crate::app::swarm_board::Confirm::delete_of(
            &swarm, &board, sessions,
        ))
    }

    /// `y` on the Agents sidebar's delete confirmation: the same engine
    /// path as the Swarm screen's (`swarm_action_on`, the TUI decider);
    /// the answer is the notice.
    pub fn delete_swarm_from_sidebar(&mut self, swarm_id: &str) {
        let workspace = self
            .trace_db_path
            .clone()
            .and_then(|db| crate::tracing::store::open_aux(&db).ok())
            .and_then(|conn| record::get_swarm(&conn, swarm_id).ok().flatten())
            .map(|s| PathBuf::from(s.workspace))
            .unwrap_or_default();
        let done = self.swarm_action_on(
            Some(swarm_id),
            &workspace,
            SwarmVerb::Delete,
            None,
            None,
            &Decider::TUI,
        );
        self.notice = Some(match done {
            Ok(m) => Notice::info(m),
            Err(e) => Notice::error(format!("✗ {e}")),
        });
    }

    /// `SwarmVerb::Delete` from the TUI: a live swarm is closed first (its
    /// sessions stop, no Langfuse export), then its rows go
    /// (`record::delete_swarm`), its sessions leave the sidebar and its
    /// worktree checkouts are removed; branches and traces are kept.
    fn delete_swarm_run(
        &mut self,
        conn: &rusqlite::Connection,
        db: &Path,
        swarm: &SwarmRun,
    ) -> Result<String, String> {
        let indexes = self.swarm_session_indexes(conn, swarm);
        let at = Instant::now();
        let mut stopped = 0;
        for &i in &indexes {
            if let Some(s) = self.sessions.get_mut(i)
                && !matches!(s.status(at), Status::Exited(_))
            {
                s.kill();
                stopped += 1;
            }
        }
        let mut cancelled = 0;
        if swarm.ended_ns.is_none() {
            // kills what is left (a session it knows by launch only) and
            // ends the swarm
            let (n, ended) = self.stop_swarm_run(swarm, false);
            cancelled = ended?.0;
            stopped = stopped.max(n);
        }
        let swarm = record::get_swarm(conn, &swarm.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no swarm {}", swarm.id))?;
        let now = crate::tracing::store::now_ns();
        let deleted = record::delete_swarm(conn, Some(db), &swarm, now)?;
        // its sessions leave the sidebar; the selection stays on the same
        // session when it was another one
        let keep_selected = self
            .sessions
            .get(self.selected)
            .map(|s| s.id)
            .filter(|_| !indexes.contains(&self.selected));
        let mut i = 0;
        self.sessions.retain(|_| {
            let keep = !indexes.contains(&i);
            i += 1;
            keep
        });
        if !indexes.is_empty() {
            self.selection = None;
            self.selected = keep_selected
                .and_then(|id| self.sessions.iter().position(|s| s.id == id))
                .unwrap_or(self.selected)
                .min(self.sessions.len().saturating_sub(1));
            let _ = self.save_active_sessions();
        }
        let removed =
            crate::bus::swarm::remove_worktrees(Path::new(&swarm.workspace), &deleted.worktrees);
        if let crate::app::Mode::Swarm(v) = &mut self.mode
            && v.scope.as_deref() == Some(swarm.id.as_str())
        {
            v.scope = None;
        }
        let group = crate::app::agents_list::AgentKind::Run(format!("swarm:{}", swarm.id));
        if self.agent_focus.as_ref().is_some_and(|f| f.kind == group) {
            self.agent_focus = None;
        }
        self.last_swarm_engine_pass = None;
        let gone = removed.iter().filter(|(_, r)| r.is_ok()).count();
        let mut out = format!(
            "deleted {}: stopped {stopped} session(s), cancelled {cancelled} task(s), removed {gone} worktree(s)",
            swarm.id
        );
        let failed: Vec<String> = removed
            .iter()
            .filter_map(|(p, r)| r.as_ref().err().map(|e| format!("{}: {e}", p.display())))
            .collect();
        if !failed.is_empty() {
            out.push_str(&format!("; worktree(s) left: {}", failed.join("; ")));
        }
        if !deleted.branches.is_empty() {
            out.push_str(&format!(
                "; {} branch(es) kept: {}",
                deleted.branches.len(),
                deleted.branches.join(", ")
            ));
        }
        if let crate::app::Mode::Swarm(v) = &mut self.mode {
            v.status_msg = Some(out.clone());
        }
        Ok(out)
    }
}

/// At most one wake of the coordinator for events per minute (decision 2).
pub const EVENTS_EVERY_NS: i64 = 60 * 1_000_000_000;

/// The Agents sidebar's title for a swarm's group: its goal, or, before the
/// operator said one, a placeholder.
pub fn swarm_group_title(swarm: &SwarmRun) -> String {
    let goal = swarm.goal.trim();
    if goal.is_empty() {
        "swarm (goal in the chat)".to_string()
    } else {
        crate::app::workflows::short_task(goal)
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(160)
        .collect()
}

/// The branch the workspace has checked out (what a worktree branch forked
/// from), for a review's `git diff <base>...HEAD`.
fn workspace_branch(ws: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !b.is_empty() && b != "HEAD").then_some(b)
}
