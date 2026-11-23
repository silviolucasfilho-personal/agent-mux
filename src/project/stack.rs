//! A Feature as a stack (spec §7.2): its pending Stories built one at a
//! time in dependency order, each on the branch of the Story before it in
//! the same repository, every pull request a draft until the last Story is
//! delivered, when all are marked ready together.
//!
//! Ported from spec-wave's `commands/implement.mjs`
//! (`planFeatureImplementation`, `featureCanAdvance`) and
//! `lib/pr-ready.mjs` (`planPrsToReady`, `collectFeaturePullRequests`,
//! `markFeaturePrsReady`), read on 2026-11-23. Where it differs: spec-wave
//! runs one session for the whole Feature and has the model open the pull
//! requests; here each Story is its own run (work, then an independent
//! review) and Rust delivers it, so a stack that stops resumes at the
//! Story it stopped on.
//!
//! `gh` 2.98.0, probed with `--help` on 2026-11-23: `gh pr ready <number>
//! --repo OWNER/REPO` marks a draft ready; it fails on one that already
//! is, so only drafts are passed to it.

use super::config::ProjectConfig;
use super::contract::*;
use super::github;
use super::ops::Report;
use super::{Item, ItemType, Pull, Snapshot, order, stage};
use crate::factory::deliver::Tools;
use serde::Serialize;
use serde_json::Value;

/// One Story of a stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackStory {
    pub number: u64,
    pub title: String,
    pub stage: String,
    pub tasks: usize,
}

/// A Feature's stack, planned from the snapshot. Pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackPlan {
    pub feature: u64,
    pub title: String,
    /// The Stories to build, in execution order.
    pub pending: Vec<StackStory>,
    /// Stories in code review or further on, and closed ones: not touched.
    pub skipped: Vec<StackStory>,
    /// Every Story of the Feature in order, for the base of each build.
    pub order: Vec<u64>,
}

/// Plans the stack of `feature`: what is pending, in order. `Err` when the
/// pending Stories depend on each other in a cycle, or one has no Task.
pub fn plan(snap: &Snapshot, feature: u64) -> Result<StackPlan, String> {
    let f = snap
        .item(feature)
        .ok_or_else(|| format!("issue #{feature} is not in the Project"))?;
    if f.item_type != Some(ItemType::Feature) {
        return Err(format!(
            "#{feature} is not a Feature: only a Feature is a stack"
        ));
    }
    let o = order::feature_order(snap, feature)?;
    let row = |r: &order::Row| StackStory {
        number: r.number,
        title: strip_tag(&r.title).to_string(),
        stage: r.stage.clone(),
        tasks: r.tasks,
    };
    let is_pending =
        |r: &order::Row| r.open && !stage::at_or_past(Some(&r.stage), STAGE_CODE_REVIEW);
    let pending: Vec<StackStory> = o.rows.iter().filter(|r| is_pending(r)).map(row).collect();
    let mut skipped: Vec<StackStory> = o.rows.iter().filter(|r| !is_pending(r)).map(row).collect();
    skipped.sort_by_key(|s| s.number);
    // a cycle keeps its Stories out of the order: they are pending all the same
    let in_cycle: Vec<u64> = o
        .cycle
        .iter()
        .copied()
        .filter(|n| {
            snap.item(*n).is_some_and(|i| {
                i.open && !stage::at_or_past(Some(&stage::shown(i).stage), STAGE_CODE_REVIEW)
            })
        })
        .collect();
    if !in_cycle.is_empty() {
        return Err(format!(
            "the Stories {} depend on each other in a cycle: fix their `Depende de:` lines before building the Feature",
            refs(&in_cycle)
        ));
    }
    let bare: Vec<u64> = pending
        .iter()
        .filter(|s| s.tasks == 0)
        .map(|s| s.number)
        .collect();
    if !bare.is_empty() {
        return Err(format!(
            "the pending Stor{} {} ha{} no Task: decompose {} before building the Feature",
            if bare.len() == 1 { "y" } else { "ies" },
            refs(&bare),
            if bare.len() == 1 { "s" } else { "ve" },
            if bare.len() == 1 { "it" } else { "them" },
        ));
    }
    Ok(StackPlan {
        feature,
        title: strip_tag(&f.title).to_string(),
        pending,
        skipped,
        order: o.rows.iter().map(|r| r.number).collect(),
    })
}

