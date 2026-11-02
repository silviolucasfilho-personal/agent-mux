//! The swarm planner's answer: one fenced `swarm-plan` block of JSON
//! (`docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §4),
//! parsed into a [`Plan`] and checked before it may be approved. Errors
//! block approval; warnings are shown and do not.

use crate::bus::TaskIsolation;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A plan with more tasks than this draws a warning: the operator reads
/// every one of them before approving.
pub const MANY_TASKS: usize = 12;

/// The fence tag of the planner's answer.
pub const FENCE: &str = "swarm-plan";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct Plan {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tasks: Vec<PlannedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct PlannedTask {
    /// Short, unique within the plan; dependencies name it.
    pub key: String,
    pub title: String,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
}

/// Reads the last fenced `swarm-plan` block of `text` (a planner may quote
/// an example first). A bare JSON object is accepted when there is no
/// fence at all.
pub fn parse(text: &str) -> Result<Plan, String> {
    let open = format!("```{FENCE}");
    let body = match text.rfind(&open) {
        Some(start) => {
            let rest = &text[start + open.len()..];
            let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
            let end = rest
                .find("```")
                .ok_or_else(|| format!("the `{FENCE}` block is not closed"))?;
            rest[..end].to_string()
        }
        None => {
            let t = text.trim();
            if t.starts_with('{') && t.ends_with('}') {
                t.to_string()
            } else {
                return Err(format!("no `{FENCE}` block in the planner's answer"));
            }
        }
    };
    let mut plan: Plan =
        serde_json::from_str(&body).map_err(|e| format!("the `{FENCE}` block: {e}"))?;
    for t in &mut plan.tasks {
        t.key = t.key.trim().to_string();
        t.title = t.title.trim().to_string();
        t.instructions = t.instructions.trim().to_string();
        t.depends_on = t
            .depends_on
            .iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        t.files = t
            .files
            .iter()
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();
        for o in [
            &mut t.acceptance,
            &mut t.harness,
            &mut t.agent,
            &mut t.isolation,
        ] {
            if o.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *o = None;
            }
        }
    }
    Ok(plan)
}

/// What [`check`] needs to know about the machine and the workspace.
#[derive(Debug, Clone, Default)]
pub struct CheckEnv {
    /// Harnesses installed here (`claude`, `codex`, `agy`, `opencode`).
    pub harnesses: Vec<String>,
    /// Agent names the catalog knows.
    pub agents: Vec<String>,
    pub git_repo: bool,
    /// The swarm's default isolation, for tasks that name none.
    pub default_isolation: TaskIsolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Problems {
    /// `(task key or "", message)`; any error blocks approval.
    pub errors: Vec<(String, String)>,
    pub warnings: Vec<(String, String)>,
}

impl CheckEnv {
    /// The checks for `workspace` from this machine: the harnesses on
    /// `PATH` (or where their installers put them), the agent catalog of
    /// the library and the workspace, and whether it is a git repository.
    /// The TUI's `App::swarm_check_env` also counts its profiles' commands.
    pub fn for_workspace(
        workspace: &std::path::Path,
        default_isolation: TaskIsolation,
    ) -> CheckEnv {
        CheckEnv {
            harnesses: crate::harness::Harness::ALL
                .iter()
                .filter(|h| crate::session::command_available(h.as_str()))
                .map(|h| h.as_str().to_string())
                .collect(),
            agents: crate::agents::Catalog::load(&crate::assets::root(), Some(workspace))
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            git_repo: crate::loops::worktree::is_git_repo(workspace),
            default_isolation,
        }
    }
}

impl Problems {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
    /// Every problem of one task, errors first.
    pub fn for_task<'a>(&'a self, key: &'a str) -> impl Iterator<Item = (bool, &'a str)> + 'a {
        self.errors
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, m)| (true, m.as_str()))
            .chain(
                self.warnings
                    .iter()
                    .filter(move |(k, _)| k == key)
                    .map(|(_, m)| (false, m.as_str())),
            )
    }
}

impl PlannedTask {
    pub fn isolation_or(&self, default: TaskIsolation) -> TaskIsolation {
        self.isolation
            .as_deref()
            .and_then(TaskIsolation::parse)
            .unwrap_or(default)
    }
}

