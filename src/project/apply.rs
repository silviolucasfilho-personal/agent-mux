//! Applying a decomposition: the reviewed `decomposition.md` becomes
//! Story and Task issues. No model takes part, and it runs only on the
//! operator's word: applying is the approval.
//!
//! What it writes, in spec-wave's order and words (`commands/decompose.mjs`,
//! read on 2026-11-18): each Story as `[STORY] <title>` with its user
//! story, body and a `Depende de: #a, #b` line, the Feature's milestone,
//! its `repo:<slug>` labels, the native blocked-by and sub-issue
//! relations, and the board's stage Ready; each Task as `[TASK] <title>`
//! under its Story, with `_Story pai: <url>_`; then the Feature in Ready,
//! the label `spec-wave:decomposed`, two comments, and the draft
//! annotated with the issues (`**Issue:** #n`, `applied=`) beside
//! `dependency-map.json`, on a pull request of its own.
//!
//! An apply that stops halfway must not duplicate what it made: every
//! created issue is journaled by its position in the draft, in a file
//! outside the repository, and a second run creates only what is missing.

use super::contract::*;
use super::decomposition::{self, Doc, Kind};
use super::deliver::{self, Artifact};
use super::github::Detail;
use super::ops::Report;
use super::run::Ctx;
use super::stage;
use super::write::{self, Board, Created};
use super::{Item, ItemType, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// An issue the apply created, as its journal keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Made {
    number: u64,
    node_id: String,
    id: u64,
    url: String,
}

impl From<Created> for Made {
    fn from(c: Created) -> Self {
        Made {
            number: c.number,
            node_id: c.node_id,
            id: c.id,
            url: c.url,
        }
    }
}

/// What an apply would create, for the operator to read first.
pub fn preview(doc: &Doc) -> Vec<String> {
    let mut out = Vec::new();
    match doc.kind {
        Kind::Stories => {
            let tasks: usize = doc.stories.iter().map(|s| s.tasks.len()).sum();
            out.push(format!(
                "{} Stories and {tasks} Tasks, each in ✅ Ready:",
                doc.stories.len()
            ));
            for (i, s) in doc.stories.iter().enumerate() {
                let mut deps: Vec<String> = s
                    .depends_on
                    .iter()
                    .map(|d| format!("Story {}", d + 1))
                    .collect();
                deps.extend(s.depends_on_issues.iter().map(|n| format!("#{n}")));
                out.push(format!(
                    "  Story {} — {} ({} tasks{}{})",
                    i + 1,
                    s.title,
                    s.tasks.len(),
                    if deps.is_empty() {
                        String::new()
                    } else {
                        format!(", after {}", deps.join(", "))
                    },
                    if s.repos.is_empty() {
                        String::new()
                    } else {
                        format!(", in {}", s.repos.join(", "))
                    }
                ));
            }
        }
        Kind::Tasks => {
            out.push(format!("{} Tasks, each in ✅ Ready:", doc.tasks.len()));
            out.extend(
                doc.tasks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("  Task {} — {}", i + 1, t.title)),
            );
        }
    }
    out
}

fn iso_now() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

impl<'a> Ctx<'a> {
    fn journal_path(&self, number: u64) -> PathBuf {
        let hub = self.cfg.hub_repo().slug().replace('/', "__");
        self.scratch.join(format!("apply-{hub}-{number}.json"))
    }

