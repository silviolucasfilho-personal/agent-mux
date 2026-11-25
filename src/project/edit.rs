//! `agent-mux project edit <n>` and `e` on the Project screens: the
//! operator changes an issue's title, priority, milestone or description.
//!
//! spec-wave's CLI has no such command (an issue is edited on GitHub's
//! site), so the rules here are agent-mux's own, chosen so that what
//! spec-wave reads stays true:
//!
//! - **The title keeps its `[TYPE]` tag.** The tag is the item's type for
//!   both tools; a tag typed in the new title is dropped and the issue's
//!   own stays.
//! - **The priority is one label and the board's `Priority` field**, as
//!   `new_item` writes them: the other priority labels are removed, never
//!   added to. The board is best effort, as in every verb.
//! - **The milestone must exist.** It is named by number or by title and
//!   read from GitHub when the verb runs.
//! - **The description is replaced whole**, and never when it changed on
//!   GitHub after it was read (`body_base`). What tools read in it is
//!   shown as a warning when it changes: the `Depende de:` lines, which
//!   decide the order of a Feature's Stories, and HTML comments.
//!
//! [`Ops::edit_plan`] only reads, and says what would change; a field
//! that already has the value asked for is not a change.
//! [`Ops::edit_write`] writes a plan.

use super::assist::hash;
use super::contract::{FIELD_PRIORITY, PRIORITIES, parse_dependencies, strip_tag};
use super::ops::{Ops, Report};
use super::{Item, Milestone, github, snapshot, write};

/// The word that empties a priority or a milestone.
pub const NONE: &str = "none";

/// What the operator asked for. A field left `None` is not touched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Edit {
    pub title: Option<String>,
    /// `Some(None)` removes the priority.
    pub priority: Option<Option<String>>,
    /// A milestone's number or title; `Some(None)` takes the item out.
    pub milestone: Option<Option<String>>,
    pub body: Option<String>,
    /// The hash of the description the new one was written from. With it,
    /// a description that changed on GitHub since is never replaced.
    pub body_base: Option<String>,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.priority.is_none()
            && self.milestone.is_none()
            && self.body.is_none()
    }

    /// `none` (any case) or nothing typed empties; anything else is a value.
    pub fn value(raw: &str) -> Option<String> {
        let t = raw.trim();
        (!t.is_empty() && !t.eq_ignore_ascii_case(NONE)).then(|| t.to_string())
    }
}

/// A priority change, resolved against the issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityChange {
    pub to: Option<String>,
    /// Priority labels the issue has that must go.
    pub remove: Vec<String>,
    /// Whether the label of `to` is missing.
    pub add: bool,
    /// Whether the board's field differs from `to`.
    pub board: bool,
}

/// What an edit changes, resolved against GitHub as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPlan {
    pub number: u64,
    /// The issue's place on the board, when it is there.
    pub board_item: Option<String>,
    /// The whole new title, tag included.
    pub title: Option<String>,
    pub priority: Option<PriorityChange>,
    /// The milestone's number and title; `Some(None)` takes the item out.
    pub milestone: Option<Option<(u64, String)>>,
    pub body: Option<String>,
    /// What would change, for a reader.
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

impl EditPlan {
    /// Nothing differs from what GitHub has.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.priority.is_none()
            && self.milestone.is_none()
            && self.body.is_none()
    }

    /// The plan as text: the changes, then the warnings.
    pub fn text(&self) -> Vec<String> {
        let mut out = self.lines.clone();
        out.extend(self.warnings.iter().map(|w| format!("! {w}")));
        out
    }
}

/// The `[TYPE]` tag a title starts with, when it has one.
fn tag_of(title: &str) -> Option<&str> {
    let t = title.trim_start();
    (t.starts_with('[')).then(|| t.find(']').map(|end| &t[..=end]))?
}

/// The title an issue gets from what was typed: its own tag, then the
/// text without any tag.
pub fn new_title(current: &str, typed: &str) -> Result<String, String> {
    let name = strip_tag(typed.trim()).trim();
    if name.is_empty() {
        return Err("a title is needed".into());
    }
    Ok(match tag_of(current) {
        Some(tag) => format!("{tag} {name}"),
        None => name.to_string(),
    })
}

