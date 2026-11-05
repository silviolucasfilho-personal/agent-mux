//! The swarm's dispatcher: a pure function from what the board and the live
//! sessions say to what agent-mux should do next
//! (`docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §5).
//! `App::swarm_engine_pass` gathers the [`Input`] and carries out the
//! [`Action`]s; every decision is here, so it is tested without a PTY.

use super::record::{BoardTask, LaunchedBy, SwarmRun, SwarmStatus};
use crate::bus::TaskStatus;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How long a worker may sit idle on an unfinished task before it is
/// nudged once, and again before it is listed as stalled (§5.3).
pub const STALL_GRACE_NS: i64 = 90 * 1_000_000_000;

/// How long a ready task stays offered to the coordinator before the
/// dispatcher starts it itself (coordinator-first spec §5.4): the
/// coordinator launches, agent-mux makes sure the swarm never waits on it.
pub const DISPATCH_GRACE_NS: i64 = 2 * 60 * 1_000_000_000;

/// A live worker session as the dispatcher sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Working,
    Idle,
    /// Waiting on a prompt in its terminal (a bell, usually a permission).
    Prompt,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Worker {
    pub state: WorkerState,
    /// Nanoseconds since the session last printed anything.
    pub quiet_ns: i64,
}

pub struct Input<'a> {
    pub swarm: &'a SwarmRun,
    pub board: &'a [BoardTask],
    /// Live sessions by launch id.
    pub workers: &'a HashMap<String, Worker>,
    /// Launches with an unanswered question.
    pub asking: &'a HashSet<String>,
    /// Now, in nanoseconds (an abandoned check's age, an offer's age).
    pub now_ns: i64,
    /// How long an offered task waits for the coordinator before the
    /// dispatcher starts it ([`DISPATCH_GRACE_NS`]; shorter in tests).
    pub dispatch_grace_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A ready task with a free slot that nobody asked for yet: tell the
    /// coordinator (it launches with `agent_mux_swarm_launch`), and start
    /// the grace period.
    Offer { task_id: String },
    /// Start a worker for this task, asked for by `by` (the coordinator or
    /// the operator), or by the dispatcher once the offer's grace ran out.
    Spawn { task_id: String, by: LaunchedBy },
    /// Remind an idle worker to finish or ask (once per attempt).
    Nudge { task_id: String, launch_id: String },
    /// Move the swarm to its next state.
    Advance(SwarmStatus),
    /// Run the swarm's final check (`check_cmd`) before it is finished.
    RunCheck,
}

/// A task's state as the board shows it (§3.2), first match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shown {
    Draft,
    Proposed,
    Blocked,
    Ready,
    Working,
    Asking,
    Stalled,
    Prompt,
    Done,
    Review,
    Failed,
    Lost,
    Cancelled,
}

impl Shown {
    pub fn as_str(self) -> &'static str {
        match self {
            Shown::Draft => "draft",
            Shown::Proposed => "proposed",
            Shown::Blocked => "blocked",
            Shown::Ready => "ready",
            Shown::Working => "working",
            Shown::Asking => "asking",
            Shown::Stalled => "stalled",
            Shown::Prompt => "prompt",
            Shown::Done => "done",
            Shown::Review => "review",
            Shown::Failed => "failed",
            Shown::Lost => "lost",
            Shown::Cancelled => "cancelled",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Shown::Draft => "·",
            Shown::Proposed => "+",
            Shown::Blocked => "⋯",
            Shown::Ready => "◦",
            Shown::Working => "●",
            Shown::Asking => "◷",
            Shown::Stalled => "○",
            Shown::Prompt => "⚑",
            Shown::Done => "✓",
            Shown::Review => "⇡",
            Shown::Failed => "✗",
            Shown::Lost => "!",
            Shown::Cancelled => "–",
        }
    }
}

/// Whether `dep` lets the tasks that depend on it start: done and, when it
/// worked on a branch, merged (§5.4).
pub fn dependency_met(dep: &BoardTask) -> bool {
    dep.task.status == TaskStatus::Done
        && (dep.task.branch.is_none() || dep.review.as_deref() == Some("merged"))
}