/// Checks a plan (§4.2): unique keys, known and acyclic dependencies,
/// non-empty title and instructions, installed harnesses, known agents,
/// worktrees only in a git repository. Warns when two tasks that may run at
/// the same time share files in a shared workspace, and past
/// [`MANY_TASKS`].
pub fn check(plan: &Plan, env: &CheckEnv) -> Problems {
    let mut p = Problems::default();
    let mut err = |k: &str, m: String| p.errors.push((k.to_string(), m));
    if plan.tasks.is_empty() {
        err("", "the plan has no tasks".into());
    }
    let mut seen = HashSet::new();
    for t in &plan.tasks {
        if t.key.is_empty() {
            err("", format!("task \"{}\" has no key", t.title));
        } else if !t
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            err(
                &t.key,
                "a key is letters, digits, `-` and `_` only".to_string(),
            );
        } else if !seen.insert(t.key.as_str()) {
            err(&t.key, format!("the key `{}` is used twice", t.key));
        }
        if t.title.is_empty() {
            err(&t.key, "no title".into());
        }
        if t.instructions.is_empty() {
            err(&t.key, "no instructions".into());
        }
        if let Some(h) = &t.harness
            && !env.harnesses.iter().any(|x| x == h)
        {
            err(&t.key, format!("harness `{h}` is not installed"));
        }
        if let Some(a) = &t.agent
            && !env.agents.iter().any(|x| x == a)
        {
            err(&t.key, format!("no agent named `{a}`"));
        }
        if let Some(i) = &t.isolation
            && TaskIsolation::parse(i).is_none()
        {
            err(
                &t.key,
                format!("isolation `{i}` is neither shared nor worktree"),
            );
        }
        if t.isolation_or(env.default_isolation) == TaskIsolation::Worktree && !env.git_repo {
            err(
                &t.key,
                "worktree isolation needs a git repository".to_string(),
            );
        }
        for d in &t.depends_on {
            if d == &t.key {
                err(&t.key, "depends on itself".into());
            } else if !plan.tasks.iter().any(|o| &o.key == d) {
                err(
                    &t.key,
                    format!("depends on `{d}`, which is not in the plan"),
                );
            }
        }
    }
    if let Some(cycle) = find_cycle(plan) {
        err(
            &cycle[0],
            format!("dependency cycle: {}", cycle.join(" → ")),
        );
    }
    if plan.tasks.len() > MANY_TASKS {
        p.warnings.push((
            String::new(),
            format!(
                "{} tasks: more than {MANY_TASKS} is a lot to review",
                plan.tasks.len()
            ),
        ));
    }
    let shared: Vec<&PlannedTask> = plan
        .tasks
        .iter()
        .filter(|t| t.isolation_or(env.default_isolation) == TaskIsolation::Shared)
        .collect();
    for (i, a) in shared.iter().enumerate() {
        for b in &shared[i + 1..] {
            if ordered(plan, &a.key, &b.key) || ordered(plan, &b.key, &a.key) {
                continue;
            }
            if let Some((fa, fb)) = overlap(&a.files, &b.files) {
                p.warnings.push((
                    b.key.clone(),
                    format!(
                        "may run alongside `{}` and both touch files ({fa} / {fb}) in the shared workspace",
                        a.key
                    ),
                ));
            }
        }
    }
    p
}

/// The keys of a dependency cycle, first repeated at the end, if any.
fn find_cycle(plan: &Plan) -> Option<Vec<String>> {
    let deps: HashMap<&str, &[String]> = plan
        .tasks
        .iter()
        .map(|t| (t.key.as_str(), t.depends_on.as_slice()))
        .collect();
    // 0 unseen, 1 on the path, 2 done
    let mut mark: HashMap<&str, u8> = HashMap::new();
    fn visit<'a>(
        k: &'a str,
        deps: &HashMap<&'a str, &'a [String]>,
        mark: &mut HashMap<&'a str, u8>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match mark.get(k) {
            Some(2) => return None,
            Some(1) => {
                let at = path.iter().position(|p| *p == k).unwrap_or(0);
                let mut c: Vec<String> = path[at..].iter().map(|s| s.to_string()).collect();
                c.push(k.to_string());
                return Some(c);
            }
            _ => {}
        }
        mark.insert(k, 1);
        path.push(k);
        for d in deps.get(k).copied().unwrap_or(&[]) {
            if deps.contains_key(d.as_str())
                && let Some(c) = visit(d.as_str(), deps, mark, path)
            {
                return Some(c);
            }
        }
        path.pop();
        mark.insert(k, 2);
        None
    }
    for t in &plan.tasks {
        let mut path = Vec::new();
        if let Some(c) = visit(t.key.as_str(), &deps, &mut mark, &mut path) {
            return Some(c);
        }
    }
    None
}

/// Whether `later` depends on `earlier`, directly or through others.
fn ordered(plan: &Plan, earlier: &str, later: &str) -> bool {
    let mut stack = vec![later];
    let mut seen = HashSet::new();
    while let Some(k) = stack.pop() {
        if !seen.insert(k) {
            continue;
        }
        if let Some(t) = plan.tasks.iter().find(|t| t.key == k) {
            for d in &t.depends_on {
                if d == earlier {
                    return true;
                }
                stack.push(d.as_str());
            }
        }
    }
    false
}