fn refs(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The plan as lines for a terminal.
pub fn plan_lines(p: &StackPlan) -> Vec<String> {
    let mut out = vec![format!(
        "next: build the {} pending Stor{} of Feature #{} as a stack, one at a time, in this order",
        p.pending.len(),
        if p.pending.len() == 1 { "y" } else { "ies" },
        p.feature
    )];
    for (i, s) in p.pending.iter().enumerate() {
        out.push(format!(
            "  {}. Story #{} {} ({} Task(s))",
            i + 1,
            s.number,
            s.title,
            s.tasks
        ));
    }
    if !p.skipped.is_empty() {
        out.push("not touched (in code review or further on):".into());
        for s in &p.skipped {
            out.push(format!(
                "  Story #{} {} · {}",
                s.number,
                s.title,
                stage_plain(&s.stage)
            ));
        }
    }
    out.push(
        "each Story's branch forks from the Story before it in the same repository; every pull request stays a draft".into(),
    );
    out.push(
        "then: when the last Story is delivered, the Feature's pull requests are marked ready together and the Feature moves to Code Review".into(),
    );
    out
}

fn same_repo(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The open pull request of `item` in `repo`, if it has one.
fn open_pull_in<'a>(item: &'a Item, repo: &str) -> Option<&'a Pull> {
    item.pulls
        .iter()
        .find(|p| p.open() && same_repo(&p.repo, repo))
}

/// What a Story's build in `repo` (`owner/repo`) forks from: the branch of
/// the nearest Story before it, in the Feature's order, that has an open
/// pull request in that repository. `None`: the default branch.
pub fn base_for(snap: &Snapshot, order: &[u64], story: u64, repo: &str) -> Option<(u64, String)> {
    let at = order.iter().position(|n| *n == story)?;
    order[..at].iter().rev().find_map(|n| {
        let p = open_pull_in(snap.item(*n)?, repo)?;
        Some((*n, p.head.clone()))
    })
}

/// The dependencies of `item` whose code is on an open pull request in
/// `repo`: their branches must be under what the build forks from.
pub fn under(snap: &Snapshot, item: &Item, repo: &str) -> Vec<(u64, String)> {
    item.depends_on
        .iter()
        .filter_map(|n| {
            let p = open_pull_in(snap.item(*n)?, repo)?;
            Some((*n, p.head.clone()))
        })
        .collect()
}

/// The pull requests to mark ready (spec-wave's `planPrsToReady`): open
/// drafts, once each, by repository then number. Pure.
pub fn drafts(pulls: &[Pull]) -> Vec<Pull> {
    let mut out: Vec<Pull> = Vec::new();
    for p in pulls {
        if p.open()
            && p.draft
            && !out
                .iter()
                .any(|x| same_repo(&x.repo, &p.repo) && x.number == p.number)
        {
            out.push(p.clone());
        }
    }
    out.sort_by(|a, b| {
        a.repo
            .to_lowercase()
            .cmp(&b.repo.to_lowercase())
            .then(a.number.cmp(&b.number))
    });
    out
}

/// Whether every Story of the Feature is in code review or further on
/// (spec-wave's `featureCanAdvance`): a Story whose stage is unknown
/// counts as pending. A Feature with no Story is not ready.
pub fn all_in_review(stories: &[Item]) -> bool {
    let open: Vec<&Item> = stories
        .iter()
        .filter(|s| s.item_type == Some(ItemType::Story) && s.open)
        .collect();
    !stories.is_empty()
        && open
            .iter()
            .all(|s| stage::at_or_past(s.stage.as_deref(), STAGE_CODE_REVIEW))
}

fn pull_of(v: &Value, repo: &str) -> Option<Pull> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(Pull {
        repo: repo.to_string(),
        number: v.get("number").and_then(Value::as_u64)?,
        url: text("url"),
        state: text("state").to_uppercase(),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: text("headRefName"),
        base: text("baseRefName"),
        review: text("reviewDecision"),
    })
}