/// The dependencies of `t` that are not met, by key (unknown keys too).
pub fn unmet_dependencies<'a>(t: &'a BoardTask, board: &'a [BoardTask]) -> Vec<&'a str> {
    t.ext
        .depends_on
        .iter()
        .filter(|k| {
            !board
                .iter()
                .find(|o| &o.ext.plan_key == *k)
                .is_some_and(dependency_met)
        })
        .map(String::as_str)
        .collect()
}

/// Whether a finished task's branch still waits for the operator.
pub fn awaits_review(t: &BoardTask) -> bool {
    t.task.status == TaskStatus::Done && t.task.branch.is_some() && t.review.is_none()
}

fn holds_worker(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Claimed | TaskStatus::Running)
}

fn worker_of<'a>(t: &BoardTask, workers: &'a HashMap<String, Worker>) -> Option<&'a Worker> {
    t.task
        .assigned_to
        .as_deref()
        .and_then(|l| workers.get(l))
        .filter(|w| w.state != WorkerState::Exited)
}

/// How the board shows one task.
pub fn shown(t: &BoardTask, input: &Input<'_>) -> Shown {
    let drafting = matches!(
        input.swarm.status,
        SwarmStatus::Planning | SwarmStatus::Review
    );
    match t.task.status {
        TaskStatus::Cancelled => return Shown::Cancelled,
        TaskStatus::Failed => return Shown::Failed,
        TaskStatus::Done if awaits_review(t) => return Shown::Review,
        TaskStatus::Done => return Shown::Done,
        _ => {}
    }
    if t.ext.proposed {
        return Shown::Proposed;
    }
    if t.task.status == TaskStatus::Open {
        if drafting && t.ext.attempts == 0 {
            return Shown::Draft;
        }
        return if unmet_dependencies(t, input.board).is_empty() {
            Shown::Ready
        } else {
            Shown::Blocked
        };
    }
    let Some(w) = worker_of(t, input.workers) else {
        return Shown::Lost;
    };
    let launch = t.task.assigned_to.as_deref().unwrap_or_default();
    if input.asking.contains(launch) {
        Shown::Asking
    } else if w.state == WorkerState::Prompt {
        Shown::Prompt
    } else if w.state == WorkerState::Idle && w.quiet_ns >= STALL_GRACE_NS {
        Shown::Stalled
    } else {
        Shown::Working
    }
}

/// Workers holding a task right now: the slots taken against `max_workers`.
/// A finished worker stays open for follow-ups but holds no slot.
pub fn busy_workers(input: &Input<'_>) -> usize {
    input
        .board
        .iter()
        .filter(|t| holds_worker(t.task.status) && worker_of(t, input.workers).is_some())
        .count()
}