/// Two file lists that may name the same file: equal patterns, one a
/// directory prefix of the other, or a glob matching the other.
fn overlap<'a>(a: &'a [String], b: &'a [String]) -> Option<(&'a str, &'a str)> {
    for x in a {
        for y in b {
            let (x, y) = (x.as_str(), y.as_str());
            let prefix = |p: &str, q: &str| {
                let p = p.trim_end_matches("/**").trim_end_matches('/');
                q == p || q.starts_with(&format!("{p}/"))
            };
            if x == y
                || prefix(x, y)
                || prefix(y, x)
                || crate::bus::store::pattern_matches_path(x, y)
                || crate::bus::store::pattern_matches_path(y, x)
            {
                return Some((x, y));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> CheckEnv {
        CheckEnv {
            harnesses: vec!["claude".into(), "codex".into()],
            agents: vec!["reviewer".into()],
            git_repo: true,
            default_isolation: TaskIsolation::Shared,
        }
    }

    fn task(key: &str, deps: &[&str], files: &[&str]) -> PlannedTask {
        PlannedTask {
            key: key.into(),
            title: format!("Task {key}"),
            instructions: "do it".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            files: files.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_the_last_fenced_block_and_trims_it() {
        let text = "An example:\n```swarm-plan\n{\"tasks\": []}\n```\nMy plan:\n\
                    ```swarm-plan\n{\"summary\": \"two parts\", \"tasks\": [\
                    {\"key\": \" a \", \"title\": \"A\", \"instructions\": \"x\", \"harness\": \"\",\
                     \"depends_on\": [\" \", \"b\"]},\
                    {\"key\": \"b\", \"title\": \"B\", \"instructions\": \"y\", \"files\": [\"src/b.rs\"]}]}\n```\n";
        let plan = parse(text).unwrap();
        assert_eq!(plan.summary, "two parts");
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].key, "a");
        assert_eq!(
            plan.tasks[0].harness, None,
            "an empty harness means the default"
        );
        assert_eq!(plan.tasks[0].depends_on, ["b"]);
        assert!(
            parse("no plan here")
                .unwrap_err()
                .contains("no `swarm-plan` block")
        );
        assert!(
            parse("```swarm-plan\n{")
                .unwrap_err()
                .contains("not closed")
        );
        assert!(parse("{\"tasks\": [{\"key\": 1}]}").is_err());
    }

    #[test]
    fn a_sound_plan_has_no_errors() {
        let plan = Plan {
            summary: String::new(),
            tasks: vec![
                task("a", &[], &["src/a.rs"]),
                task("b", &["a"], &["src/a.rs"]),
            ],
        };
        let p = check(&plan, &env());
        assert!(p.is_ok(), "{p:?}");
        assert!(
            p.warnings.is_empty(),
            "b runs after a, so sharing a file is fine: {p:?}"
        );
    }

    #[test]
    fn errors_block_and_name_their_task() {
        let mut bad = task("x", &["missing", "x"], &[]);
        bad.harness = Some("gemini".into());
        bad.agent = Some("nobody".into());
        bad.isolation = Some("sandbox".into());
        let mut dup = task("a", &[], &[]);
        dup.instructions.clear();
        let plan = Plan {
            summary: String::new(),
            tasks: vec![task("a", &[], &[]), dup, bad, task("bad key!", &[], &[])],
        };
        let p = check(&plan, &env());
        let msgs: Vec<String> = p.errors.iter().map(|(k, m)| format!("{k}: {m}")).collect();
        for want in [
            "a: the key `a` is used twice",
            "a: no instructions",
            "x: depends on `missing`, which is not in the plan",
            "x: depends on itself",
            "x: harness `gemini` is not installed",
            "x: no agent named `nobody`",
            "x: isolation `sandbox` is neither shared nor worktree",
            "bad key!: a key is letters, digits, `-` and `_` only",
        ] {
            assert!(
                msgs.iter().any(|m| m == want),
                "missing {want:?} in {msgs:#?}"
            );
        }
        assert!(!check(&Plan::default(), &env()).is_ok(), "an empty plan");
    }

    #[test]
    fn a_cycle_is_an_error() {
        let plan = Plan {
            summary: String::new(),
            tasks: vec![
                task("a", &["c"], &[]),
                task("b", &["a"], &[]),
                task("c", &["b"], &[]),
            ],
        };
        let p = check(&plan, &env());
        assert!(
            p.errors
                .iter()
                .any(|(_, m)| m.starts_with("dependency cycle: a → c → b → a")),
            "{p:?}"
        );
    }

    #[test]
    fn worktrees_need_git_and_parallel_shared_files_warn() {
        let mut wt = task("w", &[], &[]);
        wt.isolation = Some("worktree".into());
        let plan = Plan {
            summary: String::new(),
            tasks: vec![
                wt,
                task("a", &[], &["src/bus/**"]),
                task("b", &[], &["src/bus/store.rs"]),
                task("c", &[], &["docs/x.md"]),
            ],
        };
        let mut e = env();
        e.git_repo = false;
        let p = check(&plan, &e);
        assert_eq!(
            p.errors,
            [(
                "w".to_string(),
                "worktree isolation needs a git repository".to_string()
            )]
        );
        assert_eq!(p.warnings.len(), 1, "{p:?}");
        assert_eq!(p.warnings[0].0, "b");
        assert!(p.warnings[0].1.contains("may run alongside `a`"));
        let many = Plan {
            summary: String::new(),
            tasks: (0..13).map(|i| task(&format!("t{i}"), &[], &[])).collect(),
        };
        assert!(
            check(&many, &env())
                .warnings
                .iter()
                .any(|(_, m)| m.contains("13 tasks"))
        );
    }
}