/// The milestone a reference names: its number (`3`, `#3`) or its title,
/// whatever the case.
pub fn resolve_milestone<'a>(
    milestones: &'a [Milestone],
    reference: &str,
) -> Result<&'a Milestone, String> {
    let text = reference.trim();
    let have = || {
        if milestones.is_empty() {
            "the repository has none".to_string()
        } else {
            format!(
                "there are: {}",
                milestones
                    .iter()
                    .map(|m| format!("#{} \"{}\"", m.number, m.title))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    };
    let by_title: Vec<&Milestone> = milestones
        .iter()
        .filter(|m| m.title.to_lowercase() == text.to_lowercase())
        .collect();
    match by_title.as_slice() {
        [one] => return Ok(one),
        [] => {}
        many => {
            return Err(format!(
                "\"{text}\" names {} milestones: give its number ({})",
                many.len(),
                many.iter()
                    .map(|m| format!("#{}", m.number))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let digits = text.strip_prefix('#').unwrap_or(text);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        let n: u64 = digits.parse().unwrap_or(0);
        if let Some(m) = milestones.iter().find(|m| m.number == n) {
            return Ok(m);
        }
    }
    Err(format!("no milestone is \"{text}\": {}", have()))
}

/// What a new description changes that tools read.
pub fn body_warnings(old: &str, new: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (was, is) = (parse_dependencies(old), parse_dependencies(new));
    if was != is {
        let list = |n: &[u64]| {
            if n.is_empty() {
                "none".to_string()
            } else {
                n.iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        out.push(format!(
            "the `Depende de:` lines change ({} becomes {}): they decide the order the Stories are built in",
            list(&was),
            list(&is)
        ));
    }
    let comments = |t: &str| t.matches("<!--").count();
    if comments(new) < comments(old) {
        out.push(
            "the new description drops an HTML comment (`<!-- … -->`), which tools read".into(),
        );
    }
    out
}

fn show(v: Option<&str>) -> String {
    v.map_or_else(|| "none".to_string(), str::to_string)
}

impl<'a> Ops<'a> {
    /// Reads the issue (and, when asked about them, the milestones and
    /// the description) and says what the edit would change. Nothing is
    /// written.
    pub fn edit_plan(&self, number: u64, edit: &Edit) -> Result<EditPlan, String> {
        if edit.is_empty() {
            return Err(
                "nothing to change: give --title, --priority, --milestone or --body-file".into(),
            );
        }
        if let Some(Some(p)) = &edit.priority
            && !PRIORITIES.contains(&p.as_str())
        {
            return Err(format!(
                "\"{p}\" is not a priority; one of: {}, or {NONE}",
                PRIORITIES.join(", ")
            ));
        }
        if let Some(b) = &edit.body
            && b.trim().is_empty()
        {
            return Err(
                "the new description is empty: nothing was written (an issue is emptied on GitHub)"
                    .into(),
            );
        }
        let item: Item = self.read(number)?;
        let mut plan = EditPlan {
            number,
            board_item: item.board_item.clone(),
            title: None,
            priority: None,
            milestone: None,
            body: None,
            lines: vec![format!("#{number} {}", item.title)],
            warnings: Vec::new(),
        };

        if let Some(typed) = &edit.title {
            let title = new_title(&item.title, typed)?;
            if title != item.title {
                plan.lines
                    .push(format!("  title        «{}» → «{title}»", item.title));
                if tag_of(typed.trim()).is_some_and(|t| Some(t) != tag_of(&item.title)) {
                    plan.warnings.push(
                        "the tag typed in the title was dropped: an edit never changes an item's type"
                            .into(),
                    );
                }
                plan.title = Some(title);
            }
        }

        if let Some(to) = &edit.priority {
            let have: Vec<String> = item
                .labels
                .iter()
                .filter(|l| PRIORITIES.contains(&l.as_str()))
                .cloned()
                .collect();
            let remove: Vec<String> = have
                .iter()
                .filter(|l| Some(*l) != to.as_ref())
                .cloned()
                .collect();
            let add = to.as_ref().is_some_and(|p| !have.contains(p));
            let on_board = item.fields.get(FIELD_PRIORITY);
            let board = item.board_item.is_some() && on_board != to.as_ref();
            if !remove.is_empty() || add || board {
                let from = if have.is_empty() {
                    show(on_board.map(String::as_str))
                } else {
                    have.join(", ")
                };
                let mut line = format!("  priority     {from} → {}", show(to.as_deref()));
                if item.board_item.is_none() {
                    line.push_str(" (the label only: the issue is not on the board, or the board was not read)");
                } else if remove.is_empty() && !add {
                    line.push_str(" (the board's field only: the label is right)");
                }
                plan.lines.push(line);
                plan.priority = Some(PriorityChange {
                    to: to.clone(),
                    remove,
                    add,
                    board,
                });
            }
        }

        if let Some(to) = &edit.milestone {
            let target = match to {
                None => None,
                Some(reference) => {
                    let raw = github::milestones(
                        self.tools,
                        self.cfg.root(),
                        &self.cfg.hub_repo().slug(),
                    )?;
                    let all: Vec<Milestone> =
                        raw.iter().filter_map(snapshot::milestone_of).collect();
                    let m = resolve_milestone(&all, reference)?;
                    if !m.open {
                        plan.warnings
                            .push(format!("the milestone \"{}\" is closed", m.title));
                    }
                    Some((m.number, m.title.clone()))
                }
            };
            if target.as_ref().map(|(_, t)| t) != item.milestone.as_ref() {
                plan.lines.push(format!(
                    "  milestone    {} → {}",
                    show(item.milestone.as_deref()),
                    show(target.as_ref().map(|(_, t)| t.as_str()))
                ));
                plan.milestone = Some(target);
            }
        }

        if let Some(body) = &edit.body {
            // what is on GitHub now, never the cache
            let now = github::issue_detail(self.tools, self.cfg, number)?;
            if let Some(base) = &edit.body_base
                && &hash(&now.body) != base
            {
                return Err(format!(
                    "the description of #{number} changed on GitHub since it was read: nothing was written. Edit it again"
                ));
            }
            if now.body.trim() != body.trim() {
                plan.lines.push(format!(
                    "  description  replaced ({} → {} characters)",
                    now.body.trim().chars().count(),
                    body.trim().chars().count()
                ));
                plan.warnings.extend(body_warnings(&now.body, body));
                plan.body = Some(body.clone());
            }
        }

        if plan.is_empty() {
            plan.lines
                .push("  it already has all of that: nothing to change".into());
        }
        Ok(plan)
    }

    /// Writes a plan: the title and the milestone in one call, then the
    /// description, then the priority's labels and the board's field. An
    /// error says what was written before it.
    pub fn edit_write(&self, plan: &EditPlan) -> Result<Report, String> {
        let mut r = Report::default();
        if plan.is_empty() {
            r.say(format!("#{}: nothing to change", plan.number));
            return Ok(r);
        }
        let (tools, cfg, n) = (self.tools, self.cfg, plan.number);
        r.touch(n);
        let after = |r: &Report, e: String| -> String {
            if r.lines.is_empty() {
                format!("{e}: nothing was written")
            } else {
                format!("{e}. Written before it: {}", r.lines.join("; "))
            }
        };

        if plan.title.is_some() || plan.milestone.is_some() {
            write::patch_issue(
                tools,
                cfg,
                n,
                plan.title.as_deref(),
                plan.milestone
                    .as_ref()
                    .map(|m| m.as_ref().map(|(number, _)| *number)),
            )
            .map_err(|e| after(&r, e))?;
            if let Some(t) = &plan.title {
                r.say(format!("#{n}: title → «{t}»"));
            }
            if let Some(m) = &plan.milestone {
                r.say(format!(
                    "#{n}: milestone → {}",
                    show(m.as_ref().map(|(_, t)| t.as_str()))
                ));
            }
        }

        if let Some(body) = &plan.body {
            let file = std::env::temp_dir().join(format!(
                "agent-mux-edit-{}-{n}-{}.md",
                std::process::id(),
                super::now_ns()
            ));
            std::fs::write(&file, body)
                .map_err(|e| after(&r, format!("{}: {e}", file.display())))?;
            let wrote = write::set_body(tools, cfg, n, &file);
            let _ = std::fs::remove_file(&file);
            wrote.map_err(|e| after(&r, e))?;
            r.say(format!("#{n}: the description was replaced"));
        }

        if let Some(p) = &plan.priority {
            if p.add
                && let Some(to) = &p.to
            {
                write::add_label(tools, cfg, n, to).map_err(|e| after(&r, e))?;
            }
            // the new label is on before an old one goes: never without one
            for old in &p.remove {
                write::remove_label(tools, cfg, n, old).map_err(|e| after(&r, e))?;
            }
            if p.add || !p.remove.is_empty() {
                r.say(format!("#{n}: priority → {}", show(p.to.as_deref())));
            }
            if p.board
                && let Some(id) = &plan.board_item
            {
                let done = self.board().and_then(|b| match &p.to {
                    Some(to) => write::set_field(tools, cfg, &b, id, FIELD_PRIORITY, to),
                    None => write::clear_field(tools, cfg, &b, id, FIELD_PRIORITY),
                });
                match done {
                    Ok(true) => r.say(format!(
                        "#{n}: the board's {FIELD_PRIORITY} → {}",
                        show(p.to.as_deref())
                    )),
                    Ok(false) => r.say(format!(
                        "! the board has no `{FIELD_PRIORITY}` field or option for {}: only the label was changed",
                        show(p.to.as_deref())
                    )),
                    Err(e) => r.say(format!(
                        "! the board's {FIELD_PRIORITY} was not changed: {e}"
                    )),
                }
            }
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::deliver::Tools;
    use crate::project::config::{self, ProjectConfig};
    use crate::project::ops::tests::{CONFIG, fake_gh, node, nodes};
    use std::path::PathBuf;

    const MILESTONES: &str = r#"[{"number":1,"title":"v1","state":"open"},
        {"number":2,"title":"v2","state":"open"},{"number":3,"title":"v0","state":"closed"}]"#;

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
    }

    /// One issue #5, with a body of its own, and three milestones.
    fn fx(issue: String, body: &str) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("milestones.json"), MILESTONES).unwrap();
        std::fs::write(
            root.join("detail.json"),
            serde_json::json!({"title": "t", "body": body, "url": "u", "comments": []}).to_string(),
        )
        .unwrap();
        let arms = format!(
            r#"  "api repos/acme/hub/milestones"*) cat "{d}/milestones.json" ;;
  "issue view 5 "*) cat "{d}/detail.json" ;;
  *"clearProjectV2ItemFieldValue"*) echo '{{"data":{{"clearProjectV2ItemFieldValue":{{"projectV2Item":{{"id":"x"}}}}}}}}' ;;
  "issue edit 5 -R acme/hub --body-file"*) eval "last=\${{$#}}"; cp "$last" "{d}/body-written" ;;"#,
            d = root.display()
        );
        let tools = fake_gh(&root, &nodes(&[issue]), &arms);
        let cfg = config::parse(CONFIG, root.join(".spec-wave.json")).unwrap();
        Fx {
            _dir: dir,
            root,
            tools,
            cfg,
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

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        }

        /// The calls that change something.
        fn writes(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|l| {
                    !l.contains("issue(number:")
                        && !l.starts_with("api repos/acme/hub/milestones")
                        && !l.starts_with("issue view")
                })
                .collect()
        }
    }

    fn feature() -> String {
        node(
            5,
            "[FEATURE] Pedidos",
            &["[FEATURE]", "P1"],
            Some("📋 Spec"),
            r#",{"name":"P1","field":{"name":"Priority"}}"#,
        )
    }

    #[test]
    fn a_title_keeps_the_issues_own_tag() {
        assert_eq!(
            new_title("[FEATURE] Pedidos", " Cadastro de Pedidos ").unwrap(),
            "[FEATURE] Cadastro de Pedidos"
        );
        assert_eq!(
            new_title("[FEATURE] Pedidos", "[BUG] Cadastro").unwrap(),
            "[FEATURE] Cadastro"
        );
        assert_eq!(new_title("sem tag", "outro").unwrap(), "outro");
        assert!(new_title("[STORY] x", "  ").is_err());
        assert!(new_title("[STORY] x", "[STORY]").is_err());
    }

    #[test]
    fn a_milestone_is_named_by_title_or_number() {
        let all: Vec<Milestone> = serde_json::from_str::<Vec<serde_json::Value>>(MILESTONES)
            .unwrap()
            .iter()
            .filter_map(snapshot::milestone_of)
            .collect();
        assert_eq!(resolve_milestone(&all, "V2").unwrap().number, 2);
        assert_eq!(resolve_milestone(&all, "#1").unwrap().title, "v1");
        assert_eq!(resolve_milestone(&all, "3").unwrap().title, "v0");
        let e = resolve_milestone(&all, "v9").unwrap_err();
        assert!(
            e.contains("no milestone is \"v9\"") && e.contains("#2 \"v2\""),
            "{e}"
        );
        assert!(
            resolve_milestone(&[], "v1")
                .unwrap_err()
                .contains("the repository has none")
        );
        // a title made of digits wins over the number
        let mut numeric = all.clone();
        numeric[1].title = "1".into();
        assert_eq!(resolve_milestone(&numeric, "1").unwrap().number, 2);
        numeric[0].title = "1".into();
        assert!(
            resolve_milestone(&numeric, "1")
                .unwrap_err()
                .contains("names 2 milestones")
        );
    }

    #[test]
    fn what_tools_read_in_a_description_is_a_warning_when_it_changes() {
        let old = "Texto\n\nDepende de: #2\n<!-- spec-wave:x -->";
        assert!(
            body_warnings(old, "Outro texto\n\nDepende de: #2\n<!-- spec-wave:x -->").is_empty()
        );
        let w = body_warnings(old, "Outro texto");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("#2 becomes none"), "{w:?}");
        assert!(w[1].contains("HTML comment"), "{w:?}");
    }

    #[test]
    fn the_plan_reads_and_writes_nothing_and_says_what_changes() {
        let f = fx(feature(), "Texto antigo");
        let plan = f
            .ops()
            .edit_plan(
                5,
                &Edit {
                    title: Some("Cadastro de Pedidos".into()),
                    priority: Some(Some("P0".into())),
                    milestone: Some(Some("v2".into())),
                    body: Some("Texto novo\n".into()),
                    body_base: None,
                },
            )
            .unwrap();
        assert!(f.writes().is_empty(), "{:?}", f.writes());
        assert_eq!(
            plan.text(),
            vec![
                "#5 [FEATURE] Pedidos",
                "  title        «[FEATURE] Pedidos» → «[FEATURE] Cadastro de Pedidos»",
                "  priority     P1 → P0",
                "  milestone    none → v2",
                "  description  replaced (12 → 10 characters)",
            ]
        );
        assert_eq!(plan.milestone, Some(Some((2, "v2".into()))));
        assert_eq!(
            plan.priority,
            Some(PriorityChange {
                to: Some("P0".into()),
                remove: vec!["P1".into()],
                add: true,
                board: true,
            })
        );
    }

    #[test]
    fn writing_makes_exactly_the_calls_of_the_plan() {
        let f = fx(
            node(
                5,
                "[FEATURE] Pedidos",
                &["[FEATURE]", "P2"],
                Some("📋 Spec"),
                r#",{"name":"P2","field":{"name":"Priority"}}"#,
            ),
            "Texto antigo",
        );
        let ops = f.ops();
        let plan = ops
            .edit_plan(
                5,
                &Edit {
                    title: Some("Cadastro de Pedidos".into()),
                    priority: Some(Some("P1".into())),
                    milestone: Some(Some("v2".into())),
                    body: Some("Texto novo\n".into()),
                    body_base: Some(hash("Texto antigo")),
                },
            )
            .unwrap();
        let r = ops.edit_write(&plan).unwrap();
        assert_eq!(r.touched, vec![5]);
        assert_eq!(
            r.lines,
            vec![
                "#5: title → «[FEATURE] Cadastro de Pedidos»",
                "#5: milestone → v2",
                "#5: the description was replaced",
                "#5: priority → P1",
                "#5: the board's Priority → P1",
            ]
        );
        let w = f.writes();
        assert_eq!(w.len(), 5, "{w:#?}");
        assert_eq!(
            w[0],
            "api -X PATCH repos/acme/hub/issues/5 -f title=[FEATURE] Cadastro de Pedidos -F milestone=2"
        );
        assert!(
            w[1].starts_with("issue edit 5 -R acme/hub --body-file "),
            "{w:#?}"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("body-written")).unwrap(),
            "Texto novo\n"
        );
        assert_eq!(w[2], "issue edit 5 -R acme/hub --add-label P1");
        assert_eq!(w[3], "issue edit 5 -R acme/hub --remove-label P2");
        assert!(
            w[4].contains("updateProjectV2ItemFieldValue")
                && w[4].contains("itemId=PVTI_5")
                && w[4].contains("fieldId=F_PRIO"),
            "{w:#?}"
        );
    }

    #[test]
    fn a_priority_the_board_has_no_option_for_changes_only_the_label() {
        let f = fx(feature(), "");
        let ops = f.ops();
        let plan = ops
            .edit_plan(
                5,
                &Edit {
                    priority: Some(Some("P0".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let r = ops.edit_write(&plan).unwrap();
        assert_eq!(r.lines[0], "#5: priority → P0");
        assert!(r.lines[1].starts_with("! the board has no `Priority` field or option for P0"));
        assert_eq!(f.writes().len(), 2, "{:?}", f.writes());
    }

    #[test]
    fn what_is_already_so_is_no_change_and_no_call() {
        let f = fx(feature(), "Texto antigo");
        let ops = f.ops();
        let plan = ops
            .edit_plan(
                5,
                &Edit {
                    title: Some("[FEATURE] Pedidos".into()),
                    priority: Some(Some("P1".into())),
                    milestone: Some(None),
                    body: Some("Texto antigo\n".into()),
                    body_base: None,
                },
            )
            .unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        assert!(plan.text()[1].contains("nothing to change"));
        let r = ops.edit_write(&plan).unwrap();
        assert!(r.touched.is_empty());
        assert!(f.writes().is_empty());
    }

    #[test]
    fn removing_a_priority_clears_the_label_and_the_boards_field() {
        let f = fx(feature(), "");
        let ops = f.ops();
        let plan = ops
            .edit_plan(
                5,
                &Edit {
                    priority: Some(None),
                    milestone: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(plan.text()[1], "  priority     P1 → none");
        assert_eq!(plan.milestone, None);
        ops.edit_write(&plan).unwrap();
        let w = f.writes();
        assert_eq!(w.len(), 2, "{w:#?}");
        assert_eq!(w[0], "issue edit 5 -R acme/hub --remove-label P1");
        assert!(
            w[1].contains("clearProjectV2ItemFieldValue") && w[1].contains("fieldId=F_PRIO"),
            "{w:#?}"
        );
    }

    #[test]
    fn an_issue_off_the_board_changes_only_its_label() {
        let f = fx(node(5, "[STORY] ver pedidos", &["[STORY]"], None, ""), "");
        let ops = f.ops();
        let plan = ops
            .edit_plan(
                5,
                &Edit {
                    priority: Some(Some("P2".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            plan.text()[1],
            "  priority     none → P2 (the label only: the issue is not on the board, or the board was not read)"
        );
        ops.edit_write(&plan).unwrap();
        assert_eq!(f.writes(), vec!["issue edit 5 -R acme/hub --add-label P2"]);
    }

    #[test]
    fn what_is_refused_is_refused_before_any_write() {
        let f = fx(feature(), "Mudou no GitHub");
        let ops = f.ops();
        let refused = |e: &Edit| ops.edit_plan(5, e).unwrap_err();
        assert!(refused(&Edit::default()).contains("nothing to change"));
        assert!(
            refused(&Edit {
                priority: Some(Some("P9".into())),
                ..Default::default()
            })
            .contains("is not a priority")
        );
        assert!(
            refused(&Edit {
                body: Some("  \n".into()),
                ..Default::default()
            })
            .contains("is empty")
        );
        assert!(
            refused(&Edit {
                milestone: Some(Some("v9".into())),
                ..Default::default()
            })
            .contains("no milestone is \"v9\"")
        );
        assert!(
            refused(&Edit {
                title: Some("[FEATURE]".into()),
                ..Default::default()
            })
            .contains("a title is needed")
        );
        // a description that changed since it was read
        assert!(
            refused(&Edit {
                body: Some("Texto novo".into()),
                body_base: Some(hash("Texto antigo")),
                ..Default::default()
            })
            .contains("changed on GitHub since it was read")
        );
        assert!(f.writes().is_empty(), "{:?}", f.writes());
    }

    #[test]
    fn a_closed_milestone_and_a_dropped_dependency_are_warnings() {
        let f = fx(feature(), "Texto\n\nDepende de: #2");
        let plan = f
            .ops()
            .edit_plan(
                5,
                &Edit {
                    title: Some("[BUG] Pedidos novos".into()),
                    milestone: Some(Some("#3".into())),
                    body: Some("Texto".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(plan.title.as_deref(), Some("[FEATURE] Pedidos novos"));
        let w = plan.warnings.join("\n");
        assert!(w.contains("never changes an item's type"), "{w}");
        assert!(w.contains("\"v0\" is closed"), "{w}");
        assert!(w.contains("`Depende de:` lines change"), "{w}");
    }
}
