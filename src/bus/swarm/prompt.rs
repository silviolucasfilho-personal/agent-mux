//! What the swarm's models read: the worker's opening prompt (its whole
//! task, §5.2), the one nudge (§5.3) and the planner's context file (§4.1).

use super::record::{BoardTask, SwarmRun};
use crate::bus::TaskIsolation;
use serde::Serialize;

/// The opening message of the worker that runs `t`. It is passed on the
/// command line like any opening prompt, so the task starts without anything
/// being typed into the terminal. `workspace` is named in every bus call: a
/// worktree worker's own folder is not the board's workspace.
pub fn worker_prompt(swarm: &SwarmRun, t: &BoardTask, board: &[BoardTask]) -> String {
    let id = &t.task.id;
    let ws = &swarm.workspace;
    let mut p = format!(
        "You are a worker of an agent-mux swarm. Your task is `{id}` ({key}): **{title}**.\n\n\
         The swarm's goal, for context: {goal}\n\n\
         ### Instructions\n{instructions}\n",
        key = t.ext.plan_key,
        title = t.task.title,
        goal = swarm.goal,
        instructions = t.task.instructions,
    );
    if let Some(a) = t.ext.acceptance.as_deref().filter(|a| !a.trim().is_empty()) {
        p.push_str(&format!("\n### Done when\n{a}\n"));
    }
    if !t.task.files.is_empty() {
        p.push_str(&format!(
            "\n### Files\n{}\n",
            t.task
                .files
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    p.push_str(&match t.task.isolation {
        TaskIsolation::Worktree => {
            "\nYou work in your own git worktree, on your own branch: commit your work there. \
            Do not push, and do not merge; the operator merges your branch.\n"
                .to_string()
        }
        TaskIsolation::Shared => {
            "\nYou share the workspace with other workers. A file another worker holds a lease on \
            is refused to your edits: leave it, or ask.\n"
                .to_string()
        }
    });
    let deps: Vec<&BoardTask> = t
        .ext
        .depends_on
        .iter()
        .filter_map(|k| board.iter().find(|o| &o.ext.plan_key == k))
        .collect();
    if !deps.is_empty() {
        p.push_str("\n### What the tasks before yours did\n");
        for d in deps {
            let result = d
                .task
                .result
                .as_ref()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "(no summary)".into());
            p.push_str(&format!(
                "- `{}` {}: {}",
                d.ext.plan_key, d.task.title, result
            ));
            if let Some(b) = &d.task.branch {
                p.push_str(&format!(" (branch `{b}`, merged into the workspace)"));
            }
            p.push('\n');
        }
    }
    p.push_str(&format!(
        "\n### Finishing\n\
         Your task is not over until you report it. When it is done, call \
         `agent_mux_task_update(action = \"done\", task_id = \"{id}\", workspace = \"{ws}\", result = \"<what you changed and how you checked it>\")`. \
         If you cannot do it, call `agent_mux_task_update(action = \"fail\", task_id = \"{id}\", workspace = \"{ws}\", result = \"<why>\")`. \
         If you need a decision you cannot make yourself, ask the swarm's coordinator with \
         `agent_mux_send_message(to = \"coordinator\", topic = \"question\", wake = true, workspace = \"{ws}\", body = \"<the question itself on the first line, then any context>\")` \
         and wait for the answer; it asks the operator when only they can decide. If you find work outside your task, propose it with \
         `agent_mux_task_split(workspace = \"{ws}\", tasks = [...])`: agent-mux puts it on the board \
         for the operator, and you carry on with your own task; never start other agents yourself. \
         Without the MCP tools: `agent-mux bus done {id} --workspace {ws} --result '<summary>'`."
    ));
    p
}

/// The coordinator's opening prompt (coordinator-first design §5.2): three
/// lines from the library's `[swarm] coordinator` template, the role itself
/// living in the `swarm-coordinator` skill. `invocation` is the skill as
/// the harness invokes it, `context` the context file's path; `restart` is
/// the board summary for a coordinator that replaces one that exited.
pub fn coordinator_prompt(
    swarm: &SwarmRun,
    invocation: &str,
    context: &str,
    template: &str,
    restart: Option<&str>,
) -> String {
    let goal = if swarm.goal.trim().is_empty() {
        "The operator has not said the goal yet: ask for it.".to_string()
    } else {
        format!("The operator's goal: {}", swarm.goal.trim())
    };
    let template = if template.trim().is_empty() {
        DEFAULT_COORDINATOR
    } else {
        template
    };
    let mut p = template
        .replace("{id}", &swarm.id)
        .replace("{workspace}", &swarm.workspace)
        .replace("{invocation}", invocation)
        .replace("{context}", context)
        .replace("{goal}", &goal);
    if let Some(board) = restart {
        p.push_str(&format!(
            "\n\nYou replace a coordinator that exited. The board as it stands:\n{board}\n\
             The plan is {}: do not plan again. Read the board with agent_mux_swarm_board, tell the \
             operator where things stand, then follow the skill from step 4.",
            if swarm.status.approved() {
                "approved and running"
            } else {
                "not approved yet"
            }
        ));
    }
    p
}

/// The template when the library has none.
const DEFAULT_COORDINATOR: &str = "You are the coordinator of agent-mux swarm {id} in {workspace}.\n{invocation} Read the context at {context} (also in $AGENT_MUX_SWARM_CONTEXT) first and follow the skill.\n{goal}";

/// The one reminder an idle worker gets before it is listed as stalled.
pub fn nudge_prompt(t: &BoardTask, workspace: &str) -> String {
    format!(
        "[agent-mux swarm] You stopped before finishing task {id} ({key}). If it is done, call \
         agent_mux_task_update with action done, task_id {id}, workspace {workspace} and a result; \
         if you cannot finish, call it with action fail and the reason; if you need a decision, \
         ask the user with agent_mux_send_message, topic question.",
        id = t.task.id,
        key = t.ext.plan_key,
    )
}

/// The coordinator's context file (`$AGENT_MUX_SWARM_CONTEXT`; also
/// `$AGENT_MUX_SWARM_PLAN` for older skills).
#[derive(Debug, Clone, Serialize)]
pub struct PlannerContext {
    pub goal: String,
    pub workspace: serde_json::Value,
    pub workspace_path: String,
    pub git_repo: bool,
    pub harnesses: Vec<String>,
    pub default_harness: String,
    pub agents: Vec<AgentInfo>,
    pub max_workers: usize,
    pub default_isolation: String,
    /// On a re-plan: the board as it stands and the operator's notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replan: Option<Replan>,
    /// What the operator fixed in advance (coordinator-first §5.2): `null`
    /// where the coordinator may propose.
    pub limits: Limits,
    /// The swarm tools and their shell twins, and the waits' defaults.
    pub tools: Tools,
    /// One line on what the operator sees meanwhile.
    pub screen: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// `chat` or `off`.
    pub gate: String,
    /// The goal is final and the coordinator plans without asking.
    pub headless: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tools {
    /// `agent_mux_swarm_wait`'s default and ceiling, in milliseconds.
    pub wait_ms: u64,
    pub wait_ms_max: u64,
    /// After this many seconds an offered task starts on its own.
    pub dispatch_grace_s: i64,
    /// The MCP tools the coordinator uses, in the order of the playbook.
    pub mcp: Vec<&'static str>,
    /// Their shell twins, for a harness without MCP.
    pub cli: Vec<&'static str>,
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            wait_ms: super::view::DEFAULT_SWARM_WAIT_MS,
            wait_ms_max: super::view::MAX_SWARM_WAIT_MS,
            dispatch_grace_s: super::engine::DISPATCH_GRACE_NS / 1_000_000_000,
            mcp: vec![
                "agent_mux_swarm_plan",
                "agent_mux_swarm_launch",
                "agent_mux_swarm_wait",
                "agent_mux_swarm_board",
                "agent_mux_swarm_action",
                "agent_mux_send_message",
                "agent_mux_recv_messages",
            ],
            cli: vec![
                "agent-mux swarm submit-plan --file PLAN.json",
                "agent-mux swarm launch [<task>...] --as-coordinator --said \"<words>\"",
                "agent-mux swarm wait [--timeout MS] [--after ID] --json",
                "agent-mux swarm status --json",
                "agent-mux swarm <pause|resume|retry|cancel|merge|set-aside|accept|drop|close> [<task>] --as-coordinator --said \"<words>\"",
                "agent-mux bus send --to <launch> --reply-to <id> --wake <body>",
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Replan {
    pub notes: String,
    pub board: Vec<ReplanTask>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplanTask {
    pub key: String,
    pub title: String,
    pub status: String,
    pub started: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::swarm::record::SwarmTaskExt;
    use crate::bus::{BusTask, TaskStatus};

    fn board_task(key: &str, deps: &[&str]) -> BoardTask {
        BoardTask {
            task: BusTask {
                id: format!("task-{key}"),
                workspace: "/ws".into(),
                created_by: "swarm:s".into(),
                parent_task_id: None,
                assigned_to: None,
                target_harness: None,
                target_agent: None,
                title: format!("Title {key}"),
                instructions: format!("Do {key}"),
                files: vec!["src/a.rs".into()],
                isolation: TaskIsolation::Worktree,
                status: TaskStatus::Open,
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
                acceptance: Some("tests pass".into()),
                ..Default::default()
            },
            review: None,
        }
    }

    #[test]
    fn the_worker_prompt_carries_the_whole_task_and_how_to_finish() {
        let swarm = crate::bus::swarm::record::SwarmRun {
            id: "s".into(),
            workspace: "/ws".into(),
            goal: "ship".into(),
            status: crate::bus::swarm::record::SwarmStatus::Running,
            planner_harness: "claude".into(),
            planner_launch: None,
            plan_summary: None,
            plan_error: None,
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            gate_mode: crate::bus::swarm::record::GateMode::Chat,
            headless: false,
            check_cmd: None,
            check_result: None,
            owner_run: None,
            started_ns: 0,
            updated_ns: 0,
            ended_ns: None,
        };
        let mut a = board_task("a", &[]);
        a.task.status = TaskStatus::Done;
        a.task.result = Some(serde_json::json!("added the parser"));
        a.task.branch = Some("loop/bus-a".into());
        let b = board_task("b", &["a"]);
        let p = worker_prompt(&swarm, &b, &[a, b.clone()]);
        for want in [
            "Your task is `task-b` (b): **Title b**",
            "The swarm's goal, for context: ship",
            "Do b",
            "### Done when\ntests pass",
            "- `src/a.rs`",
            "your own git worktree",
            "- `a` Title a: added the parser (branch `loop/bus-a`, merged into the workspace)",
            "action = \"done\", task_id = \"task-b\", workspace = \"/ws\"",
            "to = \"coordinator\", topic = \"question\", wake = true",
        ] {
            assert!(p.contains(want), "missing {want:?} in:\n{p}");
        }
        let n = nudge_prompt(&b, "/ws");
        assert!(n.contains("task-b") && n.contains("workspace /ws"), "{n}");
        assert!(!n.contains('\n'), "a wake is one line");
    }
}