/// What to do next. Ready tasks are taken in plan order up to the cap: one
/// somebody asked for (`launched_by`) spawns at once, one never offered is
/// offered to the coordinator, and one offered longer than the grace spawns
/// by the dispatcher. Nothing spawns while planning, in review or paused;
/// nudges go to idle workers past [`STALL_GRACE_NS`] that were not nudged
/// this attempt; the swarm advances once every task is settled.
pub fn next_actions(input: &Input<'_>) -> Vec<Action> {
    let status = input.swarm.status;
    let mut out = Vec::new();
    if !matches!(
        status,
        SwarmStatus::Running | SwarmStatus::Paused | SwarmStatus::Integrating
    ) {
        return out;
    }
    for t in input.board {
        if holds_worker(t.task.status)
            && t.ext.nudged_ns.is_none()
            && !input
                .asking
                .contains(t.task.assigned_to.as_deref().unwrap_or_default())
            && let Some(w) = worker_of(t, input.workers)
            && w.state == WorkerState::Idle
            && w.quiet_ns >= STALL_GRACE_NS
        {
            out.push(Action::Nudge {
                task_id: t.task.id.clone(),
                launch_id: t.task.assigned_to.clone().unwrap_or_default(),
            });
        }
    }
    if status == SwarmStatus::Running {
        let mut free = input.swarm.max_workers.saturating_sub(busy_workers(input));
        for t in input.board {
            if free == 0 {
                break;
            }
            if t.task.status == TaskStatus::Open
                && !t.ext.proposed
                && unmet_dependencies(t, input.board).is_empty()
            {
                match (t.ext.launched_by, t.ext.offered_ns) {
                    (Some(by), _) => {
                        out.push(Action::Spawn {
                            task_id: t.task.id.clone(),
                            by,
                        });
                        free -= 1;
                    }
                    (None, None) => out.push(Action::Offer {
                        task_id: t.task.id.clone(),
                    }),
                    (None, Some(offered)) if input.now_ns - offered >= input.dispatch_grace_ns => {
                        out.push(Action::Spawn {
                            task_id: t.task.id.clone(),
                            by: LaunchedBy::Dispatcher,
                        });
                        free -= 1;
                    }
                    (None, Some(_)) => {}
                }
            }
        }
    }
    let settled = input.board.iter().all(|t| t.task.status.is_terminal());
    let next = if !settled {
        (status == SwarmStatus::Integrating).then_some(SwarmStatus::Running)
    } else if input.board.iter().any(awaits_review) {
        (status != SwarmStatus::Integrating).then_some(SwarmStatus::Integrating)
    } else {
        match (&input.swarm.check_cmd, &input.swarm.check_result) {
            (None, _) => Some(SwarmStatus::Finished),
            (Some(_), None) => {
                out.push(Action::RunCheck);
                None
            }
            (Some(_), Some(raw)) => match super::check::CheckResult::parse(raw) {
                Some(r) if r.running && !r.abandoned(input.now_ns) => None,
                Some(r) if !r.running => Some(SwarmStatus::Finished),
                _ => {
                    out.push(Action::RunCheck);
                    None
                }
            },
        }
    };
    if let Some(n) = next {
        out.push(Action::Advance(n));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::swarm::record::SwarmTaskExt;
    use crate::bus::{BusTask, TaskIsolation};

    fn swarm(status: SwarmStatus, max_workers: usize) -> SwarmRun {
        SwarmRun {
            id: "swarm-1".into(),
            workspace: "/ws".into(),
            goal: "g".into(),
            status,
            planner_harness: "claude".into(),
            planner_launch: None,
            plan_summary: None,
            plan_error: None,
            max_workers,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: None,
            check_result: None,
            owner_run: None,
            started_ns: 0,
            updated_ns: 0,
            ended_ns: None,
            gate_mode: super::super::record::GateMode::Chat,
            headless: false,
        }
    }

    /// A ready task the operator asked for: it spawns at once.
    fn task(key: &str, status: TaskStatus, deps: &[&str]) -> BoardTask {
        let mut t = fresh(key, status, deps);
        if status == TaskStatus::Open {
            t.ext.launched_by = Some(LaunchedBy::Operator);
        }
        t
    }

    /// A ready task nobody asked for yet.
    fn fresh(key: &str, status: TaskStatus, deps: &[&str]) -> BoardTask {
        BoardTask {
            task: BusTask {
                id: format!("task-{key}"),
                workspace: "/ws".into(),
                created_by: "swarm:swarm-1".into(),
                parent_task_id: None,
                assigned_to: None,
                target_harness: Some("claude".into()),
                target_agent: None,
                title: key.into(),
                instructions: "x".into(),
                files: vec![],
                isolation: TaskIsolation::Shared,
                status,
                result: None,
                error: None,
                worktree: None,
                branch: None,
                created_ns: 0,
                updated_ns: 0,
                ended_ns: None,
            },
            ext: SwarmTaskExt {
                task_id: format!("task-{key}"),
                plan_key: key.into(),
                depends_on: deps.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            review: None,
        }
    }

    fn running(key: &str, launch: &str, deps: &[&str]) -> BoardTask {
        let mut t = task(key, TaskStatus::Running, deps);
        t.task.assigned_to = Some(launch.into());
        t.ext.attempts = 1;
        t
    }

    fn live(state: WorkerState, quiet_s: i64) -> Worker {
        Worker {
            state,
            quiet_ns: quiet_s * 1_000_000_000,
        }
    }

    fn run(
        s: &SwarmRun,
        board: &[BoardTask],
        workers: &[(&str, Worker)],
        asking: &[&str],
    ) -> Vec<Action> {
        let workers: HashMap<String, Worker> =
            workers.iter().map(|(k, w)| (k.to_string(), *w)).collect();
        let asking: HashSet<String> = asking.iter().map(|s| s.to_string()).collect();
        next_actions(&Input {
            swarm: s,
            board,
            workers: &workers,
            asking: &asking,
            now_ns: 1_000 * 1_000_000_000,
            dispatch_grace_ns: 120 * 1_000_000_000,
        })
    }

    fn spawn(key: &str) -> Action {
        Action::Spawn {
            task_id: format!("task-{key}"),
            by: LaunchedBy::Operator,
        }
    }

    fn offer(key: &str) -> Action {
        Action::Offer {
            task_id: format!("task-{key}"),
        }
    }

    #[test]
    fn a_ready_task_is_offered_then_started_by_whoever_asks_or_by_the_dispatcher_after_the_grace() {
        let s = swarm(SwarmStatus::Running, 2);
        // Nobody asked: offered to the coordinator, nothing spawns.
        let a = fresh("a", TaskStatus::Open, &[]);
        assert_eq!(run(&s, std::slice::from_ref(&a), &[], &[]), [offer("a")]);
        // Offered a minute ago: still the coordinator's to launch.
        let mut offered = a.clone();
        offered.ext.offered_ns = Some(940 * 1_000_000_000);
        assert_eq!(run(&s, std::slice::from_ref(&offered), &[], &[]), []);
        // The coordinator asked: spawns now, whatever the offer's age.
        let mut asked = offered.clone();
        asked.ext.launched_by = Some(LaunchedBy::Coordinator);
        assert_eq!(
            run(&s, std::slice::from_ref(&asked), &[], &[]),
            [Action::Spawn {
                task_id: "task-a".into(),
                by: LaunchedBy::Coordinator,
            }]
        );
        // Offered longer than the grace: the dispatcher starts it itself.
        let mut stale = a.clone();
        stale.ext.offered_ns = Some(800 * 1_000_000_000);
        assert_eq!(
            run(&s, std::slice::from_ref(&stale), &[], &[]),
            [Action::Spawn {
                task_id: "task-a".into(),
                by: LaunchedBy::Dispatcher,
            }]
        );
        // An offer takes no slot, but only tasks that could run are offered.
        let board = [running("w", "L1", &[]), fresh("b", TaskStatus::Open, &[])];
        let w = [("L1", live(WorkerState::Working, 0))];
        assert_eq!(run(&swarm(SwarmStatus::Running, 1), &board, &w, &[]), []);
        assert_eq!(
            run(&swarm(SwarmStatus::Running, 2), &board, &w, &[]),
            [offer("b")]
        );
        // Paused or in review: no offers either.
        assert_eq!(
            run(
                &swarm(SwarmStatus::Paused, 2),
                std::slice::from_ref(&a),
                &[],
                &[]
            ),
            []
        );
        assert_eq!(run(&swarm(SwarmStatus::Review, 2), &[a], &[], &[]), []);
    }

    #[test]
    fn nothing_starts_before_approval_or_while_paused() {
        let board = [task("a", TaskStatus::Open, &[])];
        for st in [
            SwarmStatus::Planning,
            SwarmStatus::Review,
            SwarmStatus::Finished,
        ] {
            assert_eq!(run(&swarm(st, 3), &board, &[], &[]), [], "{st:?}");
        }
        assert_eq!(run(&swarm(SwarmStatus::Paused, 3), &board, &[], &[]), []);
        assert_eq!(
            run(&swarm(SwarmStatus::Running, 3), &board, &[], &[]),
            [spawn("a")]
        );
    }

    #[test]
    fn spawns_ready_tasks_in_plan_order_up_to_the_cap() {
        let board = [
            running("a", "L1", &[]),
            task("b", TaskStatus::Open, &[]),
            task("c", TaskStatus::Open, &["a"]),
            task("d", TaskStatus::Open, &[]),
            task("e", TaskStatus::Open, &[]),
        ];
        let w = [("L1", live(WorkerState::Working, 0))];
        assert_eq!(
            run(&swarm(SwarmStatus::Running, 3), &board, &w, &[]),
            [spawn("b"), spawn("d")],
            "a holds one of three slots; c waits for a"
        );
        // A finished worker stays open but frees its slot.
        let mut done = board.clone();
        done[0].task.status = TaskStatus::Done;
        assert_eq!(
            run(&swarm(SwarmStatus::Running, 3), &done, &w, &[]),
            [spawn("b"), spawn("c"), spawn("d")]
        );
        // An exited worker holds no slot either (its task is lost).
        let gone = [("L1", live(WorkerState::Exited, 0))];
        assert_eq!(
            run(&swarm(SwarmStatus::Running, 1), &board, &gone, &[]),
            [spawn("b")]
        );
    }

    #[test]
    fn a_worktree_dependency_counts_once_its_branch_is_merged() {
        let mut a = task("a", TaskStatus::Done, &[]);
        a.task.branch = Some("loop/bus-a".into());
        let b = task("b", TaskStatus::Open, &["a"]);
        let s = swarm(SwarmStatus::Running, 2);
        assert_eq!(run(&s, &[a.clone(), b.clone()], &[], &[]), []);
        a.review = Some("discarded".into());
        assert_eq!(
            run(&s, &[a.clone(), b.clone()], &[], &[]),
            [],
            "set aside: still blocked"
        );
        a.review = Some("merged".into());
        assert_eq!(run(&s, &[a, b], &[], &[]), [spawn("b")]);
    }

    #[test]
    fn proposed_tasks_wait_for_the_operator() {
        let mut p = task("p", TaskStatus::Open, &[]);
        p.ext.proposed = true;
        assert_eq!(run(&swarm(SwarmStatus::Running, 2), &[p], &[], &[]), []);
    }

    #[test]
    fn an_idle_worker_is_nudged_once_after_the_grace() {
        let s = swarm(SwarmStatus::Running, 1);
        let board = [running("a", "L1", &[])];
        assert_eq!(
            run(&s, &board, &[("L1", live(WorkerState::Idle, 30))], &[]),
            []
        );
        let nudge = Action::Nudge {
            task_id: "task-a".into(),
            launch_id: "L1".into(),
        };
        assert_eq!(
            run(&s, &board, &[("L1", live(WorkerState::Idle, 91))], &[]),
            std::slice::from_ref(&nudge)
        );
        assert_eq!(
            run(
                &swarm(SwarmStatus::Paused, 1),
                &board,
                &[("L1", live(WorkerState::Idle, 91))],
                &[]
            ),
            [nudge],
            "a paused swarm still reminds its workers"
        );
        let mut nudged = board.clone();
        nudged[0].ext.nudged_ns = Some(1);
        assert_eq!(
            run(&s, &nudged, &[("L1", live(WorkerState::Idle, 500))], &[]),
            []
        );
        // Not when it asked a question, sits on a prompt, or works.
        for (w, asking) in [
            (live(WorkerState::Idle, 500), vec!["L1"]),
            (live(WorkerState::Prompt, 500), vec![]),
            (live(WorkerState::Working, 500), vec![]),
        ] {
            assert_eq!(run(&s, &board, &[("L1", w)], &asking), []);
        }
    }

    #[test]
    fn the_swarm_advances_when_everything_is_settled() {
        let s = swarm(SwarmStatus::Running, 2);
        let done = task("a", TaskStatus::Done, &[]);
        let failed = task("b", TaskStatus::Failed, &[]);
        assert_eq!(
            run(&s, &[done.clone(), failed.clone()], &[], &[]),
            [Action::Advance(SwarmStatus::Finished)]
        );
        let mut branch = task("c", TaskStatus::Done, &[]);
        branch.task.branch = Some("loop/bus-c".into());
        assert_eq!(
            run(&s, &[done.clone(), branch.clone()], &[], &[]),
            [Action::Advance(SwarmStatus::Integrating)]
        );
        let integrating = swarm(SwarmStatus::Integrating, 2);
        assert_eq!(
            run(&integrating, &[done.clone(), branch.clone()], &[], &[]),
            []
        );
        branch.review = Some("merged".into());
        assert_eq!(
            run(&integrating, &[done.clone(), branch], &[], &[]),
            [Action::Advance(SwarmStatus::Finished)]
        );
        // A retried task sends an integrating swarm back to running.
        let retried = task("b", TaskStatus::Open, &[]);
        assert_eq!(
            run(&integrating, &[done, retried], &[], &[]),
            [Action::Advance(SwarmStatus::Running)]
        );
    }

    #[test]
    fn a_final_check_runs_once_before_the_swarm_is_finished() {
        let mut s = swarm(SwarmStatus::Running, 2);
        s.check_cmd = Some("cargo test".into());
        let done = [task("a", TaskStatus::Done, &[])];
        assert_eq!(run(&s, &done, &[], &[]), [Action::RunCheck]);
        s.check_result = Some(super::super::check::CheckResult::started(0).to_json());
        assert_eq!(run(&s, &done, &[], &[]), [], "it is running");
        let mut r = super::super::check::CheckResult::started(0);
        r.running = false;
        r.exit = Some(1);
        s.check_result = Some(r.to_json());
        assert_eq!(
            run(&s, &done, &[], &[]),
            [Action::Advance(SwarmStatus::Finished)],
            "a failed check still finishes the swarm; the operator reads it"
        );
    }

    #[test]
    fn the_board_shows_each_task_by_its_state() {
        let mut wt = task("r", TaskStatus::Done, &[]);
        wt.task.branch = Some("loop/bus-r".into());
        let mut prop = task("p", TaskStatus::Open, &[]);
        prop.ext.proposed = true;
        let board = vec![
            task("blocked", TaskStatus::Open, &["w"]),
            task("ready", TaskStatus::Open, &[]),
            running("w", "L1", &[]),
            running("q", "L2", &[]),
            running("s", "L3", &[]),
            running("m", "L4", &[]),
            running("lost", "gone", &[]),
            wt,
            prop,
            task("f", TaskStatus::Failed, &[]),
        ];
        let workers: HashMap<String, Worker> = [
            ("L1", live(WorkerState::Working, 0)),
            ("L2", live(WorkerState::Idle, 500)),
            ("L3", live(WorkerState::Idle, 500)),
            ("L4", live(WorkerState::Prompt, 0)),
        ]
        .into_iter()
        .map(|(k, w)| (k.to_string(), w))
        .collect();
        let asking: HashSet<String> = ["L2".to_string()].into();
        let s = swarm(SwarmStatus::Running, 4);
        let input = Input {
            swarm: &s,
            board: &board,
            workers: &workers,
            asking: &asking,
            now_ns: 0,
            dispatch_grace_ns: DISPATCH_GRACE_NS,
        };
        let got: Vec<&str> = board.iter().map(|t| shown(t, &input).as_str()).collect();
        assert_eq!(
            got,
            [
                "blocked", "ready", "working", "asking", "stalled", "prompt", "lost", "review",
                "proposed", "failed"
            ]
        );
        let review = swarm(SwarmStatus::Review, 4);
        let input = Input {
            swarm: &review,
            board: &board[..2],
            workers: &workers,
            asking: &asking,
            now_ns: 0,
            dispatch_grace_ns: DISPATCH_GRACE_NS,
        };
        assert_eq!(shown(&board[1], &input), Shown::Draft);
    }
}