    /// The reviewed draft of `item`, from the default branch, and whether
    /// it may be applied: not twice, and not over children that exist.
    pub fn apply_plan(&self, item: &Item, snap: &Snapshot) -> Result<(Doc, String), String> {
        let t = item
            .item_type
            .filter(|t| matches!(t, ItemType::Feature | ItemType::Rfc))
            .ok_or("only a Feature or an RFC is decomposed")?;
        let path = doc_path(t, &item.title, "decomposition");
        let text = deliver::read_base_file(&self.hub_root, &path)?.ok_or_else(|| {
            format!(
                "`{path}` is not on the default branch: draft the decomposition and merge its pull request first"
            )
        })?;
        let doc = decomposition::parse(&text)?;
        if item.has_label(LABEL_DECOMPOSED) {
            return Err(format!(
                "#{} already has `{LABEL_DECOMPOSED}`: its decomposition was applied",
                item.number
            ));
        }
        if let Some(at) = &doc.applied_at {
            return Err(format!(
                "`{path}` says it was applied on {at}: its issues exist"
            ));
        }
        let child = if t == ItemType::Feature {
            ItemType::Story
        } else {
            ItemType::Task
        };
        let existing: Vec<String> = snap
            .children_of(item.number)
            .into_iter()
            .filter(|c| c.item_type == Some(child))
            .map(|c| format!("#{}", c.number))
            .collect();
        // a journal means an apply of this very draft stopped halfway:
        // the children are its own, and the rest is still to make
        if !existing.is_empty() && !self.journal_path(item.number).is_file() {
            return Err(format!(
                "#{} already has {} as sub-issues ({}): applying again would duplicate them",
                item.number,
                if child == ItemType::Story {
                    "Stories"
                } else {
                    "Tasks"
                },
                existing.join(", ")
            ));
        }
        if doc.kind == Kind::Stories && doc.stories.iter().any(|s| s.tasks.is_empty()) {
            return Err("a Story of the draft has no Task: add one or remove the Story".into());
        }
        let known: Vec<&str> = self
            .cfg
            .repositories
            .iter()
            .map(|r| r.slug.as_str())
            .collect();
        for s in &doc.stories {
            if let Some(bad) = s.repos.iter().find(|r| !known.contains(&r.as_str())) {
                return Err(format!(
                    "the draft names a repository `{bad}` that is not in .spec-wave.json's `repositories`"
                ));
            }
        }
        Ok((doc, path))
    }

