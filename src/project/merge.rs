//! `project merge`: the one place agent-mux merges, and only on a
//! person's yes.
//!
//! The port of spec-wave's `merge` (`spec-wave-cli` `commands/merge.mjs`,
//! read on 2026-11-22): the pull requests of a Feature's Stories, in the
//! order of their dependencies. For each one its base is pointed at the
//! default branch (the one before it has merged, and with a merge commit
//! the retarget is clean, which is why the method is never a squash), it
//! is merged, and the board moves to QA; the branches are deleted only at
//! the end, when no open pull request is based on them.
//!
//! **A draft blocks the whole plan.** Marking a pull request ready is the
//! person's review, and merging over a draft would skip the one approval
//! the flow has. A failure in the middle stops the queue: what merged
//! stays merged, and running again resumes, since a merged pull request
//! leaves the plan.
//!
//! A Story or a Bug may be named alone: its queue is its own pull request.
//!
//! `gh` 2.98.0 was probed on 2026-11-22 (`gh pr merge --help`, `gh pr edit
//! --help`, `gh pr view --help`): `gh pr view <n> --repo R --json
//! state,isDraft,baseRefName,headRefName,commits`, `gh pr edit <n> --repo R
//! --base <branch>`, `gh pr merge <n> --repo R --merge`, and the branch is
//! removed with `gh api -X DELETE repos/R/git/refs/heads/<branch>`
//! (`--delete-branch` is never passed: it is what closed a stacked pull
//! request for good in spec-wave's history).

use super::build::missing_trailers;
use super::github::{gh, json};
use super::ops::{Ops, Report};
use super::order::{Node, order_nodes};
use super::{Item, ItemType, Pull, Snapshot};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One pull request of the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Queued {
    /// The Story or Bug the pull request delivers.
    pub item: u64,
    pub title: String,
    pub pull: Pull,
    /// The default branch of the pull request's repository.
    pub default: String,
}

/// What a merge would do, decided before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct MergePlan {
    /// Open, ready pull requests in merge order.
    pub queue: Vec<Queued>,
    /// Items whose pull request is merged already: what makes a second
    /// run a resume.
    pub done: Vec<(u64, Pull)>,
    pub without_pr: Vec<u64>,
    /// What stops the whole plan.
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
}

/// `#12` in the hub, `acme/api#12` outside it.
pub fn pr_ref(pull: &Pull, hub: &str) -> String {
    if pull.repo.is_empty() || pull.repo.eq_ignore_ascii_case(hub) {
        format!("#{}", pull.number)
    } else {
        format!("{}#{}", pull.repo, pull.number)
    }
}