/// The open pull requests that close one of `issues`, read now from the
/// hub and every code repository, by what their text says (`Closes #n`,
/// `Closes owner/hub#n`): GitHub's own link is recorded only for a pull
/// request based on the default branch, and a stack's upper ones are not.
pub fn collect(tools: &Tools, cfg: &ProjectConfig, issues: &[u64]) -> (Vec<Pull>, Vec<String>) {
    let hub = cfg.hub_repo().slug();
    let mut repos = vec![hub.clone()];
    for r in &cfg.repositories {
        let slug = r.repo.slug();
        if !repos.iter().any(|x| same_repo(x, &slug)) {
            repos.push(slug);
        }
    }
    let mut pulls = Vec::new();
    let mut warnings = Vec::new();
    for repo in &repos {
        match github::pull_list(tools, cfg.root(), repo, "open", 200) {
            Ok(list) => {
                for p in &list {
                    let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
                    let said = format!("{}\n{}", text("title"), text("body"));
                    if closing_refs(&said, repo, &hub)
                        .iter()
                        .any(|n| issues.contains(n))
                        && let Some(pull) = pull_of(p, repo)
                    {
                        pulls.push(pull);
                    }
                }
            }
            Err(e) => warnings.push(format!("pull requests of {repo} were not read: {e}")),
        }
    }
    (pulls, warnings)
}