    /// Creates the issues of `doc`. Call [`Ctx::apply_plan`] first.
    pub fn apply(
        &self,
        item: &Item,
        detail: &Detail,
        snap: &Snapshot,
        mut doc: Doc,
        path: &str,
    ) -> Result<Report, String> {
        let (tools, cfg) = (self.tools, self.cfg);
        let n = item.number;
        // without the board the issues would have no stage and show nowhere
        let board = Board::resolve(tools, cfg, snap.board_id.as_deref())
            .map_err(|e| format!("not applied: {e}. The issues would be born with no stage"))?;
        // a dependency on another Feature's issue must exist already
        let mut outside: BTreeMap<u64, u64> = BTreeMap::new();
        for dep in doc.stories.iter().flat_map(|s| s.depends_on_issues.iter()) {
            if !outside.contains_key(dep) {
                let id = write::issue_id(tools, cfg, *dep).map_err(|e| {
                    format!("not applied: `Depende de: #{dep}` names an issue that could not be read ({e})")
                })?;
                outside.insert(*dep, id);
            }
        }
        let milestone = item
            .milestone
            .as_ref()
            .and_then(|title| snap.milestones.iter().find(|m| &m.title == title))
            .map(|m| m.number);

        std::fs::create_dir_all(&self.scratch).map_err(|e| e.to_string())?;
        let journal_file = self.journal_path(n);
        let mut journal: BTreeMap<String, Made> = std::fs::read_to_string(&journal_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let resumed = !journal.is_empty();
        let mut report = Report::default();
        report.touched.push(n);
        if resumed {
            report.lines.push(format!(
                "resuming an apply that stopped: {} issue(s) were already created",
                journal.len()
            ));
        }
        let mut board_failures: Vec<String> = Vec::new();

        // one issue: created once, journaled before anything else is done to it
        let make = |journal: &mut BTreeMap<String, Made>,
                    key: String,
                    title: &str,
                    body: &str,
                    labels: &[String]|
         -> Result<(Made, bool), String> {
            if let Some(m) = journal.get(&key) {
                return Ok((m.clone(), false));
            }
            let made: Made =
                write::create_issue(tools, cfg, title, body, labels, milestone)?.into();
            journal.insert(key, made.clone());
            std::fs::write(
                &journal_file,
                serde_json::to_string_pretty(journal).unwrap_or_default(),
            )
            .map_err(|e| {
                format!(
                    "#{} was created but the journal was not written: {e}",
                    made.number
                )
            })?;
            Ok((made, true))
        };
        let place = |failures: &mut Vec<String>, made: &Made, t: ItemType| {
            let done = (|| -> Result<(), String> {
                let id = write::add_to_board(tools, cfg, &board, &made.node_id)?;
                if !write::set_field(tools, cfg, &board, &id, FIELD_STAGE, STAGE_READY)? {
                    return Err(format!("the board has no stage \"{STAGE_READY}\""));
                }
                write::set_field(tools, cfg, &board, &id, FIELD_TYPE, t.as_str())?;
                Ok(())
            })();
            if let Err(e) = done {
                failures.push(format!("#{} ({}): {e}", made.number, t.as_str()));
            }
        };

        let mut created_lines: Vec<String> = Vec::new();
        match doc.kind {
            Kind::Stories => {
                let mut stories: Vec<Made> = Vec::new();
                for i in 0..doc.stories.len() {
                    let story = doc.stories[i].clone();
                    let title = format!("{} {}", ItemType::Story.tag(), story.title);
                    let mut body = [story.user_story.trim(), story.body.trim()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if body.is_empty() {
                        body = "_(sem descrição)_".to_string();
                    }
                    let mut dep_numbers: Vec<u64> = story
                        .depends_on
                        .iter()
                        .filter_map(|d| stories.get(*d).map(|m| m.number))
                        .collect();
                    dep_numbers.extend(story.depends_on_issues.iter().copied());
                    let line = dependency_line(&dep_numbers);
                    if !line.is_empty() {
                        body.push_str(&format!("\n\n{line}"));
                    }
                    let mut labels = vec![ItemType::Story.tag().to_string()];
                    let repo_labels: Vec<String> = story
                        .repos
                        .iter()
                        .map(|r| format!("{REPO_LABEL_PREFIX}{r}"))
                        .collect();
                    labels.extend(repo_labels.iter().cloned());
                    let (made, fresh) =
                        make(&mut journal, format!("s{i}"), &title, &body, &labels)?;
                    doc.stories[i].issue = Some(made.number);
                    report.touched.push(made.number);
                    created_lines.push(format!("- {} — {title}", made.url));
                    if fresh {
                        // best effort: the `Depende de:` line already says it
                        let blockers = story
                            .depends_on
                            .iter()
                            .filter_map(|d| stories.get(*d).map(|m| m.id))
                            .chain(
                                story
                                    .depends_on_issues
                                    .iter()
                                    .filter_map(|x| outside.get(x).copied()),
                            );
                        for id in blockers {
                            let _ = write::add_blocked_by(tools, cfg, made.number, id);
                        }
                        if let Err(e) =
                            write::add_sub_issue(tools, cfg, &item.node_id, &made.node_id)
                        {
                            report.lines.push(format!(
                                "! Story #{} was not linked to #{n}: {e}",
                                made.number
                            ));
                        }
                        place(&mut board_failures, &made, ItemType::Story);
                    }
                    for (j, task) in story.tasks.iter().enumerate() {
                        let task_title = format!("{} {}", ItemType::Task.tag(), task.title);
                        let task_body = format!("{}\n\n_Story pai: {}_", task.body, made.url);
                        let mut task_labels = vec![ItemType::Task.tag().to_string()];
                        task_labels.extend(repo_labels.iter().cloned());
                        let (t_made, t_fresh) = make(
                            &mut journal,
                            format!("s{i}t{j}"),
                            &task_title,
                            &task_body,
                            &task_labels,
                        )?;
                        doc.stories[i].tasks[j].issue = Some(t_made.number);
                        report.touched.push(t_made.number);
                        if t_fresh {
                            if let Err(e) =
                                write::add_sub_issue(tools, cfg, &made.node_id, &t_made.node_id)
                            {
                                report.lines.push(format!(
                                    "! Task #{} was not linked to Story #{}: {e}",
                                    t_made.number, made.number
                                ));
                            }
                            place(&mut board_failures, &t_made, ItemType::Task);
                        }
                    }
                    stories.push(made);
                }
            }
            Kind::Tasks => {
                for i in 0..doc.tasks.len() {
                    let task = doc.tasks[i].clone();
                    let title = format!("{} {}", ItemType::Task.tag(), task.title);
                    let body = if task.body.trim().is_empty() {
                        "_(sem descrição)_".to_string()
                    } else {
                        task.body.clone()
                    };
                    let (made, fresh) = make(
                        &mut journal,
                        format!("t{i}"),
                        &title,
                        &body,
                        &[ItemType::Task.tag().to_string()],
                    )?;
                    doc.tasks[i].issue = Some(made.number);
                    report.touched.push(made.number);
                    created_lines.push(format!("- {} — {title}", made.url));
                    if fresh {
                        if let Err(e) =
                            write::add_sub_issue(tools, cfg, &item.node_id, &made.node_id)
                        {
                            report.lines.push(format!(
                                "! Task #{} was not linked to #{n}: {e}",
                                made.number
                            ));
                        }
                        place(&mut board_failures, &made, ItemType::Task);
                    }
                }
            }
        }

        // the Feature itself moves to Ready, only forward
        if stage::plan_move(item.stage.as_deref(), STAGE_READY).advance {
            let moved = (|| -> Result<(), String> {
                let id = match &item.board_item {
                    Some(id) => id.clone(),
                    None => write::add_to_board(tools, cfg, &board, &item.node_id)?,
                };
                write::set_field(tools, cfg, &board, &id, FIELD_STAGE, STAGE_READY)?;
                Ok(())
            })();
            if let Err(e) = moved {
                board_failures.push(format!("#{n} itself: {e}"));
            }
        }

        // the draft, annotated with what exists now, and the dependency map
        let applied_at = iso_now();
        doc.applied_at = Some(applied_at.clone());
        let mut extra: Vec<(String, String)> = Vec::new();
        if let Some(map) = decomposition::dependency_map(&doc, n, &applied_at) {
            extra.push((
                path.replace("decomposition.md", "dependency-map.json"),
                decomposition::dependency_map_json(&map),
            ));
        }
        let annotated = decomposition::render(&doc);
        match deliver::publish(
            tools,
            &self.hub_root,
            &self.scratch,
            &Artifact {
                doc: "decomposition-apply",
                number: n,
                issue_title: &detail.title,
                issue_url: &detail.url,
                path,
                content: &annotated,
                next_step: None,
                extra: &extra,
            },
        ) {
            Ok(p) => report.lines.push(format!(
                "the draft was annotated with the issues on {}{}",
                p.branch,
                p.pr_url.map(|u| format!(": {u}")).unwrap_or_default()
            )),
            Err(e) => report.lines.push(format!(
                "! the issues exist, but `{path}` was not annotated ({e}): the file still describes the proposal"
            )),
        }

        if let Err(e) = write::add_label(tools, cfg, n, LABEL_DECOMPOSED) {
            report
                .lines
                .push(format!("! `{LABEL_DECOMPOSED}` was not added: {e}"));
        }
        if item.has_label(LABEL_DECOMPOSE_READY) {
            let _ = write::remove_label(tools, cfg, n, LABEL_DECOMPOSE_READY);
        }

        let what = if doc.kind == Kind::Stories {
            format!("{} stories e suas tasks", doc.stories.len())
        } else {
            format!("{} tasks", doc.tasks.len())
        };
        let placement = if board_failures.is_empty() {
            "Tudo posicionado em **✅ Ready**.".to_string()
        } else {
            format!(
                "⚠️ Estes itens foram criados mas ficaram sem Etapa no board — posicione-os à mão:\n{}",
                board_failures
                    .iter()
                    .map(|f| format!("- {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let milestone_line = match (&item.milestone, milestone) {
            (Some(title), Some(_)) => format!("🏁 Milestone herdado do pai: **{title}**.\n\n"),
            _ => String::new(),
        };
        let _ = write::comment(
            tools,
            cfg,
            n,
            &format!(
                "🔀 **Decomposição aplicada!**\n\nA partir de `{path}` foram criados {what}:\n\n{}\n\n{milestone_line}{placement}",
                created_lines.join("\n")
            ),
        );
        if doc.kind == Kind::Stories {
            let _ = write::comment(tools, cfg, n, &order_comment(&doc));
        }

        // it is whole: the journal has done its work
        let _ = std::fs::remove_file(&journal_file);
        report.lines.insert(
            usize::from(resumed),
            format!("decomposition applied: {what} created under #{n}"),
        );
        for f in &board_failures {
            report.lines.push(format!("! not placed on the board: {f}"));
        }
        Ok(report)
    }
}

/// The comment that says in which order the Stories are executed
/// (spec-wave's `renderOrderComment`).
pub fn order_comment(doc: &Doc) -> String {
    let Some(map) = decomposition::dependency_map(doc, 0, "") else {
        return String::new();
    };
    let line = |i: usize, number: u64| -> String {
        let s = map.stories.iter().find(|s| s.number == number);
        let deps = s
            .filter(|s| !s.depends_on.is_empty())
            .map(|s| {
                format!(
                    " — depende de {}",
                    s.depends_on
                        .iter()
                        .map(|d| format!("#{d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_default();
        format!(
            "{}. #{number} {}{deps}",
            i + 1,
            s.map_or("", |s| s.title.as_str())
        )
        .trim_end()
        .to_string()
    };
    let mut parts = vec![
        "📋 **Ordem de execução das Stories**".to_string(),
        map.order
            .iter()
            .enumerate()
            .map(|(i, n)| line(i, *n))
            .collect::<Vec<_>>()
            .join("\n"),
    ];
    if !map.cycle.is_empty() {
        parts.push(format!(
            "⚠️ **Ciclo de dependências:** {} ficaram FORA da ordem acima. Corrija as linhas \"Depende de\" (ou as relações blocked_by) dessas issues.",
            map.cycle.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", ")
        ));
    }
    parts.join("\n\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::factory::deliver::Tools;
    use crate::project::config::{self, ProjectConfig};
    use crate::project::deliver::tests::repo;
    use crate::project::next::tests::item;
    use crate::project::ops::tests::fake_gh;
    use std::path::Path;
    use std::process::Command;

    /// Arms for the stand-in `gh`: new issues get the numbers 101, 102, …
    /// (a file keeps the count), and the third creation fails while
    /// `fail-at-3` exists.
    const ARMS: &str = r#"  "api -X POST repos/acme/hub/issues/"*"/dependencies/blocked_by"*) echo '{}' ;;
  "api -X POST repos/acme/hub/issues"*)
    n=$(cat "$d/n" 2>/dev/null || echo 100); n=$((n+1))
    if [ -f "$d/fail-at-3" ] && [ "$n" = "103" ]; then echo 'gh: Server Error (HTTP 502)' >&2; exit 1; fi
    echo $n > "$d/n"
    echo "{\"number\":$n,\"node_id\":\"I_$n\",\"id\":$((n+9000)),\"html_url\":\"https://github.com/acme/hub/issues/$n\"}" ;;
  "api repos/acme/hub/issues/"*) echo 777 ;;
  "pr create"*) echo 'https://github.com/acme/hub/pull/9' ;;"#;

    const CONFIG: &str = r#"{"owner":"acme","repo":"hub",
      "repositories":[{"slug":"api","owner":"acme","repo":"api"}],
      "project":{"id":"PVT_x","fields":{
        "Etapa":{"id":"F_STAGE","options":{"📋 Plan":"o_plan","✅ Ready":"o_ready"}},
        "Work Item Type":{"id":"F_TYPE","options":{"Story":"t_story","Task":"t_task"}}}}}"#;

    const DRAFT: &str = "# Decomposição — [FEATURE] Pedidos\n<!-- spec-wave:decomposition v1 issue=1 kind=stories -->\n\n## Story 1 — ver pedidos\n\n**User story:** Como cliente, quero ver pedidos\n**Depende de:** —\n**Repos:** api\n\nContexto.\n\n### Task 1.1 — criar endpoint\n\nGET /pedidos\n\n## Story 2 — cancelar pedido\n\n**User story:** Como cliente, quero cancelar\n**Depende de:** Story 1, #50\n\n### Task 2.1 — endpoint de cancelamento\n\nDELETE /pedidos\n";

    struct Fx {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        hub: std::path::PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn fx(draft: &str) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = repo(&root);
        let file = hub.join("docs/features/pedidos/decomposition.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, draft).unwrap();
        git(&hub, &["add", "-A"]);
        git(&hub, &["commit", "-q", "-m", "draft merged"]);
        git(&hub, &["push", "-q", "origin", "HEAD:main"]);
        let tools = fake_gh(&root, "{}", ARMS);
        let cfg = config::parse(CONFIG, hub.join(".spec-wave.json")).unwrap();
        Fx {
            _dir: dir,
            root,
            hub,
            tools,
            cfg,
        }
    }

    impl Fx {
        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                tools: &self.tools,
                cfg: &self.cfg,
                hub_root: self.hub.clone(),
                scratch: self.root.join("scratch"),
            }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn created(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|c| c.starts_with("api -X POST repos/acme/hub/issues -f"))
                .collect()
        }
    }

    fn feature() -> Item {
        let mut it = item(
            1,
            "[FEATURE] Pedidos",
            &["[FEATURE]", LABEL_PLAN_APPROVED, LABEL_DECOMPOSE_READY],
        );
        it.stage = Some(STAGE_PLAN.into());
        it.board_item = Some("PVTI_1".into());
        it.milestone = Some("v1".into());
        it
    }

    fn snap(items: Vec<Item>) -> Snapshot {
        Snapshot {
            hub: "acme/hub".into(),
            items,
            milestones: vec![crate::project::Milestone {
                number: 4,
                title: "v1".into(),
                open: true,
                due: None,
                open_issues: 0,
                closed_issues: 0,
            }],
            ..Default::default()
        }
    }

    fn detail() -> Detail {
        Detail {
            title: "[FEATURE] Pedidos".into(),
            body: String::new(),
            url: "https://github.com/acme/hub/issues/1".into(),
            comments: vec![],
        }
    }

    #[test]
    fn the_preview_says_what_would_be_created() {
        let doc = decomposition::parse(DRAFT).unwrap();
        let p = preview(&doc);
        assert_eq!(p[0], "2 Stories and 2 Tasks, each in ✅ Ready:");
        assert_eq!(p[1], "  Story 1 — ver pedidos (1 tasks, in api)");
        assert_eq!(
            p[2],
            "  Story 2 — cancelar pedido (1 tasks, after Story 1, #50)"
        );
    }

    #[test]
    fn the_reviewed_draft_becomes_issues_in_spec_waves_shape() {
        let f = fx(DRAFT);
        let it = feature();
        let s = snap(vec![it.clone()]);
        let (doc, path) = f.ctx().apply_plan(&it, &s).unwrap();
        assert_eq!(path, "docs/features/pedidos/decomposition.md");
        let r = f.ctx().apply(&it, &detail(), &s, doc, &path).unwrap();
        assert_eq!(
            r.lines[0],
            "decomposition applied: 2 stories e suas tasks created under #1"
        );
        assert_eq!(r.touched, vec![1, 101, 102, 103, 104]);

        let made = f.created();
        assert_eq!(made.len(), 4, "{made:?}");
        // Story 1: its user story, body, repo label and the Feature's milestone
        assert!(made[0].contains("title=[STORY] ver pedidos"), "{}", made[0]);
        assert!(
            made[0].contains("body=Como cliente, quero ver pedidos  Contexto."),
            "{}",
            made[0]
        );
        assert!(made[0].contains("labels[]=[STORY]") && made[0].contains("labels[]=repo:api"));
        assert!(made[0].contains("milestone=4"));
        // its Task, under it, with the same repo label
        assert!(made[1].contains("title=[TASK] criar endpoint"));
        assert!(made[1].contains("_Story pai: https://github.com/acme/hub/issues/101_"));
        assert!(made[1].contains("labels[]=repo:api"));
        // Story 2 depends on Story 1's issue and on #50
        assert!(made[2].contains("Depende de: #101, #50"), "{}", made[2]);
        assert!(!made[2].contains("repo:"));

        let calls = f.calls();
        // blocked-by takes database ids: Story 1's, then #50's
        let blocked: Vec<&String> = calls
            .iter()
            .filter(|c| c.contains("/dependencies/blocked_by"))
            .collect();
        assert_eq!(blocked.len(), 2);
        assert!(
            blocked[0].contains("issues/103/dependencies/blocked_by -F issue_id=9101"),
            "{}",
            blocked[0]
        );
        assert!(blocked[1].contains("issue_id=777"));
        // parents: the Feature for a Story, the Story for its Task
        let links: Vec<&String> = calls.iter().filter(|c| c.contains("addSubIssue")).collect();
        assert_eq!(links.len(), 4);
        assert!(links[0].contains("issueId=I_1") && links[0].contains("subIssueId=I_101"));
        assert!(links[1].contains("issueId=I_101") && links[1].contains("subIssueId=I_102"));
        // every new issue and the Feature are put in Ready
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains("optionId=o_ready"))
                .count(),
            5
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains("optionId=t_story"))
                .count(),
            2
        );
        // the labels, the two comments
        assert!(
            calls
                .iter()
                .any(|c| c == "issue edit 1 -R acme/hub --add-label spec-wave:decomposed ")
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "issue edit 1 -R acme/hub --remove-label spec-wave:decompose-ready ")
        );
        let applied = calls
            .iter()
            .find(|c| c.contains("🔀 **Decomposição aplicada!**"))
            .unwrap();
        assert!(
            applied.contains("foram criados 2 stories e suas tasks")
                && applied.contains("🏁 Milestone herdado do pai: **v1**.")
        );
        assert!(applied.contains("Tudo posicionado em **✅ Ready**."));
        let order = calls
            .iter()
            .find(|c| c.contains("📋 **Ordem de execução das Stories**"))
            .unwrap();
        assert!(
            order.contains("1. #101 ver pedidos 2. #103 cancelar pedido — depende de #50, #101"),
            "{order}"
        );

        // the draft is annotated on its own branch, beside the dependency map
        let origin = f.root.join("origin.git");
        let annotated = git(
            &origin,
            &[
                "show",
                "spec-wave/1-decompose-apply:docs/features/pedidos/decomposition.md",
            ],
        );
        assert!(annotated.contains("kind=stories applied=20"), "{annotated}");
        assert!(annotated.contains("**Repos:** api\n**Issue:** #101"));
        assert!(annotated.contains("### Task 2.1 — endpoint de cancelamento\n\n**Issue:** #104"));
        let map = git(
            &origin,
            &[
                "show",
                "spec-wave/1-decompose-apply:docs/features/pedidos/dependency-map.json",
            ],
        );
        assert!(
            map.contains("\"feature\": 1") && map.contains("\"order\": [\n    101,\n    103\n  ]"),
            "{map}"
        );
        assert_eq!(
            git(
                &origin,
                &["log", "-1", "--format=%s", "spec-wave/1-decompose-apply"]
            ),
            "docs: registra as issues criadas em docs/features/pedidos/decomposition.md (issue 1) [spec-wave]"
        );
        // the journal is gone: the apply is whole
        assert!(!f.ctx().journal_path(1).exists());
    }

    #[test]
    fn an_apply_that_stopped_resumes_without_a_duplicate() {
        let f = fx(DRAFT);
        let it = feature();
        let s = snap(vec![it.clone()]);
        std::fs::write(f.root.join("fail-at-3"), "").unwrap();
        let (doc, path) = f.ctx().apply_plan(&it, &s).unwrap();
        let e = f.ctx().apply(&it, &detail(), &s, doc, &path).unwrap_err();
        assert!(e.contains("502"), "{e}");
        assert_eq!(f.created().len(), 3, "two were made, the third failed");
        assert!(f.ctx().journal_path(1).is_file());
        assert!(!f.calls().iter().any(|c| c.contains("spec-wave:decomposed")));

        // the Story it made is on GitHub now: the journal says it is its own
        std::fs::remove_file(f.root.join("fail-at-3")).unwrap();
        let mut story = item(101, "[STORY] ver pedidos", &["[STORY]"]);
        story.parent = Some(1);
        let s = snap(vec![it.clone(), story]);
        let (doc, path) = f.ctx().apply_plan(&it, &s).unwrap();
        let r = f.ctx().apply(&it, &detail(), &s, doc, &path).unwrap();
        assert!(r.lines[0].contains("resuming"), "{r:?}");
        // five creation calls in all: 3 before (one failed) and only the 2 missing now
        let made = f.created();
        assert_eq!(made.len(), 5, "{made:?}");
        assert_eq!(
            made.iter()
                .filter(|c| c.contains("title=[STORY] ver pedidos"))
                .count(),
            1
        );
        assert_eq!(
            made.iter()
                .filter(|c| c.contains("title=[TASK] criar endpoint"))
                .count(),
            1
        );
        // the second Story still depends on the first one's number
        assert!(made[3].contains("Depende de: #101, #50"), "{}", made[3]);
        assert_eq!(r.touched, vec![1, 101, 102, 103, 104]);
    }

    #[test]
    fn what_stops_an_apply_before_any_issue() {
        let it = feature();
        // applied already, by its label or by the file's own marker
        let f = fx(DRAFT);
        let mut done = it.clone();
        done.labels.push(LABEL_DECOMPOSED.to_string());
        assert!(
            f.ctx()
                .apply_plan(&done, &snap(vec![done.clone()]))
                .unwrap_err()
                .contains("was applied")
        );
        let f = fx(&DRAFT.replace(
            "kind=stories",
            "kind=stories applied=2026-01-01T00:00:00.000Z",
        ));
        assert!(
            f.ctx()
                .apply_plan(&it, &snap(vec![it.clone()]))
                .unwrap_err()
                .contains("its issues exist")
        );
        // Stories that exist with no journal: someone else's apply
        let f = fx(DRAFT);
        let mut story = item(60, "[STORY] antiga", &["[STORY]"]);
        story.parent = Some(1);
        let e = f
            .ctx()
            .apply_plan(&it, &snap(vec![it.clone(), story]))
            .unwrap_err();
        assert!(e.contains("would duplicate") && e.contains("#60"), "{e}");
        // a draft a person broke is an error with its reason, never a guess
        let f = fx(&DRAFT.replace("**Depende de:** Story 1, #50", "**Depende de:** Story 2"));
        assert!(
            f.ctx()
                .apply_plan(&it, &snap(vec![it.clone()]))
                .unwrap_err()
                .contains("só apontam para trás")
        );
        let f = fx(&DRAFT.replace("**Repos:** api", "**Repos:** mobile"));
        assert!(
            f.ctx()
                .apply_plan(&it, &snap(vec![it.clone()]))
                .unwrap_err()
                .contains("`mobile`")
        );
        // not merged: nothing to apply
        let f = fx(DRAFT);
        let other = item(2, "[FEATURE] Outra", &["[FEATURE]"]);
        assert!(
            f.ctx()
                .apply_plan(&other, &snap(vec![other.clone()]))
                .unwrap_err()
                .contains("not on the default branch")
        );
        assert!(f.calls().is_empty());
        assert!(
            f.ctx()
                .apply_plan(&item(3, "[STORY] s", &[]), &snap(vec![]))
                .is_err()
        );
    }

    #[test]
    fn the_order_comment_is_spec_waves() {
        let mut doc = decomposition::parse(DRAFT).unwrap();
        doc.stories[0].issue = Some(10);
        doc.stories[0].title = "A".into();
        doc.stories[1].issue = Some(11);
        doc.stories[1].title = "B".into();
        doc.stories[1].depends_on_issues.clear();
        assert_eq!(
            order_comment(&doc),
            "📋 **Ordem de execução das Stories**\n\n1. #10 A\n2. #11 B — depende de #10"
        );
    }
}