/// The items a merge of `number` covers, in order: a Feature's Stories by
/// their dependencies, or the Story or Bug alone.
pub fn scope(snap: &Snapshot, number: u64) -> Result<Vec<u64>, String> {
    let item = snap
        .item(number)
        .ok_or_else(|| format!("issue #{number} is not in the Project"))?;
    match item.item_type {
        Some(ItemType::Story | ItemType::Bug) => Ok(vec![number]),
        Some(ItemType::Feature) => {
            let stories: Vec<&Item> = snap
                .children_of(number)
                .into_iter()
                .filter(|c| c.item_type == Some(ItemType::Story))
                .collect();
            if stories.is_empty() {
                return Err(format!(
                    "Feature #{number} has no Stories: nothing to merge"
                ));
            }
            let nodes: Vec<Node> = stories
                .iter()
                .map(|s| Node {
                    number: s.number,
                    depends_on: s.depends_on.clone(),
                })
                .collect();
            let ordered = order_nodes(&nodes);
            if !ordered.cycle.is_empty() {
                return Err(format!(
                    "the Stories {} depend on each other in a cycle: there is no merge order. Correct their \"Depende de\" lines and run again",
                    ordered
                        .cycle
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(ordered.order)
        }
        other => Err(format!(
            "#{number} is {}: a merge takes a Feature, a Story or a Bug",
            other.map_or("an issue with no type", ItemType::as_str)
        )),
    }
}

/// The merge plan (pure; spec-wave's `planMerge`). `defaults` is the
/// default branch of each repository, by lowercase `owner/repo`; a
/// repository that is not there takes the hub's.
pub fn plan(sorted: &[u64], snap: &Snapshot, defaults: &BTreeMap<String, String>) -> MergePlan {
    let mut out = MergePlan::default();
    let hub = snap.hub.to_lowercase();
    let repo_key = |p: &Pull| {
        if p.repo.is_empty() {
            hub.clone()
        } else {
            p.repo.to_lowercase()
        }
    };
    let default_of = |p: &Pull| {
        defaults
            .get(&repo_key(p))
            .cloned()
            .unwrap_or_else(|| snap.default_branch.clone())
    };
    // a stack is per repository: the head of a pull request in one is no
    // base for a pull request in another
    let mut heads: BTreeSet<String> = BTreeSet::new();
    for n in sorted {
        let Some(item) = snap.item(*n) else {
            out.without_pr.push(*n);
            continue;
        };
        let merged: Vec<&Pull> = item.pulls.iter().filter(|p| p.merged()).collect();
        let open: Vec<&Pull> = item.pulls.iter().filter(|p| p.open()).collect();
        if open.is_empty() {
            match merged.first() {
                Some(p) => out.done.push((*n, (*p).clone())),
                None => out.without_pr.push(*n),
            }
            continue;
        }
        // one pull request per repository is a Story across repositories;
        // two open in the same one is ambiguous
        let mut by_repo: BTreeMap<String, Vec<&Pull>> = BTreeMap::new();
        for p in open {
            by_repo.entry(repo_key(p)).or_default().push(p);
        }
        let mut keys: Vec<String> = by_repo.keys().cloned().collect();
        keys.sort_by(|a, b| (a != &hub).cmp(&(b != &hub)).then(a.cmp(b)));
        let mut blocked = false;
        let mut own: Vec<&Pull> = Vec::new();
        for key in keys {
            let group = &by_repo[&key];
            if group.len() > 1 {
                out.blocks.push(format!(
                    "#{n} has {} open pull requests in the same repository ({}): close the one that does not count before merging",
                    group.len(),
                    group
                        .iter()
                        .map(|p| pr_ref(p, &snap.hub))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                blocked = true;
                continue;
            }
            let p = group[0];
            if p.draft {
                out.blocks.push(format!(
                    "pull request {} (#{n}) is a draft. Marking it ready is your review of it: review it and mark it ready before merging",
                    pr_ref(p, &snap.hub)
                ));
                blocked = true;
                continue;
            }
            if p.base != default_of(p) && !heads.contains(&format!("{key}:{}", p.base)) {
                out.warnings.push(format!(
                    "pull request {} (#{n}) is based on \"{}\", which is neither the default branch nor the branch of an earlier pull request of the queue in that repository: check the stack before confirming",
                    pr_ref(p, &snap.hub),
                    p.base
                ));
            }
            own.push(p);
        }
        if blocked {
            continue;
        }
        for p in own {
            heads.insert(format!("{}:{}", repo_key(p), p.head));
            out.queue.push(Queued {
                item: *n,
                title: super::contract::strip_tag(&item.title).to_string(),
                pull: p.clone(),
                default: default_of(p),
            });
        }
    }
    if !out.without_pr.is_empty() {
        out.warnings.push(format!(
            "no pull request for {}: left out; if a pull request of the queue was built on their code, the merge takes that code with it",
            out.without_pr
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

/// The plan as lines for a reader.
pub fn plan_lines(p: &MergePlan, hub: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !p.queue.is_empty() {
        out.push("merge order (merge commit):".to_string());
    }
    for q in &p.queue {
        let retarget = if q.pull.base != q.default {
            format!("  (base {} → {})", q.pull.base, q.default)
        } else {
            String::new()
        };
        out.push(format!(
            "  {} · #{} {}{retarget}",
            pr_ref(&q.pull, hub),
            q.item,
            q.title
        ));
    }
    for (n, pull) in &p.done {
        out.push(format!("  merged already: {} · #{n}", pr_ref(pull, hub)));
    }
    for w in &p.warnings {
        out.push(format!("! {w}"));
    }
    for b in &p.blocks {
        out.push(format!("✗ {b}"));
    }
    out
}

/// The repository of a pull request as `gh --repo` takes it.
fn repo_of<'a>(pull: &'a Pull, hub: &'a str) -> &'a str {
    if pull.repo.is_empty() {
        hub
    } else {
        &pull.repo
    }
}

/// The default branch of each code repository the Stories' pull requests
/// are in, read from GitHub. One that cannot be read is a warning and
/// takes the hub's.
pub fn default_branches(
    ops: &Ops,
    cwd: &Path,
    snap: &Snapshot,
    sorted: &[u64],
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut out = BTreeMap::new();
    let mut warnings = Vec::new();
    let repos: BTreeSet<String> = sorted
        .iter()
        .filter_map(|n| snap.item(*n))
        .flat_map(|i| i.pulls.iter())
        .filter(|p| !p.repo.is_empty() && !p.repo.eq_ignore_ascii_case(&snap.hub))
        .map(|p| p.repo.clone())
        .collect();
    for repo in repos {
        let read = gh(
            ops.tools,
            cwd,
            &["api", &format!("repos/{repo}"), "--jq", ".default_branch"],
            "gh api (default branch)",
        );
        match read {
            Ok(b) if !b.trim().is_empty() => {
                out.insert(repo.to_lowercase(), b.trim().to_string());
            }
            _ => warnings.push(format!(
                "the default branch of {repo} was not read: taking \"{}\"",
                snap.default_branch
            )),
        }
    }
    (out, warnings)
}

/// A pull request read just now.
fn fresh(ops: &Ops, cwd: &Path, repo: &str, number: u64) -> Result<Value, String> {
    let text = gh(
        ops.tools,
        cwd,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "state,isDraft,baseRefName,headRefName,commits",
        ],
        "gh pr view",
    )?;
    json(&text, "gh pr view")
}

/// Whether no commit of the pull request names the item in a
/// `Spec-Wave-Story` trailer: a report, never a guard.
fn lacks_trailer(pr: &Value, item: u64) -> bool {
    let messages: Vec<String> = pr
        .get("commits")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    format!(
                        "{}\n\n{}",
                        c.get("messageHeadline")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        c.get("messageBody")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if messages.is_empty() {
        return false;
    }
    !missing_trailers(&messages, &[item], &[]).0.is_empty()
}

/// Merges the queue in order. Stops at the first refusal: nothing after
/// it is merged. Branches are deleted at the end unless `keep_branches`.
/// `Err` carries the report of what was done before the stop.
pub fn execute(
    ops: &Ops,
    cwd: &Path,
    snap: &Snapshot,
    plan: &MergePlan,
    keep_branches: bool,
) -> Result<Report, (Report, String)> {
    let mut r = Report::default();
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut stop: Option<String> = None;
    for q in &plan.queue {
        let repo = repo_of(&q.pull, &snap.hub);
        let name = pr_ref(&q.pull, &snap.hub);
        let n = q.pull.number.to_string();
        // the queue may have aged between the plan and here
        let now = match fresh(ops, cwd, repo, q.pull.number) {
            Ok(v) => v,
            Err(e) => {
                stop = Some(format!("pull request {name} was not read: {e}"));
                break;
            }
        };
        let field = |k: &str| now.get(k).and_then(Value::as_str).unwrap_or_default();
        if field("state") == "MERGED" {
            r.say(format!("pull request {name} was merged already"));
        } else {
            if now.get("isDraft").and_then(Value::as_bool) == Some(true) {
                stop = Some(format!(
                    "pull request {name} (#{}) became a draft again: nothing after it was merged",
                    q.item
                ));
                break;
            }
            let story = snap.item(q.item).and_then(|i| i.item_type) == Some(ItemType::Story);
            if story && lacks_trailer(&now, q.item) {
                r.say(format!(
                    "! pull request {name}: no commit carries `{}: #{}`; it was merged as asked, check it is the right work",
                    super::contract::TRAILER_STORY,
                    q.item
                ));
            }
            if field("baseRefName") != q.default {
                if let Err(e) = gh(
                    ops.tools,
                    cwd,
                    &["pr", "edit", &n, "--repo", repo, "--base", &q.default],
                    "gh pr edit",
                ) {
                    stop = Some(format!(
                        "the base of pull request {name} was not changed to {}: {e}",
                        q.default
                    ));
                    break;
                }
                r.say(format!(
                    "pull request {name}: base {} → {}",
                    field("baseRefName"),
                    q.default
                ));
            }
            // a merge commit, never a squash: a squash rewrites the merge
            // base of the pull requests stacked on this one
            if let Err(e) = gh(
                ops.tools,
                cwd,
                &["pr", "merge", &n, "--repo", repo, "--merge"],
                "gh pr merge",
            ) {
                stop = Some(format!(
                    "the merge of pull request {name} (#{}) was refused: {e}. Nothing after it was merged. Resolve it (checks, a conflict) and run the command again: what merged leaves the plan",
                    q.item
                ));
                break;
            }
            r.say(format!("pull request {name} merged (#{})", q.item));
            if !q.pull.head.is_empty() {
                merged.push((repo.to_string(), q.pull.head.clone()));
            }
        }
        // the board: a merged item is in QA. A failure here undoes no merge
        let board = ops.merged_board(q.item);
        r.lines.extend(board.lines);
        for t in board.touched {
            r.touch(t);
        }
        r.touch(q.item);
    }
    // branches only at the end, when no open pull request is based on one
    if !keep_branches {
        for (repo, head) in &merged {
            match gh(
                ops.tools,
                cwd,
                &[
                    "api",
                    "-X",
                    "DELETE",
                    &format!("repos/{repo}/git/refs/heads/{head}"),
                ],
                "gh api (delete branch)",
            ) {
                Ok(_) => r.say(format!("branch {head} deleted ({repo})")),
                Err(e) => r.say(format!("! branch {head} ({repo}) was not deleted: {e}")),
            }
        }
    }
    match stop {
        Some(why) => Err((r, why)),
        None => Ok(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::deliver::Tools;
    use crate::project::config::{self, ProjectConfig};
    use crate::project::ops::read_items;
    use crate::project::ops::tests::{CONFIG, fake_gh, node, nodes};
    use std::path::PathBuf;

    fn pull(n: u64, state: &str, draft: bool, head: &str, base: &str) -> Pull {
        Pull {
            repo: "acme/hub".into(),
            number: n,
            url: format!("https://github.com/acme/hub/pull/{n}"),
            state: state.into(),
            draft,
            head: head.into(),
            base: base.into(),
            review: String::new(),
        }
    }

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
        snap: Snapshot,
    }

    /// Feature #3 with Stories #12 and #15 (#15 depends on #12), both in
    /// code review, and Bug #40.
    fn fx(arms: &str) -> Fx {
        let feature = format!(
            r#"{},"subIssues":{{"nodes":[{{"number":12}},{{"number":15}}]}}"#,
            node(
                3,
                "[FEATURE] Pedidos",
                &["[FEATURE]"],
                Some("👀 Code Review"),
                ""
            )
        );
        let first = format!(
            r#"{},"parent":{{"number":3}}"#,
            node(12, "[STORY] ver", &["[STORY]"], Some("👀 Code Review"), "")
        );
        let second = format!(
            r#"{},"parent":{{"number":3}}"#,
            node(
                15,
                "[STORY] filtrar",
                &["[STORY]"],
                Some("👀 Code Review"),
                ""
            )
        );
        let bug = node(
            40,
            "[BUG] total errado",
            &["[BUG]"],
            Some("👀 Code Review"),
            "",
        );
        let json = nodes(&[feature, first, second, bug]);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let tools = fake_gh(&root, &json, arms);
        let cfg = config::parse(CONFIG, root.join(".spec-wave.json")).unwrap();
        let mut items = read_items(&tools, &cfg, &[3, 12, 15, 40]).unwrap();
        for i in &mut items {
            match i.number {
                12 => i.pulls = vec![pull(70, "OPEN", false, "story/12-ver", "main")],
                15 => {
                    i.depends_on = vec![12];
                    i.pulls = vec![pull(71, "OPEN", false, "story/15-filtrar", "story/12-ver")];
                }
                40 => i.pulls = vec![pull(80, "OPEN", false, "fix/40-total", "main")],
                _ => {}
            }
        }
        std::fs::remove_file(root.join("calls")).ok();
        let snap = Snapshot {
            hub: "acme/hub".into(),
            default_branch: "main".into(),
            items,
            ..Default::default()
        };
        Fx {
            _dir: dir,
            root,
            tools,
            cfg,
            snap,
        }
    }

    impl Fx {
        fn ops(&self) -> Ops<'_> {
            Ops {
                tools: &self.tools,
                cfg: &self.cfg,
                seen_board: None,
            }
        }

        fn item(&mut self, n: u64) -> &mut Item {
            self.snap.items.iter_mut().find(|i| i.number == n).unwrap()
        }

        /// The calls that were not reads of issues.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.contains("issue(number:"))
                .map(|l| l.trim().to_string())
                .collect()
        }
    }

    const PR_ARMS: &str = r#"  "pr view 70 "*) echo '{"state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"story/12-ver","commits":[{"messageHeadline":"feat: ver","messageBody":"Spec-Wave-Story: #12"}]}' ;;
  "pr view 71 "*) echo '{"state":"OPEN","isDraft":false,"baseRefName":"story/12-ver","headRefName":"story/15-filtrar","commits":[{"messageHeadline":"feat: filtrar","messageBody":"no trailer here"}]}' ;;
  "pr view 80 "*) echo '{"state":"MERGED","isDraft":false,"baseRefName":"main","headRefName":"fix/40-total","commits":[]}' ;;
  "pr edit "*|"api -X DELETE "*) echo ok ;;"#;

    #[test]
    fn a_feature_is_merged_in_the_order_of_its_stories() {
        let f = fx("");
        assert_eq!(scope(&f.snap, 3).unwrap(), vec![12, 15]);
        assert_eq!(scope(&f.snap, 15).unwrap(), vec![15]);
        assert_eq!(scope(&f.snap, 40).unwrap(), vec![40]);
        let p = plan(&[12, 15], &f.snap, &BTreeMap::new());
        assert!(p.blocks.is_empty() && p.warnings.is_empty(), "{p:?}");
        assert_eq!(
            plan_lines(&p, "acme/hub"),
            vec![
                "merge order (merge commit):",
                "  #70 · #12 ver",
                "  #71 · #15 filtrar  (base story/12-ver → main)",
            ]
        );
    }

    #[test]
    fn a_cycle_has_no_merge_order_and_a_task_is_not_merged() {
        let mut f = fx("");
        f.item(12).depends_on = vec![15];
        let e = scope(&f.snap, 3).unwrap_err();
        assert!(e.contains("#12, #15") && e.contains("cycle"), "{e}");
        f.item(12).item_type = Some(ItemType::Task);
        assert!(scope(&f.snap, 12).unwrap_err().contains("a merge takes"));
    }

    #[test]
    fn a_draft_blocks_the_whole_plan() {
        let mut f = fx("");
        f.item(15).pulls[0].draft = true;
        let p = plan(&[12, 15], &f.snap, &BTreeMap::new());
        assert_eq!(p.queue.len(), 1);
        assert_eq!(p.blocks.len(), 1);
        assert!(p.blocks[0].contains("#71") && p.blocks[0].contains("draft"));
    }

    #[test]
    fn two_open_pull_requests_in_one_repository_are_ambiguous() {
        let mut f = fx("");
        f.item(12)
            .pulls
            .push(pull(72, "OPEN", false, "story/12-outra", "main"));
        let p = plan(&[12], &f.snap, &BTreeMap::new());
        assert!(p.queue.is_empty());
        assert!(p.blocks[0].contains("#70, #72"), "{:?}", p.blocks);
        // one per repository is a Story across repositories
        let mut f = fx("");
        let mut other = pull(9, "OPEN", false, "story/12-ver", "trunk");
        other.repo = "acme/api".into();
        f.item(12).pulls.push(other);
        let defaults = BTreeMap::from([("acme/api".to_string(), "trunk".to_string())]);
        let p = plan(&[12], &f.snap, &defaults);
        assert!(p.blocks.is_empty() && p.warnings.is_empty(), "{p:?}");
        let refs: Vec<String> = p
            .queue
            .iter()
            .map(|q| pr_ref(&q.pull, "acme/hub"))
            .collect();
        assert_eq!(refs, vec!["#70", "acme/api#9"]);
    }

    #[test]
    fn what_merged_leaves_the_plan_and_what_has_no_pull_request_is_said() {
        let mut f = fx("");
        f.item(12).pulls[0].state = "MERGED".into();
        f.item(15).pulls.clear();
        let p = plan(&[12, 15], &f.snap, &BTreeMap::new());
        assert!(p.queue.is_empty());
        assert_eq!(p.done.len(), 1);
        assert_eq!(p.without_pr, vec![15]);
        assert!(p.warnings[0].contains("no pull request for #15"));
        // a base nobody in the queue owns is a warning
        let mut f = fx("");
        f.item(15).pulls[0].base = "elsewhere".into();
        let p = plan(&[15], &f.snap, &BTreeMap::new());
        assert!(p.warnings[0].contains("\"elsewhere\""), "{:?}", p.warnings);
    }

    #[test]
    fn the_queue_is_retargeted_merged_moved_and_its_branches_go_last() {
        let f = fx(&format!("{PR_ARMS}\n  \"pr merge \"*) echo ok ;;"));
        let p = plan(&[12, 15], &f.snap, &BTreeMap::new());
        let r = execute(&f.ops(), &f.root, &f.snap, &p, false).unwrap();
        let calls = f.calls();
        let writes: Vec<&String> = calls.iter().filter(|c| !c.starts_with("pr view")).collect();
        let at = |needle: &str| {
            writes
                .iter()
                .position(|c| c.contains(needle))
                .unwrap_or_else(|| panic!("no call with {needle}: {writes:#?}"))
        };
        // the first is merged before the second is retargeted, and no
        // branch is deleted before the last merge
        assert_eq!(writes[0].as_str(), "pr merge 70 --repo acme/hub --merge");
        assert!(at("pr merge 70") < at("pr edit 71 --repo acme/hub --base main"));
        assert!(at("pr edit 71") < at("pr merge 71 --repo acme/hub --merge"));
        assert!(at("pr merge 71") < at("git/refs/heads/story/12-ver"));
        assert!(at("git/refs/heads/story/12-ver") < at("git/refs/heads/story/15-filtrar"));
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("--delete-branch") || c.contains("--squash"))
        );
        // both Stories reached QA, so the Feature followed the second
        assert!(
            r.lines.contains(&"Story #12 → 🧪 QA / Todo".to_string()),
            "{r:?}"
        );
        assert!(r.lines.contains(&"Story #15 → 🧪 QA / Todo".to_string()));
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("#71") && l.contains("Spec-Wave-Story: #15")),
            "{r:?}"
        );
        assert!(r.touched.contains(&12) && r.touched.contains(&15));
    }

    #[test]
    fn a_refused_merge_stops_the_queue_and_keeps_the_later_branches() {
        let f = fx(&format!(
            "{PR_ARMS}\n  \"pr merge 70 \"*) echo ok ;;\n  \"pr merge 71 \"*) echo 'checks pending' >&2; exit 1 ;;"
        ));
        let p = plan(&[12, 15], &f.snap, &BTreeMap::new());
        let (r, why) = execute(&f.ops(), &f.root, &f.snap, &p, false).unwrap_err();
        assert!(
            why.contains("#71") && why.contains("checks pending"),
            "{why}"
        );
        assert!(r.lines.iter().any(|l| l == "pull request #70 merged (#12)"));
        let calls = f.calls();
        // what merged loses its branch; the refused one keeps its own
        assert!(
            calls
                .iter()
                .any(|c| c.contains("git/refs/heads/story/12-ver"))
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("git/refs/heads/story/15-filtrar"))
        );
    }

    #[test]
    fn a_pull_request_merged_meanwhile_only_moves_the_board() {
        let f = fx(PR_ARMS);
        let p = plan(&[40], &f.snap, &BTreeMap::new());
        let r = execute(&f.ops(), &f.root, &f.snap, &p, true).unwrap();
        assert_eq!(
            r.lines,
            vec![
                "pull request #80 was merged already",
                "Bug #40 → 🧪 QA / Todo"
            ]
        );
        assert!(
            !f.calls()
                .iter()
                .any(|c| c.starts_with("pr merge") || c.contains("DELETE"))
        );
    }
}