/// Marks the Feature's draft pull requests ready, when every one of its
/// Stories is in code review or further on. Best effort, as the board
/// moves of a build are: the work is delivered already, and a draft that
/// stayed one is a line naming it. `stories` are the Feature's, read now.
pub fn mark_ready(tools: &Tools, cfg: &ProjectConfig, feature: u64, stories: &[Item]) -> Report {
    let mut r = Report::default();
    if !all_in_review(stories) {
        return r;
    }
    let mut issues = vec![feature];
    issues.extend(stories.iter().map(|s| s.number));
    let (pulls, warnings) = collect(tools, cfg, &issues);
    for w in warnings {
        r.say(format!("! {w}"));
    }
    let hub = cfg.hub_repo().slug();
    let mut marked = Vec::new();
    let mut kept = Vec::new();
    for p in drafts(&pulls) {
        let name = super::merge::pr_ref(&p, &hub);
        match github::gh(
            tools,
            cfg.root(),
            &["pr", "ready", &p.number.to_string(), "--repo", &p.repo],
            "gh pr ready",
        ) {
            Ok(_) => marked.push(name),
            Err(e) => kept.push(format!("{name} ({e})")),
        }
    }
    if !marked.is_empty() {
        r.say(format!(
            "every Story of Feature #{feature} is in code review: marked ready for review {}",
            marked.join(", ")
        ));
    }
    if !kept.is_empty() {
        r.say(format!(
            "! still drafts, mark them ready by hand: {}",
            kept.join(", ")
        ));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::config;
    use crate::project::next::tests::item as titled;
    use crate::project::ops::tests::{CONFIG, fake_gh};

    fn item(n: u64, t: ItemType, stage: Option<&str>) -> Item {
        let title = match t {
            ItemType::Feature => "[FEATURE] f",
            ItemType::Story => "[STORY] s",
            _ => "[TASK] t",
        };
        let mut i = titled(n, title, &[]);
        i.item_type = Some(t);
        i.stage = stage.map(str::to_string);
        i
    }

    /// A recording `gh` that answers `pr list` with `pulls`.
    struct Rec {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
    }

    fn recorder(pulls: &str) -> Rec {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("pulls.json"), pulls).unwrap();
        let tools = fake_gh(
            &root,
            "{}",
            r#"  "pr list"*) cat "$d/pulls.json" ;;
  "pr ready"*) echo ok ;;"#,
        );
        let cfg = config::parse(CONFIG, root.join(".spec-wave.json")).unwrap();
        Rec {
            _dir: dir,
            root,
            tools,
            cfg,
        }
    }

    impl Rec {
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        }
    }

    fn pull(repo: &str, number: u64, head: &str, draft: bool, state: &str) -> Pull {
        Pull {
            repo: repo.into(),
            number,
            url: format!("https://github.com/{repo}/pull/{number}"),
            state: state.into(),
            draft,
            head: head.into(),
            base: "main".into(),
            review: String::new(),
        }
    }

    fn story(n: u64, stage: &str, deps: &[u64], tasks: &[u64]) -> Item {
        let mut i = item(n, ItemType::Story, Some(stage));
        i.title = format!("[STORY] s{n}");
        i.parent = Some(3);
        i.depends_on = deps.to_vec();
        i.children = tasks.to_vec();
        i
    }

    fn task(n: u64, parent: u64) -> Item {
        let mut i = item(n, ItemType::Task, Some(STAGE_READY));
        i.parent = Some(parent);
        i
    }

    fn snap(items: Vec<Item>) -> Snapshot {
        let mut f = item(3, ItemType::Feature, Some(STAGE_READY));
        f.title = "[FEATURE] Pedidos".into();
        f.children = items
            .iter()
            .filter(|i| i.item_type == Some(ItemType::Story))
            .map(|i| i.number)
            .collect();
        let mut all = vec![f];
        all.extend(items);
        Snapshot {
            hub: "acme/hub".into(),
            items: all,
            ..Default::default()
        }
    }

    #[test]
    fn the_pending_stories_are_planned_in_dependency_order() {
        let mut done = story(10, STAGE_CODE_REVIEW, &[], &[101]);
        done.pulls = vec![pull("acme/api", 7, "story/10-s10", true, "OPEN")];
        let s = snap(vec![
            done,
            story(12, STAGE_READY, &[11], &[121]),
            story(11, STAGE_READY, &[10], &[111, 112]),
            task(101, 10),
            task(111, 11),
            task(112, 11),
            task(121, 12),
        ]);
        let p = plan(&s, 3).unwrap();
        let pending: Vec<u64> = p.pending.iter().map(|x| x.number).collect();
        assert_eq!(pending, vec![11, 12]);
        assert_eq!(p.pending[0].tasks, 2);
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].number, 10);
        assert_eq!(p.order, vec![10, 11, 12]);
        let text = plan_lines(&p).join("\n");
        assert!(
            text.contains("build the 2 pending Stories of Feature #3"),
            "{text}"
        );
        assert!(text.contains("  1. Story #11 s11 (2 Task(s))"), "{text}");
        assert!(text.contains("not touched"), "{text}");
        assert!(text.contains("Story #10 s10"), "{text}");

        // the base: the Story before it with an open pull request in the repository
        assert_eq!(
            base_for(&s, &p.order, 11, "Acme/API"),
            Some((10, "story/10-s10".into()))
        );
        // nothing open below it in another repository: the default branch
        assert_eq!(base_for(&s, &p.order, 11, "acme/web"), None);
        // the Story between has no pull request yet: the one before it is the base
        assert_eq!(
            base_for(&s, &p.order, 12, "acme/api"),
            Some((10, "story/10-s10".into()))
        );
        assert_eq!(base_for(&s, &p.order, 10, "acme/api"), None);
        assert_eq!(
            under(&s, s.item(11).unwrap(), "acme/api"),
            vec![(10, "story/10-s10".to_string())]
        );
        assert!(under(&s, s.item(12).unwrap(), "acme/api").is_empty());
    }

    #[test]
    fn a_cycle_and_a_story_with_no_task_stop_the_plan() {
        let s = snap(vec![
            story(11, STAGE_READY, &[12], &[111]),
            story(12, STAGE_READY, &[11], &[121]),
            task(111, 11),
            task(121, 12),
        ]);
        let e = plan(&s, 3).unwrap_err();
        assert!(e.contains("#11, #12") && e.contains("cycle"), "{e}");

        let s = snap(vec![
            story(11, STAGE_READY, &[], &[111]),
            story(12, STAGE_READY, &[], &[]),
            task(111, 11),
        ]);
        let e = plan(&s, 3).unwrap_err();
        assert!(e.contains("Story #12 has no Task"), "{e}");
        assert!(plan(&s, 11).unwrap_err().contains("not a Feature"));
    }

    #[test]
    fn only_open_drafts_are_marked_once_each_by_repository() {
        let pulls = vec![
            pull("acme/web", 4, "b", true, "OPEN"),
            pull("acme/api", 9, "c", true, "OPEN"),
            pull("acme/api", 2, "a", true, "OPEN"),
            pull("Acme/API", 2, "a", true, "OPEN"),
            pull("acme/api", 5, "d", false, "OPEN"),
            pull("acme/api", 6, "e", true, "MERGED"),
            pull("acme/api", 8, "f", true, "CLOSED"),
        ];
        let got: Vec<(String, u64)> = drafts(&pulls)
            .into_iter()
            .map(|p| (p.repo, p.number))
            .collect();
        assert_eq!(
            got,
            vec![
                ("acme/api".to_string(), 2),
                ("acme/api".to_string(), 9),
                ("acme/web".to_string(), 4)
            ]
        );
    }

    #[test]
    fn a_feature_is_in_review_when_every_open_story_is() {
        let review = story(11, STAGE_CODE_REVIEW, &[], &[]);
        let qa = story(12, STAGE_QA, &[], &[]);
        let ready = story(13, STAGE_READY, &[], &[]);
        let mut unknown = story(14, STAGE_READY, &[], &[]);
        unknown.stage = None;
        let mut closed = story(15, STAGE_READY, &[], &[]);
        closed.open = false;
        assert!(all_in_review(&[review.clone(), qa.clone()]));
        assert!(all_in_review(&[review.clone(), closed]));
        assert!(!all_in_review(&[review.clone(), ready]));
        assert!(!all_in_review(&[review, unknown]));
        assert!(!all_in_review(&[]));
    }

    #[test]
    fn the_drafts_of_a_finished_feature_are_marked_ready_and_no_other() {
        // the hub's list has a stacked pull request GitHub does not link,
        // one of another Feature, and one that is ready already
        let hub_pulls = serde_json::json!([
            {"number": 21, "title": "s11", "body": "Closes #11", "headRefName": "story/11-s11",
             "baseRefName": "main", "isDraft": true, "state": "OPEN", "url": "u21"},
            {"number": 22, "title": "s12", "body": "Closes acme/hub#12", "headRefName": "story/12-s12",
             "baseRefName": "story/11-s11", "isDraft": true, "state": "OPEN", "url": "u22"},
            {"number": 23, "title": "other", "body": "Closes #99", "headRefName": "x",
             "baseRefName": "main", "isDraft": true, "state": "OPEN", "url": "u23"},
            {"number": 24, "title": "s11 docs", "body": "Closes #11", "headRefName": "y",
             "baseRefName": "main", "isDraft": false, "state": "OPEN", "url": "u24"}
        ])
        .to_string();
        let rec = recorder(&hub_pulls);
        let stories = vec![
            story(11, STAGE_CODE_REVIEW, &[], &[]),
            story(12, STAGE_CODE_REVIEW, &[], &[]),
        ];
        let r = mark_ready(&rec.tools, &rec.cfg, 3, &stories);
        let calls = rec.calls();
        let ready: Vec<&String> = calls.iter().filter(|c| c.starts_with("pr ready")).collect();
        assert_eq!(
            ready,
            vec!["pr ready 21 --repo acme/hub", "pr ready 22 --repo acme/hub"],
            "{calls:?}"
        );
        assert!(
            r.lines[0].contains("marked ready for review #21, #22"),
            "{:?}",
            r.lines
        );

        // a Story still behind: nothing is read and nothing is marked
        let rec = recorder(&hub_pulls);
        let behind = vec![
            story(11, STAGE_CODE_REVIEW, &[], &[]),
            story(12, STAGE_DEVELOPMENT, &[], &[]),
        ];
        let r = mark_ready(&rec.tools, &rec.cfg, 3, &behind);
        assert!(r.lines.is_empty());
        assert!(rec.calls().is_empty(), "{:?}", rec.calls());
    }
}
